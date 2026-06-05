//! iroh node server — serves profile/collections and accepts direct DMs.
//!
//! Protocol (`/hoardbook/node/1`):
//!   Client → Server  [u32-LE request-len] [JSON NodeRequest]
//!   Server → Client  [u32-LE response-len] [JSON NodeResponse]

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use hb_core::{ChatMessage, Collection, Profile, SignedEnvelope};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::{
    error::CmdResult,
    store::{DataStore, Settings},
};

pub const NODE_ALPN: &[u8] = b"/hoardbook/node/1";

/// Maximum number of direct DMs held in the in-memory queue before rejecting new ones.
const MAX_DM_QUEUE: usize = 500;

/// Cap on the framed get_profile response (profile + collection envelopes).
const MAX_PROFILE_RESPONSE_BYTES: u32 = 4 * 1024 * 1024;

pub type SharedDmQueue = Arc<Mutex<Vec<SignedEnvelope>>>;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NodeRequest {
    GetProfile,
    SendDm { envelope: SignedEnvelope },
}

#[derive(Serialize, Deserialize)]
struct GetProfileResponse {
    profile: Option<SignedEnvelope>,
    collections: Vec<SignedEnvelope>,
}

#[derive(Serialize)]
struct SendDmResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Inner logic (tested without iroh)
// ---------------------------------------------------------------------------

/// Load the signed profile and all signed collections from disk.
pub(crate) fn load_profile_data(store: &DataStore) -> (Option<SignedEnvelope>, Vec<SignedEnvelope>) {
    let profile = match store.load_profile_signed() {
        Ok(opt) => opt,
        Err(e) => { tracing::warn!("load_profile_signed error (serving empty): {e}"); None }
    };
    let collections = store.list_collections().unwrap_or_default();
    (profile, collections)
}

