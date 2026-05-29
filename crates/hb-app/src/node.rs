//! iroh node server — serves profile/collections and accepts direct DMs.
//!
//! Protocol (`/hoardbook/node/1`):
//!   Client → Server  [u32-LE request-len] [JSON NodeRequest]
//!   Server → Client  [u32-LE response-len] [JSON NodeResponse]

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use hb_core::{ChatMessage, SignedEnvelope};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::{
    commands::settings::Settings,
    error::CmdResult,
    store::DataStore,
};

pub const NODE_ALPN: &[u8] = b"/hoardbook/node/1";

/// Maximum number of direct DMs held in the in-memory queue before rejecting new ones.
const MAX_DM_QUEUE: usize = 500;

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
    if req_len > 1024 * 1024 {
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
) -> Result<()> {
    let (send, recv) = conn.accept_bi().await.context("accept_bi")?;
    handle_node_stream(send, recv, &store, own_hb_id, &dm_queue).await
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
}
