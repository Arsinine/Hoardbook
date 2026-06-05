use std::collections::HashSet;

use chrono::Utc;
use hb_core::{DocType, HbId, SignedEnvelope, types::ChatMessage};
use serde::Serialize;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    node,
    store::DataStore,
    SharedDmQueue, SharedEndpoint, SharedIdentity, SharedRelay,
};

/// L12: associated data that binds a message ciphertext to its routing and
/// timestamp. Built only from unencrypted fields, so the recipient can
/// reconstruct it before decrypting. Must be byte-identical on encrypt and decrypt.
fn message_aad(from: &str, to: &str, sent_at_rfc3339: &str) -> Vec<u8> {
    hb_core::jcs::canonicalize(&serde_json::json!({
        "from": from,
        "to": to,
        "sent_at": sent_at_rfc3339,
    }))
}

/// A decoded, sender-attributed chat message returned to the frontend.
/// Content is always plaintext — decryption happens here before returning.
#[derive(Debug, Clone, Serialize)]
pub struct ReceivedMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub sent_at: String, // ISO 8601
    pub encrypted: bool,
}

/// Encrypt and send a chat message to `to`.
/// Delivery order: iroh-direct when the peer is online; relay store-and-forward otherwise.
/// Falls back to relay automatically on iroh failure (transparent to caller).
/// Returns the sent message so the frontend can append it immediately.
#[tauri::command]
pub async fn send_message(
    to: HbId,
    content: String,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<ReceivedMessage> {
    let recipient_pubkey = to.pubkey();

    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Err("Message cannot be empty".into());
    }
    if trimmed.len() > 4096 {
        return Err(format!("Message too long ({} chars, max 4096)", trimmed.len()));
    }

    let guard = identity.read().await;
    let kp = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;

    let sent_at = Utc::now();
    let from = kp.hb_id();
    let aad = message_aad(&from, &to.to_string(), &sent_at.to_rfc3339());

    let encrypted_content = kp.encrypt_for(&recipient_pubkey, &trimmed, &aad).map_err(cmd_err)?;

    let msg = ChatMessage { to: to.to_string(), content: encrypted_content, encrypted: true, sent_at };
    let envelope = SignedEnvelope::create(kp, DocType::Message, &msg).map_err(cmd_err)?;

    // Drop the identity lock before the (potentially slow) network calls.
    drop(guard);

    // Try iroh-direct first; fall back to relay on any failure.
    let delivered_direct = try_send_via_iroh(&relay, &endpoint, &to, &envelope).await;

    if !delivered_direct {
        relay.publish("message", &envelope).await.map_err(cmd_err)?;
    }

    Ok(ReceivedMessage {
        from,
        to: to.to_string(),
        content: trimmed,
        sent_at: msg.sent_at.to_rfc3339(),
        encrypted: true,
    })
}

/// Attempt iroh-direct delivery. Returns true if the remote node accepted the message.
/// All failures are logged and swallowed — the caller falls back to relay.
async fn try_send_via_iroh(
    relay: &crate::relay::RelayClient,
    endpoint_state: &tokio::sync::RwLock<Option<iroh::Endpoint>>,
    to: &str,
    envelope: &SignedEnvelope,
) -> bool {
    let peer = match relay.fetch_peer(to).await {
        Ok(p) => p,
        Err(e) => { tracing::debug!("relay lookup for iroh-DM failed: {e}"); return false; }
    };
    let Some(addr) = peer.node_addr.filter(|_| peer.online) else { return false; };

    let ep_guard = endpoint_state.read().await;
    let Some(ref ep) = *ep_guard else {
        tracing::debug!("iroh endpoint not initialised — falling back to relay for DM");
        return false;
    };

    match node::send_dm_via_iroh(ep, &addr, envelope).await {
        Ok(()) => { tracing::debug!("DM delivered directly via iroh to {to}"); true }
        Err(e) => { tracing::warn!("iroh-direct DM to {to} failed ({e}), falling back to relay"); false }
    }
}