/// Verify an incoming DM envelope and check that the `to` field matches `own_hb_id`.
pub(crate) fn validate_dm(envelope: &SignedEnvelope, own_hb_id: &str) -> Result<(), String> {
    if envelope.doc_type != hb_core::DocType::Message {
        return Err(format!("expected doc_type=message, got {:?}", envelope.doc_type));
    }
    if let Err(e) = envelope.verify() {
        return Err(format!("invalid signature: {e}"));
    }
    let msg: ChatMessage = envelope
        .parse_payload()
        .map_err(|e| format!("invalid DM payload: {e}"))?;
    if msg.to != own_hb_id {
        return Err(format!(
            "recipient mismatch: message addressed to {}, but this node is {}",
            msg.to, own_hb_id,
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Client — fetch profile + collections over an iroh stream
// ---------------------------------------------------------------------------

/// Stream-level get_profile exchange: send the request, parse the response,
/// verify every envelope's signature and `public_key`, and decode payloads.
/// Invalid envelopes are silently dropped with a warning log.
pub(crate) async fn fetch_profile_via_stream(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    expected_hb_id: &str,
) -> Result<(Option<Profile>, Vec<Collection>)> {
    let req_bytes = serde_json::to_vec(&serde_json::json!({"type": "get_profile"}))
        .context("serialize get_profile request")?;
    send.write_u32_le(req_bytes.len() as u32).await.context("write req len")?;
    send.write_all(&req_bytes).await.context("write req")?;
    send.shutdown().await.context("shutdown send")?;

    let resp_len = recv.read_u32_le().await.context("read resp len")?;
    if resp_len > MAX_PROFILE_RESPONSE_BYTES {
        return Err(anyhow!("response too large: {resp_len} bytes"));
    }
    let mut resp_bytes = vec![0u8; resp_len as usize];
    recv.read_exact(&mut resp_bytes).await.context("read response")?;

    let resp: GetProfileResponse =
        serde_json::from_slice(&resp_bytes).context("parse get_profile response")?;

    let profile = resp.profile.and_then(|env| decode_envelope::<Profile>(env, expected_hb_id, "profile"));
    let collections = resp
        .collections
        .into_iter()
        .filter_map(|env| decode_envelope::<Collection>(env, expected_hb_id, "collection"))
        .collect();

    Ok((profile, collections))
}

fn decode_envelope<T: for<'de> serde::Deserialize<'de>>(
    env: SignedEnvelope,
    expected_hb_id: &str,
    kind: &str,
) -> Option<T> {
    if env.public_key != expected_hb_id {
        tracing::warn!(
            "{kind} envelope public_key {} does not match expected {expected_hb_id} — discarding",
            env.public_key
        );
        return None;
    }
    if let Err(e) = env.verify() {
        tracing::warn!("{kind} envelope signature invalid: {e} — discarding");
        return None;
    }
    match env.parse_payload::<T>() {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("{kind} envelope payload parse error: {e} — discarding");
            None
        }
    }
}

/// Connect to a remote iroh node and fetch their profile + collections.
///
/// `node_addr_str` is the JSON-serialised `iroh::EndpointAddr` stored by the relay
/// (produced by `serde_json::to_string(&endpoint.addr())`). Invalid or tampered
/// envelopes are silently discarded with a warning log. Returns `Err` only on
/// connection / IO failure.
pub async fn fetch_profile_via_iroh(
    endpoint: &iroh::Endpoint,
    node_addr_str: &str,
    expected_hb_id: &str,
) -> Result<(Option<Profile>, Vec<Collection>)> {
    let peer_addr: iroh::EndpointAddr =
        serde_json::from_str(node_addr_str).context("parse peer EndpointAddr")?;
    let conn = endpoint
        .connect(peer_addr, NODE_ALPN)
        .await
        .context("iroh connect")?;
    let (send, recv) = conn.open_bi().await.context("open_bi")?;
    fetch_profile_via_stream(send, recv, expected_hb_id).await
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

/// Protocol handler for a single node request/response exchange.
/// Extracted with generic stream bounds so it can be tested with in-process duplex streams.
pub(crate) async fn handle_node_stream(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    store: &DataStore,
    own_hb_id: &str,
    dm_queue: &SharedDmQueue,
) -> Result<()> {
    let req_len = recv.read_u32_le().await.context("read req len")?;
    // M13: cap at 64 KiB to match the transfer-request cap. The largest legitimate
    // node request is a SendDm envelope, well under this; the old 1 MiB cap allowed
    // a peer to force an oversized allocation per connection.
    if req_len > 64 * 1024 {
        return Err(anyhow!("request too large: {req_len} bytes"));
    }
    let mut req_bytes = vec![0u8; req_len as usize];
    recv.read_exact(&mut req_bytes).await.context("read request")?;

    let request: NodeRequest =
        serde_json::from_slice(&req_bytes).context("parse node request")?;

    let resp_bytes = match request {
        NodeRequest::GetProfile => {
            let (profile, collections) = load_profile_data(store);
            serde_json::to_vec(&GetProfileResponse { profile, collections })
                .context("serialize get_profile response")?
        }
        NodeRequest::SendDm { envelope } => {
            let resp = match validate_dm(&envelope, own_hb_id) {
                Ok(()) => {
                    let mut q = dm_queue.lock().await;
                    if q.len() >= MAX_DM_QUEUE {
                        SendDmResponse { ok: false, error: Some("inbox full".to_string()) }
                    } else {
                        q.push(envelope);
                        SendDmResponse { ok: true, error: None }
                    }
                }
                Err(reason) => SendDmResponse { ok: false, error: Some(reason) },
            };
            serde_json::to_vec(&resp).context("serialize send_dm response")?
        }
    };

    send.write_u32_le(resp_bytes.len() as u32)
        .await
        .context("write resp len")?;
    send.write_all(&resp_bytes).await.context("write resp")?;
    send.shutdown().await.context("shutdown send")?;
    Ok(())
}

pub async fn handle_node_connection(
    conn: iroh::endpoint::Connection,
    store: DataStore,
    own_hb_id: &str,
    dm_queue: SharedDmQueue,
    app: tauri::AppHandle,
) -> Result<()> {
    let len_before = dm_queue.lock().await.len();
    let (send, recv) = conn.accept_bi().await.context("accept_bi")?;
    handle_node_stream(send, recv, &store, own_hb_id, &dm_queue).await?;
    let len_after = dm_queue.lock().await.len();
    if len_after > len_before {
        let _ = app.emit("dm-received", len_after);
        if let Some(tray) = app.tray_by_id("hb_tray") {
            let tip = if len_after == 1 {
                "Hoardbook — 1 unread message".to_string()
            } else {
                format!("Hoardbook — {len_after} unread messages")
            };
            let _ = tray.set_tooltip(Some(&tip));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

/// Drain and return all DMs received directly over iroh (not via relay).
/// Respects the `allow_dms` setting: when off, only messages from contacts are returned.
#[tauri::command]
pub async fn fetch_direct_dm_inbox(
    dm_queue: State<'_, SharedDmQueue>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<SignedEnvelope>> {
    let mut guard = dm_queue.lock().await;
    let all = std::mem::take(&mut *guard);
    let allow_dms = store
        .load_settings()
        .ok()
        .flatten()
        .map(|s: Settings| s.allow_dms)
        .unwrap_or(true);
    if allow_dms {
        return Ok(all);
    }
    let contacts = store.list_contacts().unwrap_or_default();
    let contact_ids: std::collections::HashSet<String> =
        contacts.into_iter().map(|c| c.hb_id).collect();
    Ok(all.into_iter().filter(|env| contact_ids.contains(&env.public_key)).collect())
}

// ---------------------------------------------------------------------------
// Tests — T17 acceptance criteria
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hb_core::{DocType, HoardbookKeypair, SignedEnvelope};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;

    use super::*;
    use crate::store::DataStore;

    fn test_store() -> (TempDir, DataStore) {
        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn make_profile(store: &DataStore, kp: &HoardbookKeypair) {
        let prof = hb_core::Profile {
            display_name: "Alice".to_string(),
            bio: None, tags: vec![], since: None, est_size: None, languages: vec![],
            contact_hint: None, email: None, location: None, social_links: vec![],
            willing_to: vec![], content_types: vec![], updated: chrono::Utc::now(),
        };
        store.save_profile_draft(&prof).unwrap();
        let env = SignedEnvelope::create(kp, DocType::Profile, &prof).unwrap();
        store.save_profile_signed(&env).unwrap();
    }

    fn make_collection(store: &DataStore, kp: &HoardbookKeypair, slug: &str) {
        let col = hb_core::Collection {
            slug: slug.to_string(), path_alias: slug.to_string(), description: None,
            item_count: 0, est_size: None, content_types: vec!["video".to_string()],
            tags: vec![], languages: vec![], last_updated: chrono::Utc::now(), listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        let env = SignedEnvelope::create(kp, DocType::Collection, &col).unwrap();
        store.save_collection_signed(slug, &env).unwrap();
    }

    // ── Unit tests ────────────────────────────────────────────────────────────

    #[test]
    fn get_profile_returns_signed_files() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();

        // Empty store: both None and empty.
        let (profile, collections) = load_profile_data(&store);
        assert!(profile.is_none());
        assert!(collections.is_empty());

        // With profile + collection.
        make_profile(&store, &kp);
        make_collection(&store, &kp, "films");

        let (profile2, collections2) = load_profile_data(&store);
        assert!(profile2.is_some(), "signed profile must be returned");
        assert_eq!(collections2.len(), 1);
    }

    #[test]
    fn send_dm_validates_recipient() {
        let kp = HoardbookKeypair::generate();
        let own_hb_id = kp.hb_id();

        let msg = hb_core::ChatMessage {
            to: own_hb_id.clone(),
            content: "hello".to_string(),
            encrypted: false,
            sent_at: chrono::Utc::now(),
        };
        let envelope = SignedEnvelope::create(&kp, DocType::Message, &msg).unwrap();
        assert!(validate_dm(&envelope, &own_hb_id).is_ok());
    }

    #[test]
    fn send_dm_wrong_recipient_rejected() {
        let kp = HoardbookKeypair::generate();
        let other_kp = HoardbookKeypair::generate();

        let msg = hb_core::ChatMessage {
            to: other_kp.hb_id(), // addressed to someone else
            content: "hello".to_string(),
            encrypted: false,
            sent_at: chrono::Utc::now(),
        };
        let envelope = SignedEnvelope::create(&kp, DocType::Message, &msg).unwrap();

        let own_hb_id = kp.hb_id();
        let err = validate_dm(&envelope, &own_hb_id).unwrap_err();
        assert!(err.contains("recipient mismatch"), "got: {err}");
    }

    // ── Integration test ──────────────────────────────────────────────────────
    // Uses tokio::io::duplex() to exercise the full framing + dispatch logic
    // without real QUIC networking (which is unreliable in WSL2/CI).

    #[tokio::test]
    async fn iroh_client_connects_and_fetches_profile() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        let own_hb_id = kp.hb_id();
        make_profile(&store, &kp);

        let dm_queue: SharedDmQueue = Arc::new(Mutex::new(vec![]));

        // Duplex pair: server_side ↔ client_side (in-memory byte streams).
        let (server_side, client_side) = tokio::io::duplex(64 * 1024);
        let (client_recv, client_send) = tokio::io::split(client_side);
        let (server_recv, server_send) = tokio::io::split(server_side);

        // Run the node handler on the server side.
        let store_srv = store.clone();
        let hb_id_srv = own_hb_id.clone();
        let q_srv = dm_queue.clone();
        let server_task = tokio::spawn(async move {
            handle_node_stream(server_send, server_recv, &store_srv, &hb_id_srv, &q_srv)
                .await
                .unwrap();
        });

        // Client: send a get_profile request.
        let mut send = client_send;
        let mut recv = client_recv;

        let req = serde_json::json!({"type": "get_profile"});
        let req_bytes = serde_json::to_vec(&req).unwrap();
        send.write_u32_le(req_bytes.len() as u32).await.unwrap();
        send.write_all(&req_bytes).await.unwrap();
        send.shutdown().await.unwrap();

        let resp_len = recv.read_u32_le().await.unwrap();
        let mut resp_bytes = vec![0u8; resp_len as usize];
        recv.read_exact(&mut resp_bytes).await.unwrap();

        let resp: GetProfileResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(resp.profile.is_some(), "profile must be returned via iroh");

        server_task.await.unwrap();
    }

    // ── T20: client-side verify discards tampered envelopes ──────────────────

    /// Build the (server_send, server_recv, client_send, client_recv) tuple and
    /// drive `handle_node_stream` on the server side over an in-memory duplex.
    async fn spawn_server_with_store(
        store: DataStore,
        own_hb_id: String,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
    ) {
        let dm_queue: SharedDmQueue = Arc::new(Mutex::new(vec![]));
        let (server_side, client_side) = tokio::io::duplex(64 * 1024);
        let (client_recv, client_send) = tokio::io::split(client_side);
        let (server_recv, server_send) = tokio::io::split(server_side);

        let task = tokio::spawn(async move {
            // Tests only need a successful single exchange; ignore errors at teardown.
            let _ = handle_node_stream(server_send, server_recv, &store, &own_hb_id, &dm_queue).await;
        });
        (task, client_send, client_recv)
    }

    #[tokio::test]
    async fn tampered_envelope_discarded() {
        let (_dir, store) = test_store();
        let signer = HoardbookKeypair::generate();
        let impostor = HoardbookKeypair::generate();

        // Sign with `signer`, then advertise the impostor as the author.
        let prof = hb_core::Profile {
            display_name: "Mallory".to_string(),
            bio: None, tags: vec![], since: None, est_size: None, languages: vec![],
            contact_hint: None, email: None, location: None, social_links: vec![],
            willing_to: vec![], content_types: vec![], updated: chrono::Utc::now(),
        };
        let mut env = SignedEnvelope::create(&signer, DocType::Profile, &prof).unwrap();
        env.public_key = impostor.hb_id(); // tamper: header doesn't match signing key
        store.save_profile_signed(&env).unwrap();

        let (task, send, recv) =
            spawn_server_with_store(store, signer.hb_id()).await;

        let (profile, _collections) =
            fetch_profile_via_stream(send, recv, &impostor.hb_id())
                .await
                .expect("stream exchange completes");

        assert!(profile.is_none(), "tampered envelope must be silently discarded");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_signature_discarded() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();

        let prof = hb_core::Profile {
            display_name: "Carol".to_string(),
            bio: None, tags: vec![], since: None, est_size: None, languages: vec![],
            contact_hint: None, email: None, location: None, social_links: vec![],
            willing_to: vec![], content_types: vec![], updated: chrono::Utc::now(),
        };
        let mut env = SignedEnvelope::create(&kp, DocType::Profile, &prof).unwrap();
        // Mutate one hex character of the signature so verification fails.
        let mut sig = env.signature.clone();
        let first = sig.remove(0);
        let flipped = if first == '0' { '1' } else { '0' };
        sig.insert(0, flipped);
        env.signature = sig;
        store.save_profile_signed(&env).unwrap();

        let (task, send, recv) = spawn_server_with_store(store, kp.hb_id()).await;

        let (profile, _collections) =
            fetch_profile_via_stream(send, recv, &kp.hb_id())
                .await
                .expect("stream exchange completes");

        assert!(profile.is_none(), "envelope with bad signature must be discarded");
        task.await.unwrap();
    }
}
