use chrono::Utc;
use hb_core::{ChannelMessage, DocType, SignedEnvelope, types::ChatMessage};
use serde::Serialize;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    SharedIdentity, SharedRelay,
    store::DataStore,
};

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
    to: String,
    content: String,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<ReceivedMessage> {
    // Validate recipient and decode their public key for encryption.
    let recipient_pubkey = hb_core::hb_id_decode(&to)
        .map_err(|_| "Invalid recipient ID".to_string())?;

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

    let encrypted_content = kp
        .encrypt_for(&recipient_pubkey, &trimmed)
        .map_err(cmd_err)?;

    let sent_at = Utc::now();
    let msg = ChatMessage {
        to: to.clone(),
        content: encrypted_content,
        encrypted: true,
        sent_at,
    };

    let envelope = SignedEnvelope::create(kp, DocType::Message, &msg).map_err(cmd_err)?;
    let from = kp.hb_id();

    relay.publish("message", &envelope).await.map_err(cmd_err)?;

    Ok(ReceivedMessage {
        from,
        to,
        content: trimmed, // return plaintext to the frontend
        sent_at: sent_at.to_rfc3339(),
        encrypted: true,
    })
}

/// A decoded channel message returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct ReceivedChannelMessage {
    pub from: String,
    pub content: String,
    pub sent_at: String,
}

/// Fetch recent messages from a public channel.
#[tauri::command]
pub async fn get_channel_messages(
    channel: String,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<ReceivedChannelMessage>> {
    let raw = relay.fetch_channel_messages(&channel).await.map_err(cmd_err)?;
    let messages = raw
        .into_iter()
        .map(|(from, msg)| ReceivedChannelMessage {
            from,
            content: msg.content,
            sent_at: msg.sent_at.to_rfc3339(),
        })
        .collect();
    Ok(messages)
}

/// Post a plain-text message to a public channel.
#[tauri::command]
pub async fn post_channel_message(
    channel: String,
    content: String,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<ReceivedChannelMessage> {
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Err("Message cannot be empty".into());
    }
    if trimmed.len() > 1000 {
        return Err(format!("Message too long ({} chars, max 1000)", trimmed.len()));
    }

    let guard = identity.read().await;
    let kp = guard.as_ref().ok_or("No identity loaded.")?;

    let sent_at = Utc::now();
    let msg = ChannelMessage {
        channel: channel.clone(),
        content: trimmed.clone(),
        sent_at,
    };

    let envelope = SignedEnvelope::create(kp, DocType::Channel, &msg).map_err(cmd_err)?;
    let from = kp.hb_id();

    relay.post_channel_message(&channel, &envelope).await.map_err(cmd_err)?;

    Ok(ReceivedChannelMessage { from, content: trimmed, sent_at: sent_at.to_rfc3339() })
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
    let my_pubkey = kp.hb_id();

    let raw = relay.fetch_messages(&my_pubkey).await.map_err(cmd_err)?;

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
            contact_ids.as_ref().map_or(true, |ids| ids.contains(from))
        })
        .map(|(from, msg)| {
            let content = if msg.encrypted {
                match hb_core::hb_id_decode(&from) {
                    Ok(sender_pubkey) => kp
                        .decrypt_from(&sender_pubkey, &msg.content)
                        .unwrap_or_else(|_| "[decryption failed]".to_string()),
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