/// Fetch and decrypt the unified DM inbox: direct iroh queue + relay poll.
/// Deduplicates by `(from, sent_at)` so messages delivered via both paths appear once.
/// Respects `allow_dms`: when off, only messages from contacts are returned.
#[tauri::command]
pub async fn get_messages(
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
    store: State<'_, DataStore>,
    dm_queue: State<'_, SharedDmQueue>,
) -> CmdResult<Vec<ReceivedMessage>> {
    let guard = identity.read().await;
    let kp = guard.as_ref().ok_or("No identity loaded.")?;
    let own_hb_id = kp.hb_id();

    // Build contact allow-list if DMs from strangers are disabled.
    let settings = store.load_settings().map_err(cmd_err)?;
    let allow_dms = settings.as_ref().map(|s| s.allow_dms).unwrap_or(true);
    let contact_ids: Option<HashSet<String>> = if !allow_dms {
        Some(store.list_contacts().map_err(cmd_err)?.into_iter().map(|c| c.hb_id).collect())
    } else {
        None
    };

    let mut messages: Vec<ReceivedMessage> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (from, sent_at_rfc3339)

    // ── Source A: direct iroh queue (drain without dropping; keep for dedup) ──
    let direct_envelopes: Vec<SignedEnvelope> = {
        let mut q = dm_queue.lock().await;
        std::mem::take(&mut *q)
    };
    for env in direct_envelopes {
        if let Some(msg) = decode_dm_envelope(&env, kp, &own_hb_id, &contact_ids) {
            let key = (msg.from.clone(), msg.sent_at.clone());
            if seen.insert(key) {
                messages.push(msg);
            }
        }
    }

    // ── Source B: relay poll ──
    match relay.fetch_messages(kp).await {
        Ok(raw) => {
            for (from, chat_msg) in raw {
                if contact_ids.as_ref().is_some_and(|ids| !ids.contains(&from)) {
                    continue;
                }
                let sent_at = chat_msg.sent_at.to_rfc3339();
                let key = (from.clone(), sent_at.clone());
                if !seen.insert(key) {
                    continue; // already delivered via iroh
                }
                let content = decrypt_content(kp, &from, &chat_msg, &own_hb_id);
                messages.push(ReceivedMessage { from, to: chat_msg.to, content, sent_at, encrypted: chat_msg.encrypted });
            }
        }
        Err(e) => tracing::warn!("relay inbox fetch failed: {e}"),
    }

    // Sort oldest first within the combined result.
    messages.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));

    Ok(messages)
}

/// Decode a raw DM envelope from the direct iroh queue into a `ReceivedMessage`.
/// Returns None if the message fails verification, is not addressed to us, or is
/// filtered by the allow_dms setting.
fn decode_dm_envelope(
    env: &SignedEnvelope,
    kp: &hb_core::HoardbookKeypair,
    own_hb_id: &str,
    contact_ids: &Option<HashSet<String>>,
) -> Option<ReceivedMessage> {
    if env.verify().is_err() {
        tracing::warn!("direct DM from {} has invalid signature — discarding", env.public_key);
        return None;
    }
    let msg: ChatMessage = env.parse_payload().ok()?;
    if msg.to != own_hb_id {
        return None;
    }
    let from = env.public_key.clone();
    if contact_ids.as_ref().is_some_and(|ids| !ids.contains(&from)) {
        return None;
    }
    let content = decrypt_content(kp, &from, &msg, own_hb_id);
    Some(ReceivedMessage { from, to: msg.to, content, sent_at: msg.sent_at.to_rfc3339(), encrypted: msg.encrypted })
}

