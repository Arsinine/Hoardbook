use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::net::SocketAddr;
use hb_core::{
    DocType, HbError, SignedEnvelope,
    types::{ChatMessage, HeartbeatBody},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{db, error::AppError, state::AppState};

// ---------------------------------------------------------------------------
// POST /v1/publish  — messages only
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PublishRequest {
    #[serde(rename = "type")]
    pub doc_type: String,
    pub document: Value,
}

pub async fn publish(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(body): Json<PublishRequest>,
) -> Result<StatusCode, AppError> {
    if !state.rate_limiter.check(&addr.ip().to_string()) {
        return Err(AppError::BadRequest("rate limit exceeded".into()));
    }

    // Only message-type documents are accepted. Profile and collection data is
    // served peer-to-peer via each node's iroh endpoint, not cached on the relay.
    if body.doc_type != "message" {
        return Err(AppError::BadRequest(format!(
            "'{}' documents are not accepted; relay accepts messages only",
            body.doc_type
        )));
    }

    let raw_size = serde_json::to_vec(&body.document)
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .len();

    if raw_size > 6 * 1024 {
        return Err(AppError::TooLarge);
    }

    let envelope: SignedEnvelope = serde_json::from_value(body.document.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid envelope: {e}")))?;

    envelope.verify()?;

    if envelope.doc_type != DocType::Message {
        return Err(AppError::BadRequest(
            "envelope doc_type must be 'message'".into(),
        ));
    }

    let msg: ChatMessage = envelope
        .parse_payload()
        .map_err(|e: HbError| AppError::BadRequest(e.to_string()))?;

    hb_core::hb_id_decode(&msg.to)
        .map_err(|_| AppError::BadRequest("invalid recipient key".into()))?;

    if !timestamp_is_fresh(&msg.sent_at.to_rfc3339()).unwrap_or(false) {
        return Err(AppError::BadRequest(
            "message timestamp out of acceptable range".into(),
        ));
    }

    let count = db::count_messages_for(&state.pool, &msg.to).await?;
    if count >= db::MAX_MESSAGES_PER_RECIPIENT {
        return Err(AppError::BadRequest("recipient mailbox full".into()));
    }

    let pubkey = &envelope.public_key;

    // M6: stop one sender monopolizing a recipient, or flooding many recipients.
    if db::count_messages_from_to(&state.pool, pubkey, &msg.to).await? >= db::MAX_MESSAGES_PER_PAIR {
        return Err(AppError::BadRequest(
            "too many undelivered messages to this recipient".into(),
        ));
    }
    if db::count_messages_from(&state.pool, pubkey).await? >= db::MAX_MESSAGES_PER_SENDER {
        return Err(AppError::BadRequest("sender message quota exceeded".into()));
    }
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    db::insert_message(&state.pool, pubkey, &msg.to, &msg.sent_at.to_rfc3339(), &envelope_json)
        .await?;

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn timestamp_is_fresh(ts: &str) -> Option<bool> {
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let age_secs = chrono::Utc::now()
        .signed_duration_since(dt.with_timezone(&chrono::Utc))
        .num_seconds();
    Some(age_secs.abs() <= 300)
}

// ---------------------------------------------------------------------------
// POST /v1/heartbeat
// ---------------------------------------------------------------------------

pub async fn heartbeat(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(envelope): Json<SignedEnvelope>,
) -> Result<StatusCode, AppError> {
    if !state.rate_limiter.check(&addr.ip().to_string()) {
        return Err(AppError::BadRequest("rate limit exceeded".into()));
    }

    // L16: a heartbeat is a self-contained SignedEnvelope — verify it like any
    // other signed document instead of reconstructing the signed body by hand.
    envelope.verify()?;
    if envelope.doc_type != DocType::Heartbeat {
        return Err(AppError::BadRequest(
            "envelope doc_type must be 'heartbeat'".into(),
        ));
    }

    let body: HeartbeatBody = envelope
        .parse_payload()
        .map_err(|e: HbError| AppError::BadRequest(e.to_string()))?;

    if !timestamp_is_fresh(&body.signed_at).unwrap_or(false) {
        return Err(AppError::BadRequest(
            "heartbeat timestamp out of acceptable range".into(),
        ));
    }

    // M4: cap node_addr length to prevent oversized blobs reaching SQLite.
    if let Some(ref addr) = body.node_addr {
        if addr.len() > 2048 {
            return Err(AppError::BadRequest("node_addr exceeds maximum allowed length".into()));
        }
    }

    db::upsert_heartbeat(&state.pool, &body.public_key, body.node_addr.as_deref()).await?;

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// GET /v1/peer/:pubkey  — online status + NodeAddr only
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PeerResponse {
    pub online: bool,
    pub last_seen_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_addr: Option<String>,
}

const ONLINE_THRESHOLD_SECS: i64 = 600;

pub async fn get_peer(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> Result<Json<PeerResponse>, AppError> {
    if !state.rate_limiter.check(&addr.ip().to_string()) {
        return Err(AppError::BadRequest("rate limit exceeded".into()));
    }
    hb_core::hb_id_decode(&pubkey)?;

    let (online, node_addr, last_seen_at) = match db::get_heartbeat(&state.pool, &pubkey).await? {
        Some((last_seen, addr)) => {
            let age = chrono::Utc::now().timestamp() - last_seen;
            let is_online = age < ONLINE_THRESHOLD_SECS;
            (is_online, if is_online { addr } else { None }, Some(last_seen))
        }
        None => (false, None, None),
    };

    Ok(Json(PeerResponse { online, last_seen_at, node_addr }))
}

// ---------------------------------------------------------------------------
// GET /v1/messages/:pubkey
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MessagesResponse {
    pub messages: Vec<Value>,
}

/// Signed read-authorization for a mailbox fetch (H3). The client signs a JCS-canonical
/// `{purpose, public_key, signed_at}` with its private key; the relay reconstructs that
/// object from the PATH pubkey and verifies, so a signature only ever authorizes the
/// signer's own mailbox.
#[derive(Deserialize)]
pub struct MailboxAuthQuery {
    pub signed_at: String,
    pub signature: String,
}

pub const MAILBOX_READ_PURPOSE: &str = "hoardbook.mailbox.read.v1";

pub async fn get_messages(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
    Query(auth): Query<MailboxAuthQuery>,
) -> Result<Json<MessagesResponse>, AppError> {
    if !state.rate_limiter.check(&addr.ip().to_string()) {
        return Err(AppError::BadRequest("rate limit exceeded".into()));
    }

    let pubkey_bytes = hb_core::hb_id_decode(&pubkey)?;

    if !timestamp_is_fresh(&auth.signed_at).unwrap_or(false) {
        return Err(AppError::BadRequest(
            "auth timestamp out of acceptable range".into(),
        ));
    }

    // Reconstruct the signed object from the PATH pubkey so the signed public_key is
    // forced to equal the mailbox being read; verify against that same key.
    let signed = serde_json::json!({
        "purpose": MAILBOX_READ_PURPOSE,
        "public_key": pubkey,
        "signed_at": auth.signed_at,
    });
    hb_core::crypto::verify(&pubkey_bytes, &signed, &auth.signature)?;

    let envelopes = db::get_messages_for(&state.pool, &pubkey).await?;
    // M1: mailbox growth is controlled by the 30-day TTL expiry task.
    // We do NOT delete here — deleting before the response is confirmed delivered
    // would silently lose messages on any network failure (at-most-once).
    // A proper ACK-based deletion endpoint is tracked in HANDOVER as a post-MVP item.
    let messages = envelopes
        .into_iter()
        .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
        .collect();

    Ok(Json(MessagesResponse { messages }))
}

// ---------------------------------------------------------------------------
// GET /v1/health
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub stored_peers: i64,
    pub peers: Vec<String>,
}

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match db::count_stored_peers(&state.pool).await {
        Ok(count) => Json(HealthResponse {
            ok: true,
            stored_peers: count,
            peers: state.peer_relays.clone(),
        })
        .into_response(),
        Err(e) => {
            tracing::error!("health check db error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RateLimiter;
    use hb_core::{DocType, HoardbookKeypair, SignedEnvelope};
    use hb_core::types::{ChatMessage, HeartbeatBody};
    use sqlx::SqlitePool;
    use std::net::SocketAddr;

    async fn test_state() -> AppState {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        AppState {
            pool,
            rate_limiter: RateLimiter::new(1000, std::time::Duration::from_secs(60)),
            peer_relays: vec![],
        }
    }

    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo("127.0.0.1:1234".parse().unwrap())
    }

    fn make_message_envelope(kp: &HoardbookKeypair, to: &str) -> SignedEnvelope {
        let msg = ChatMessage {
            to: to.to_string(),
            content: "hello".into(),
            encrypted: false,
            sent_at: chrono::Utc::now(),
        };
        SignedEnvelope::create(kp, DocType::Message, &msg).unwrap()
    }

    // --- T7: publish ---

    #[tokio::test]
    async fn publish_non_message_types_rejected() {
        let state = test_state().await;
        for bad_type in &["profile", "collection", "succession"] {
            let req = PublishRequest {
                doc_type: bad_type.to_string(),
                document: serde_json::json!({}),
            };
            let result = publish(test_addr(), State(state.clone()), Json(req)).await;
            assert!(
                matches!(result, Err(AppError::BadRequest(_))),
                "type '{}' must be rejected with 400", bad_type
            );
        }
    }

    #[tokio::test]
    async fn publish_valid_message() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let to = kp.hb_id(); // send to self so recipient key is valid
        let envelope = make_message_envelope(&kp, &to);
        let req = PublishRequest {
            doc_type: "message".into(),
            document: serde_json::to_value(&envelope).unwrap(),
        };
        let result = publish(test_addr(), State(state), Json(req)).await;
        assert!(result.is_ok(), "valid message must be accepted: {:?}", result);
    }

    #[tokio::test]
    async fn publish_tampered_message() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let to = kp.hb_id();
        let mut envelope = make_message_envelope(&kp, &to);
        envelope.payload["content"] = serde_json::json!("injected");
        let req = PublishRequest {
            doc_type: "message".into(),
            document: serde_json::to_value(&envelope).unwrap(),
        };
        let result = publish(test_addr(), State(state), Json(req)).await;
        assert!(matches!(result, Err(_)), "tampered envelope must be rejected");
    }

    #[tokio::test]
    async fn publish_stale_timestamp() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let to = kp.hb_id();
        let msg = ChatMessage {
            to: to.clone(),
            content: "old".into(),
            encrypted: false,
            sent_at: chrono::Utc::now() - chrono::Duration::seconds(600),
        };
        let envelope = SignedEnvelope::create(&kp, DocType::Message, &msg).unwrap();
        let req = PublishRequest {
            doc_type: "message".into(),
            document: serde_json::to_value(&envelope).unwrap(),
        };
        let result = publish(test_addr(), State(state), Json(req)).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "stale timestamp must be rejected"
        );
    }

    #[tokio::test]
    async fn publish_invalid_recipient() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let msg = ChatMessage {
            to: "not_a_valid_hb_id".into(),
            content: "hello".into(),
            encrypted: false,
            sent_at: chrono::Utc::now(),
        };
        let envelope = SignedEnvelope::create(&kp, DocType::Message, &msg).unwrap();
        let req = PublishRequest {
            doc_type: "message".into(),
            document: serde_json::to_value(&envelope).unwrap(),
        };
        let result = publish(test_addr(), State(state), Json(req)).await;
        assert!(matches!(result, Err(AppError::BadRequest(_))), "invalid recipient must be rejected");
    }

    #[tokio::test]
    async fn mailbox_cap_enforced() {
        let state = test_state().await;
        let recipient_kp = HoardbookKeypair::generate();
        let to = recipient_kp.hb_id();

        // Fill to one below the cap using many distinct sender keys so no per-pair
        // or per-sender cap fires before the mailbox total cap.
        let fill_to = db::MAX_MESSAGES_PER_RECIPIENT - 1;
        for i in 0..fill_to {
            let sent_at = chrono::Utc::now() + chrono::Duration::milliseconds(i);
            let fake_from = format!("fake_sender_{i}");
            let msg = ChatMessage { to: to.clone(), content: "x".into(), encrypted: false, sent_at };
            let env = SignedEnvelope::create(&recipient_kp, DocType::Message, &msg).unwrap();
            db::insert_message(&state.pool, &fake_from, &to, &sent_at.to_rfc3339(), &serde_json::to_string(&env).unwrap())
                .await.unwrap();
        }

        // The 500th message (at the cap) must be accepted through the publish handler.
        let sender = HoardbookKeypair::generate();
        let at_cap_msg = ChatMessage {
            to: to.clone(),
            content: "at_cap".into(),
            encrypted: false,
            sent_at: chrono::Utc::now() + chrono::Duration::milliseconds(fill_to),
        };
        let at_cap_env = SignedEnvelope::create(&sender, DocType::Message, &at_cap_msg).unwrap();
        let at_cap_req = PublishRequest {
            doc_type: "message".into(),
            document: serde_json::to_value(&at_cap_env).unwrap(),
        };
        publish(test_addr(), State(state.clone()), Json(at_cap_req))
            .await
            .expect("500th message must be accepted by the publish handler");

        // The 501st message must be rejected via the publish handler's mailbox cap check.
        let sender2 = HoardbookKeypair::generate();
        let overflow_msg = ChatMessage {
            to: to.clone(),
            content: "overflow".into(),
            encrypted: false,
            sent_at: chrono::Utc::now() + chrono::Duration::milliseconds(fill_to + 1),
        };
        let overflow_env = SignedEnvelope::create(&sender2, DocType::Message, &overflow_msg).unwrap();
        let overflow_req = PublishRequest {
            doc_type: "message".into(),
            document: serde_json::to_value(&overflow_env).unwrap(),
        };
        let result = publish(test_addr(), State(state), Json(overflow_req)).await;
        match result {
            Err(AppError::BadRequest(msg)) => assert!(msg.contains("mailbox"), "error must mention mailbox, got: {msg}"),
            other => panic!("expected mailbox BadRequest, got: {:?}", other),
        }
    }

    // --- T8: heartbeat ---

    fn signed_heartbeat(kp: &HoardbookKeypair, node_addr: Option<String>) -> SignedEnvelope {
        let body = HeartbeatBody {
            node_addr,
            public_key: kp.hb_id(),
            signed_at: chrono::Utc::now().to_rfc3339(),
        };
        SignedEnvelope::create(kp, DocType::Heartbeat, &body).unwrap()
    }

    #[tokio::test]
    async fn heartbeat_valid() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let req = signed_heartbeat(&kp, Some("iroh://addr".into()));
        let result = heartbeat(test_addr(), State(state.clone()), Json(req)).await;
        assert!(result.is_ok(), "valid heartbeat must succeed");
        let hb = db::get_heartbeat(&state.pool, &kp.hb_id()).await.unwrap();
        assert!(hb.is_some(), "heartbeat must be stored");
        assert_eq!(hb.unwrap().1.as_deref(), Some("iroh://addr"));
    }

    #[tokio::test]
    async fn heartbeat_stale() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let stale_at = (chrono::Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
        let body = HeartbeatBody { node_addr: None, public_key: kp.hb_id(), signed_at: stale_at };
        let env = SignedEnvelope::create(&kp, DocType::Heartbeat, &body).unwrap();
        let result = heartbeat(test_addr(), State(state), Json(env)).await;
        assert!(matches!(result, Err(AppError::BadRequest(_))), "stale heartbeat must be rejected");
    }

    #[tokio::test]
    async fn heartbeat_invalid_sig() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let mut req = signed_heartbeat(&kp, None);
        req.signature = "deadbeef".repeat(8); // garbage signature
        let result = heartbeat(test_addr(), State(state), Json(req)).await;
        assert!(matches!(result, Err(_)), "invalid signature must be rejected");
    }

    #[tokio::test]
    async fn heartbeat_rate_limited() {
        // Use a very tight rate limiter: 1 request per minute per IP.
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        db::migrate(&pool).await.unwrap();
        let state = AppState {
            pool,
            rate_limiter: RateLimiter::new(1, std::time::Duration::from_secs(60)),
            peer_relays: vec![],
        };
        let kp = HoardbookKeypair::generate();
        let first = heartbeat(test_addr(), State(state.clone()), Json(signed_heartbeat(&kp, None))).await;
        assert!(first.is_ok(), "first heartbeat must pass");
        let second = heartbeat(test_addr(), State(state), Json(signed_heartbeat(&kp, None))).await;
        assert!(matches!(second, Err(AppError::BadRequest(_))), "second request must be rate-limited");
    }

    #[tokio::test]
    async fn heartbeat_oversized_node_addr_rejected() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let oversized_addr = "x".repeat(2049);
        let req = signed_heartbeat(&kp, Some(oversized_addr));
        let result = heartbeat(test_addr(), State(state), Json(req)).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "node_addr exceeding 2048 bytes must be rejected"
        );
    }

    #[tokio::test]
    async fn node_addr_stored_and_cleared() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();

        // Heartbeat with node_addr.
        let req = signed_heartbeat(&kp, Some("iroh://node1".into()));
        heartbeat(test_addr(), State(state.clone()), Json(req)).await.unwrap();
        let hb = db::get_heartbeat(&state.pool, &kp.hb_id()).await.unwrap().unwrap();
        assert_eq!(hb.1.as_deref(), Some("iroh://node1"));

        // Heartbeat without node_addr clears it.
        let req2 = signed_heartbeat(&kp, None);
        heartbeat(test_addr(), State(state.clone()), Json(req2)).await.unwrap();
        let hb2 = db::get_heartbeat(&state.pool, &kp.hb_id()).await.unwrap().unwrap();
        assert!(hb2.1.is_none(), "node_addr must be cleared when absent from heartbeat");
    }

    // --- T9: get_peer ---

    #[tokio::test]
    async fn get_peer_unknown_key() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let result = get_peer(test_addr(), State(state), Path(kp.hb_id())).await.unwrap();
        assert!(!result.0.online);
        assert!(result.0.last_seen_at.is_none());
        assert!(result.0.node_addr.is_none());
    }

    #[tokio::test]
    async fn get_peer_invalid_format() {
        let state = test_state().await;
        let result = get_peer(test_addr(), State(state), Path("not_a_valid_id".into())).await;
        assert!(matches!(result, Err(_)), "invalid key format must return error");
    }

    #[tokio::test]
    async fn get_peer_online() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        db::upsert_heartbeat(&state.pool, &kp.hb_id(), Some("iroh://node1")).await.unwrap();
        let result = get_peer(test_addr(), State(state), Path(kp.hb_id())).await.unwrap();
        assert!(result.0.online);
        assert_eq!(result.0.node_addr.as_deref(), Some("iroh://node1"));
    }

    #[tokio::test]
    async fn get_peer_offline_after_600s() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        // Insert a heartbeat with last_seen > 600s ago.
        sqlx::query(
            "INSERT INTO heartbeats (pubkey, last_seen, node_addr) VALUES (?, ?, ?)"
        )
        .bind(kp.hb_id())
        .bind(chrono::Utc::now().timestamp() - 700)
        .bind("iroh://old")
        .execute(&state.pool)
        .await
        .unwrap();
        let result = get_peer(test_addr(), State(state), Path(kp.hb_id())).await.unwrap();
        assert!(!result.0.online, "peer with old heartbeat must be offline");
        assert!(result.0.node_addr.is_none(), "node_addr must be absent when offline");
        assert!(result.0.last_seen_at.is_some(), "last_seen_at must still be present");
    }

    #[test]
    fn response_has_no_profile_fields() {
        let resp = PeerResponse { online: false, last_seen_at: None, node_addr: None };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("profile"), "profile must not appear in PeerResponse");
        assert!(!json.contains("collections"), "collections must not appear in PeerResponse");
        assert!(!json.contains("succession"), "succession must not appear in PeerResponse");
    }

    // --- T10: get_messages (H3 authenticated) ---

    fn signed_mailbox_query(signer: &HoardbookKeypair, mailbox: &str) -> Query<MailboxAuthQuery> {
        let signed_at = chrono::Utc::now().to_rfc3339();
        let signed = serde_json::json!({
            "purpose": MAILBOX_READ_PURPOSE,
            "public_key": mailbox,
            "signed_at": signed_at,
        });
        Query(MailboxAuthQuery { signed_at, signature: signer.sign(&signed) })
    }

    #[tokio::test]
    async fn get_messages_invalid_key() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let q = signed_mailbox_query(&kp, "bad_key");
        let result = get_messages(test_addr(), State(state), Path("bad_key".into()), q).await;
        assert!(matches!(result, Err(_)), "invalid key must return error");
    }

    #[tokio::test]
    async fn get_messages_valid_auth_returns_ok() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let q = signed_mailbox_query(&kp, &kp.hb_id());
        let result = get_messages(test_addr(), State(state), Path(kp.hb_id()), q).await;
        assert!(result.is_ok(), "valid signed read must be accepted: {result:?}");
    }

    #[tokio::test]
    async fn get_messages_unsigned_or_forged_rejected() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        // Forged signature.
        let mut q = signed_mailbox_query(&kp, &kp.hb_id());
        q.signature = "deadbeef".repeat(8);
        let result = get_messages(test_addr(), State(state.clone()), Path(kp.hb_id()), q).await;
        assert!(matches!(result, Err(_)), "forged signature must be rejected");
    }

    #[tokio::test]
    async fn get_messages_wrong_identity_rejected() {
        // A valid signature by key A must NOT read key B's mailbox (signed key == path key).
        let state = test_state().await;
        let attacker = HoardbookKeypair::generate();
        let victim = HoardbookKeypair::generate();
        // attacker signs over the VICTIM's mailbox id, but with the attacker's key.
        let q = signed_mailbox_query(&attacker, &victim.hb_id());
        let result = get_messages(test_addr(), State(state), Path(victim.hb_id()), q).await;
        assert!(matches!(result, Err(_)), "signature by non-owner must be rejected");
    }

    #[tokio::test]
    async fn get_messages_stale_auth_rejected() {
        let state = test_state().await;
        let kp = HoardbookKeypair::generate();
        let stale_at = (chrono::Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
        let signed = serde_json::json!({
            "purpose": MAILBOX_READ_PURPOSE,
            "public_key": kp.hb_id(),
            "signed_at": stale_at,
        });
        let q = Query(MailboxAuthQuery { signed_at: stale_at, signature: kp.sign(&signed) });
        let result = get_messages(test_addr(), State(state), Path(kp.hb_id()), q).await;
        assert!(matches!(result, Err(AppError::BadRequest(_))), "stale auth must be rejected");
    }

    // --- T11: health ---

    #[tokio::test]
    async fn health_response_format() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        db::migrate(&pool).await.unwrap();
        let state = AppState {
            pool: pool.clone(),
            rate_limiter: RateLimiter::new(30, std::time::Duration::from_secs(60)),
            peer_relays: vec!["https://relay2.example.com".into()],
        };
        // Add one heartbeat so stored_peers == 1.
        db::upsert_heartbeat(&pool, "hb1_testkey", None).await.unwrap();

        let resp = health(State(state)).await.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["ok"], true);
        assert_eq!(health["stored_peers"], 1);
        assert_eq!(health["peers"][0], "https://relay2.example.com");
    }

    #[test]
    fn mailbox_cap_constant_is_500() {
        assert_eq!(db::MAX_MESSAGES_PER_RECIPIENT, 500);
    }

    // --- timestamp helpers ---

    #[test]
    fn fresh_timestamp_accepted() {
        let ts = chrono::Utc::now().to_rfc3339();
        assert_eq!(timestamp_is_fresh(&ts), Some(true));
    }

    #[test]
    fn stale_timestamp_rejected() {
        let old = (chrono::Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
        assert_eq!(timestamp_is_fresh(&old), Some(false));
    }

    #[test]
    fn future_timestamp_too_far_rejected() {
        let future = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
        assert_eq!(timestamp_is_fresh(&future), Some(false));
    }

    #[test]
    fn recent_timestamp_accepted() {
        let recent = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        assert_eq!(timestamp_is_fresh(&recent), Some(true));
    }

    #[test]
    fn invalid_timestamp_returns_none() {
        assert_eq!(timestamp_is_fresh("not-a-timestamp"), None);
        assert_eq!(timestamp_is_fresh(""), None);
        assert_eq!(timestamp_is_fresh("2026-13-01T00:00:00Z"), None);
    }
}
