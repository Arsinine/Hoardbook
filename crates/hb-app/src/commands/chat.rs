use chrono::Utc;
use hb_core::{DocType, HbId, SignedEnvelope, types::ChatMessage};
use serde::Serialize;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    SharedIdentity, SharedRelay,
    store::DataStore,
};

/// L12: associated data that binds a message ciphertext to its routing and
/// timestamp. Built only from unencrypted fields, so the recipient can reconstruct
/// it before decrypting. Must be byte-identical on encrypt and decrypt.
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

/// Encrypt and send a chat message to `to` via all configured relays.
/// Returns the sent message so the frontend can append it immediately.
#[tauri::command]
pub async fn send_message(
    to: HbId,
    content: String,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<ReceivedMessage> {
    let recipient_pubkey = to.pubkey();

    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Err("Message cannot be empty".into());
    }
    if trimmed.len() > 4096 {
        return Err(format!(
            "Message too long ({} chars, max 4096)",
            trimmed.len()
        ));
    }

    let guard = identity.read().await;
    let kp = guard
        .as_ref()
        .ok_or("No identity loaded. Generate a keypair first.")?;

    let sent_at = Utc::now();
    let from = kp.hb_id();
    let aad = message_aad(&from, &to.to_string(), &sent_at.to_rfc3339());

    let encrypted_content = kp
        .encrypt_for(&recipient_pubkey, &trimmed, &aad)
        .map_err(cmd_err)?;

    let msg = ChatMessage {
        to: to.to_string(),
        content: encrypted_content,
        encrypted: true,
        sent_at,
    };

    let envelope = SignedEnvelope::create(kp, DocType::Message, &msg).map_err(cmd_err)?;

    relay.publish("message", &envelope).await.map_err(cmd_err)?;

    Ok(ReceivedMessage {
        from,
        to: to.to_string(),
        content: trimmed,
        sent_at: sent_at.to_rfc3339(),
        encrypted: true,
    })
}

/// Fetch and decrypt messages from all relays addressed to the current user's inbox.
/// Messages with invalid or undecryptable content are returned with a placeholder.
/// Respects the `allow_dms` setting: when off, only messages from contacts are returned.
#[tauri::command]
pub async fn get_messages(
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<ReceivedMessage>> {
    let guard = identity.read().await;
    let kp = guard.as_ref().ok_or("No identity loaded.")?;

    let raw = relay.fetch_messages(kp).await.map_err(cmd_err)?;

    // Build contact allow-list if DMs from strangers are disabled.
    let settings = store.load_settings().map_err(cmd_err)?;
    let allow_dms = settings.as_ref().map(|s| s.allow_dms).unwrap_or(true);
    let contact_ids: Option<std::collections::HashSet<String>> = if !allow_dms {
        let contacts = store.list_contacts().map_err(cmd_err)?;
        Some(contacts.into_iter().map(|c| c.hb_id).collect())
    } else {
        None
    };

    let messages = raw
        .into_iter()
        .filter(|(from, _)| {
            contact_ids.as_ref().is_none_or(|ids| ids.contains(from))
        })
        .map(|(from, msg)| {
            let content = if msg.encrypted {
                match hb_core::hb_id_decode(&from) {
                    Ok(sender_pubkey) => {
                        let aad = message_aad(&from, &msg.to, &msg.sent_at.to_rfc3339());
                        kp.decrypt_from(&sender_pubkey, &msg.content, &aad)
                            .unwrap_or_else(|_| "[decryption failed]".to_string())
                    }
                    Err(_) => "[unknown sender key]".to_string(),
                }
            } else {
                msg.content
            };

            ReceivedMessage {
                from,
                to: msg.to,
                content,
                sent_at: msg.sent_at.to_rfc3339(),
                encrypted: msg.encrypted,
            }
        })
        .collect();

    Ok(messages)
}
