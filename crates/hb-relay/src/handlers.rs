use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::net::SocketAddr;
use hb_core::{
    DocType, HbError, SignedEnvelope,
    types::{ChatMessage, Collection, HeartbeatBody},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{db, error::AppError, state::AppState};

// ---------------------------------------------------------------------------
// POST /v1/publish
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

    let raw_size = serde_json::to_vec(&body.document)
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .len();

    if body.doc_type == "collection" && raw_size > state.max_collection_bytes {
        return Err(AppError::TooLarge);
    }
    if raw_size > 64 * 1024 && body.doc_type != "collection" {
        return Err(AppError::TooLarge);
    }

    let envelope: SignedEnvelope = serde_json::from_value(body.document.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid envelope: {e}")))?;

    envelope.verify()?;

    let expected_type = match envelope.doc_type {
        DocType::Profile    => "profile",
        DocType::Collection => "collection",
        DocType::Message    => "message",
    };
    if body.doc_type != expected_type {
        return Err(AppError::BadRequest(format!(
            "type mismatch: request says '{}', envelope says '{}'",
            body.doc_type, expected_type
        )));
    }

    let pubkey = &envelope.public_key;
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match envelope.doc_type {
        DocType::Collection => {
            let collection: Collection = envelope
                .parse_payload()
                .map_err(|e: HbError| AppError::BadRequest(e.to_string()))?;
            db::upsert_collection(&state.pool, pubkey, &collection.slug, &envelope_json).await?;
        }
        DocType::Profile => {
            db::upsert_document(&state.pool, pubkey, "profile", &envelope_json).await?;
        }
        DocType::Message => {
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

            if msg.content.len() > 6000 {
                return Err(AppError::TooLarge);
            }

            let count = db::count_messages_for(&state.pool, &msg.to).await?;
            if count >= db::MAX_MESSAGES_PER_RECIPIENT {
                return Err(AppError::BadRequest("recipient mailbox full".into()));
            }

            db::insert_message(
                &state.pool,
                pubkey,
                &msg.to,
                &msg.sent_at.to_rfc3339(),
                &envelope_json,
            )
            .await?;
        }
    }

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

#[derive(Deserialize)]
pub struct HeartbeatRequest {
    pub public_key: String,
    pub signed_at: String,
    pub node_addr: Option<String>,
    pub signature: String,
}

pub async fn heartbeat(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(body): Json<HeartbeatRequest>,
) -> Result<StatusCode, AppError> {
    if !state.rate_limiter.check(&addr.ip().to_string()) {
        return Err(AppError::BadRequest("rate limit exceeded".into()));
    }

    if !timestamp_is_fresh(&body.signed_at).unwrap_or(false) {
        return Err(AppError::BadRequest(
            "heartbeat timestamp out of acceptable range".into(),
        ));
    }

    let signed_body = HeartbeatBody {
        node_addr: body.node_addr.clone(),
        public_key: body.public_key.clone(),
        signed_at: body.signed_at.clone(),
    };
    let signed_value = serde_json::to_value(&signed_body)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let pubkey_bytes = hb_core::hb_id_decode(&body.public_key)?;
    hb_core::crypto::verify(&pubkey_bytes, &signed_value, &body.signature)?;

    db::upsert_heartbeat(
        &state.pool,
        &body.public_key,
        body.node_addr.as_deref(),
    )
    .await?;

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// GET /v1/peer/:pubkey
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PeerResponse {
    pub profile: Option<Value>,
    pub collections: Vec<Value>,
    pub succession: Option<Value>,
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_addr: Option<String>,
    /// Unix timestamp of the peer's last heartbeat, so clients can display
    /// accurate "last seen" times rather than just online/offline status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<i64>,
}

const ONLINE_THRESHOLD_SECS: i64 = 600;

pub async fn get_peer(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> Result<Json<PeerResponse>, AppError> {
    hb_core::hb_id_decode(&pubkey)?;

    let profile = db::get_document(&state.pool, &pubkey, "profile")
        .await?
        .and_then(|s| serde_json::from_str(&s).ok());

    let succession = db::get_document(&state.pool, &pubkey, "succession")
        .await?
        .and_then(|s| serde_json::from_str(&s).ok());

    let collections = db::get_collections(&state.pool, &pubkey)
        .await?
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();

    let (online, node_addr, last_seen_at) = match db::get_heartbeat(&state.pool, &pubkey).await? {
        Some((last_seen, addr)) => {
            let age = chrono::Utc::now().timestamp() - last_seen;
            (age < ONLINE_THRESHOLD_SECS, addr, Some(last_seen))
        }
        None => (false, None, None),
    };

    Ok(Json(PeerResponse {
        profile,
        collections,
        succession,
        online,
        node_addr,
        last_seen_at,
    }))
}

// ---------------------------------------------------------------------------
// GET /v1/messages/:pubkey
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MessagesResponse {
    pub messages: Vec<Value>,
}

pub async fn get_messages(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> Result<Json<MessagesResponse>, AppError> {
    hb_core::hb_id_decode(&pubkey)?;

    let envelopes = db::get_messages_for(&state.pool, &pubkey).await?;
    let messages = envelopes
        .into_iter()
        .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
        .collect();

    Ok(Json(MessagesResponse { messages }))
}

// ---------------------------------------------------------------------------
// POST /v1/deactivate  — signed self-removal request
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DeactivateRequest {
    pub public_key: String,
    pub signed_at: String,
    pub action: String,
    pub signature: String,
}

#[derive(Serialize)]
struct DeactivateBody {
    public_key: String,
    signed_at: String,
    action: String,
}

pub async fn deactivate_peer(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(body): Json<DeactivateRequest>,
) -> Result<StatusCode, AppError> {
    if !state.rate_limiter.check(&addr.ip().to_string()) {
        return Err(AppError::BadRequest("rate limit exceeded".into()));
    }

    if body.action != "deactivate" {
        return Err(AppError::BadRequest("invalid action".into()));
    }

    if !timestamp_is_fresh(&body.signed_at).unwrap_or(false) {
        return Err(AppError::BadRequest("timestamp out of acceptable range".into()));
    }

    let signed = DeactivateBody {
        public_key: body.public_key.clone(),
        signed_at: body.signed_at.clone(),
        action: body.action.clone(),
    };
    let signed_value = serde_json::to_value(&signed)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let pubkey_bytes = hb_core::hb_id_decode(&body.public_key)?;
    hb_core::crypto::verify(&pubkey_bytes, &signed_value, &body.signature)?;

    db::deactivate_peer(&state.pool, &body.public_key).await?;

    Ok(StatusCode::OK)
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
    match db::count_stored(&state.pool).await {
        Ok(count) => Json(HealthResponse {
            ok: true,
            stored_peers: count,
            peers: vec![],
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