fn decrypt_content(
    kp: &hb_core::HoardbookKeypair,
    from: &str,
    msg: &ChatMessage,
    own_hb_id: &str,
) -> String {
    if !msg.encrypted {
        return msg.content.clone();
    }
    let aad = message_aad(from, own_hb_id, &msg.sent_at.to_rfc3339());
    match hb_core::hb_id_decode(from) {
        Ok(sender_pubkey) => kp
            .decrypt_from(&sender_pubkey, &msg.content, &aad)
            .unwrap_or_else(|_| "[Unable to decrypt]".to_string()),
        Err(_) => "[Unable to decrypt]".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use hb_core::{DocType, HoardbookKeypair, SignedEnvelope, types::ChatMessage};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::node::{SharedDmQueue, handle_node_stream};
    use crate::store::DataStore;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        (dir, DataStore::new(path))
    }

    fn make_dm_envelope(from_kp: &HoardbookKeypair, to_hb_id: &str) -> SignedEnvelope {
        let msg = ChatMessage {
            to: to_hb_id.to_string(),
            content: "hello".to_string(),
            encrypted: false,
            sent_at: chrono::Utc::now(),
        };
        SignedEnvelope::create(from_kp, DocType::Message, &msg).unwrap()
    }

    /// send_dm_via_stream + handle_node_stream round-trip over a duplex pair.
    #[tokio::test]
    async fn send_dm_via_stream_accepted() {
        let (_dir, store) = test_store();
        let recipient_kp = HoardbookKeypair::generate();
        let own_hb_id = recipient_kp.hb_id();
        let sender_kp = HoardbookKeypair::generate();
        let envelope = make_dm_envelope(&sender_kp, &own_hb_id);
        let dm_queue: SharedDmQueue = Arc::new(Mutex::new(vec![]));

        let (server_side, client_side) = tokio::io::duplex(64 * 1024);
        let (client_recv, client_send) = tokio::io::split(client_side);
        let (server_recv, server_send) = tokio::io::split(server_side);

        let store_srv = store.clone();
        let hb_id_srv = own_hb_id.clone();
        let q_srv = dm_queue.clone();
        let server = tokio::spawn(async move {
            handle_node_stream(server_send, server_recv, &store_srv, &hb_id_srv, &q_srv).await.unwrap();
        });

        crate::node::send_dm_via_stream(client_send, client_recv, &envelope).await.unwrap();
        server.await.unwrap();

        let q = dm_queue.lock().await;
        assert_eq!(q.len(), 1, "DM must land in the server's queue");
    }

    /// get_messages deduplicates a message that arrives via both the direct queue
    /// and the relay fetch.
    #[tokio::test]
    async fn dedup_across_sources() {
        let recipient_kp = HoardbookKeypair::generate();
        let sender_kp = HoardbookKeypair::generate();
        let own_hb_id = recipient_kp.hb_id();

        let msg = ChatMessage {
            to: own_hb_id.clone(),
            content: "hello dedup".to_string(),
            encrypted: false,
            sent_at: chrono::DateTime::from_timestamp(1_000_000, 0).unwrap(),
        };
        let env = SignedEnvelope::create(&sender_kp, DocType::Message, &msg).unwrap();

        // Prime the seen set with the same (from, sent_at) key.
        let from = env.public_key.clone();
        let sent_at = msg.sent_at.to_rfc3339();
        let mut seen: std::collections::HashSet<(String, String)> = Default::default();

        // First occurrence inserts → true.
        assert!(seen.insert((from.clone(), sent_at.clone())));
        // Second occurrence (relay path) → false (already seen).
        assert!(!seen.insert((from, sent_at)));
    }

    /// Decryption failure produces the spec placeholder string.
    #[test]
    fn decryption_failure_placeholder() {
        let kp = HoardbookKeypair::generate();
        let other_kp = HoardbookKeypair::generate();
        let msg = ChatMessage {
            to: kp.hb_id(),
            content: "not-valid-ciphertext".to_string(),
            encrypted: true,
            sent_at: chrono::Utc::now(),
        };
        let result = decrypt_content(&kp, &other_kp.hb_id(), &msg, &kp.hb_id());
        assert_eq!(result, "[Unable to decrypt]");
    }

    /// Messages from unknown senders also produce the placeholder.
    #[test]
    fn unknown_sender_key_placeholder() {
        let kp = HoardbookKeypair::generate();
        let msg = ChatMessage {
            to: kp.hb_id(),
            content: "gibberish".to_string(),
            encrypted: true,
            sent_at: chrono::Utc::now(),
        };
        let result = decrypt_content(&kp, "not-a-valid-hb-id", &msg, &kp.hb_id());
        assert_eq!(result, "[Unable to decrypt]");
    }
}
