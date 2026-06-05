use chrono::Utc;
use hb_core::{DocType, HoardbookKeypair, SignedEnvelope, types::{ChatMessage, HeartbeatBody}};
use serde_json::Value;

pub const MAILBOX_READ_PURPOSE: &str = "hoardbook.mailbox.read.v1";

/// Build a signed heartbeat envelope.
pub fn make_heartbeat(kp: &HoardbookKeypair, node_addr: Option<String>) -> SignedEnvelope {
    let body = HeartbeatBody {
        public_key: kp.hb_id(),
        node_addr,
        signed_at: Utc::now().to_rfc3339(),
    };
    SignedEnvelope::create(kp, DocType::Heartbeat, &body).unwrap()
}

/// Build a stale heartbeat (signed_at in the past by `age_secs`).
pub fn make_stale_heartbeat(kp: &HoardbookKeypair, age_secs: i64) -> SignedEnvelope {
    let body = HeartbeatBody {
        public_key: kp.hb_id(),
        node_addr: None,
        signed_at: (Utc::now() - chrono::Duration::seconds(age_secs)).to_rfc3339(),
    };
    SignedEnvelope::create(kp, DocType::Heartbeat, &body).unwrap()
}

/// Build a message envelope. `offset_ms` ensures unique sent_at timestamps.
pub fn make_message(from_kp: &HoardbookKeypair, to_id: &str, offset_ms: i64) -> SignedEnvelope {
    let msg = ChatMessage {
        to: to_id.to_string(),
        content: "test".into(),
        encrypted: false,
        sent_at: Utc::now() + chrono::Duration::milliseconds(offset_ms),
    };
    SignedEnvelope::create(from_kp, DocType::Message, &msg).unwrap()
}

/// Build a stale message (sent_at in the past by `age_secs`).
pub fn make_stale_message(from_kp: &HoardbookKeypair, to_id: &str, age_secs: i64) -> SignedEnvelope {
    let msg = ChatMessage {
        to: to_id.to_string(),
        content: "stale test".into(),
        encrypted: false,
        sent_at: Utc::now() - chrono::Duration::seconds(age_secs),
    };
    SignedEnvelope::create(from_kp, DocType::Message, &msg).unwrap()
}

/// Build an E2E-encrypted message envelope.
/// AAD format matches `message_aad()` in hb-app/src/commands/chat.rs.
pub fn make_encrypted_message(
    from_kp: &HoardbookKeypair,
    to_id: &str,
    plaintext: &str,
    offset_ms: i64,
) -> SignedEnvelope {
    let sent_at = Utc::now() + chrono::Duration::milliseconds(offset_ms);
    let from = from_kp.hb_id();
    let aad = hb_core::jcs::canonicalize(&serde_json::json!({
        "from": from,
        "to": to_id,
        "sent_at": sent_at.to_rfc3339(),
    }));
    let to_pubkey = hb_core::hb_id_decode(to_id).unwrap();
    let ciphertext = from_kp.encrypt_for(&to_pubkey, plaintext, &aad).unwrap();
    let msg = ChatMessage {
        to: to_id.to_string(),
        content: ciphertext,
        encrypted: true,
        sent_at,
    };
    SignedEnvelope::create(from_kp, DocType::Message, &msg).unwrap()
}

/// Signed mailbox read query params. Returns (signed_at, signature).
pub fn mailbox_auth(kp: &HoardbookKeypair) -> (String, String) {
    let signed_at = Utc::now().to_rfc3339();
    let signed = serde_json::json!({
        "purpose": MAILBOX_READ_PURPOSE,
        "public_key": kp.hb_id(),
        "signed_at": signed_at,
    });
    let sig = kp.sign(&signed);
    (signed_at, sig)
}

/// POST /v1/publish wrapper body.
pub fn publish_body(envelope: &SignedEnvelope) -> Value {
    serde_json::json!({
        "type": "message",
        "document": serde_json::to_value(envelope).unwrap(),
    })
}
