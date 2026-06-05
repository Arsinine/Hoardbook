//! Background DHT announce service and TCP identity-exchange server.
//!
//! # Protocol
//! Hoardbook DHT discovery uses BEP 5 + a thin identity layer on top:
//!
//! **Announce** (per opted-in tag/content_type):
//! - `info_hash = SHA-1(term_string)`.
//! - `announce_peer(info_hash, dht_identity_port)` so the node appears in `get_peers` results.
//! - TCP server on `dht_identity_port` (default 6882) responds to each connection with a signed
//!   JSON identity payload, then closes.
//!
//! **Search**:
//! - `get_peers(SHA-1(term))` → `Vec<SocketAddrV4>`.
//! - TCP-connect to each address → read `{payload, sig}` → verify Ed25519 → `(hb_id, relay_urls)`.
//! - AND across tags, OR across content-types.
//! - Query relay `GET /v1/peer/:pubkey` for online status and NodeAddr.
//!
//! Peers behind NAT can announce but cannot serve the identity endpoint; searchers skip them.
//! NAT traversal for the identity exchange is deferred to Phase 2.

use mainline::async_dht::AsyncDht;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::Duration,
};

use crate::{relay::RelayClient, SharedIdentity};

const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30 * 60);
const INITIAL_DELAY: Duration = Duration::from_secs(30);
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(5);
const GET_PEERS_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_PEERS_PER_TERM: usize = 300;
/// Max concurrent inbound TCP connections to the identity server.
const MAX_IDENTITY_CONNECTIONS: usize = 64;
/// Max clock skew accepted in a peer identity response before it is rejected.
const MAX_TIMESTAMP_SKEW_SECS: i64 = 300;

// ---------------------------------------------------------------------------
// SHA-1 helper
// ---------------------------------------------------------------------------

/// Compute a `mainline::Id` by SHA-1-hashing `data`.
pub fn sha1_id(data: &[u8]) -> mainline::Id {
    use sha1::Digest;
    let digest = sha1::Sha1::digest(data);
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(&digest);
    mainline::Id::from(bytes)
}

// ---------------------------------------------------------------------------
// Background announce loop
// ---------------------------------------------------------------------------

/// Announce opted-in tags/content-types on the mainline DHT every 30 minutes.
///
/// Initial 30-second delay allows the identity to load before the first announce.
/// Wakes immediately when `cancel_rx` changes: `false` = re-check settings now,
/// `true` = shut down.
///
/// A single DHT node is created once and reused across cycles so its routing
/// table warms up instead of bootstrapping from scratch every 30 min.
pub async fn run_dht_announce_loop(
    identity: SharedIdentity,
    _relay: Arc<RelayClient>,
    store: crate::store::DataStore,
    mut cancel_rx: watch::Receiver<bool>,
) {
    tokio::select! {
        _ = tokio::time::sleep(INITIAL_DELAY) => {}
        _ = cancel_rx.changed() => {
            if *cancel_rx.borrow() { return; }
        }
    }

    let dht = match mainline::Dht::builder().build() {
        Ok(d) => d.as_async(),
        Err(e) => {
            tracing::warn!("DHT announce: failed to create DHT node: {e}");
            return;
        }
    };

    loop {
        let settings = store.load_settings().ok().flatten().unwrap_or_default();

        if settings.dht_announce_enabled {
            let hb_id_str = {
                let guard = identity.read().await;
                guard.as_ref().map(|kp| kp.hb_id())
            };
            if let Some(hb_id_str) = hb_id_str {
                let terms: Vec<String> = settings
                    .dht_announce_tags
                    .iter()
                    .chain(settings.dht_announce_content_types.iter())
                    .cloned()
                    .collect();

                if !terms.is_empty() {
                    for term in &terms {
                        let info_hash = sha1_id(term.as_bytes());
                        match dht.announce_peer(info_hash, Some(settings.dht_identity_port)).await {
                            Ok(_) => tracing::debug!(
                                "DHT announced {:?} on port {}",
                                term,
                                settings.dht_identity_port
                            ),
                            Err(e) => tracing::warn!("DHT announce {:?}: {e}", term),
                        }
                    }
                    tracing::info!(
                        "DHT announce complete: {} terms, hb_id={}",
                        terms.len(),
                        hb_id_str
                    );
                }
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(ANNOUNCE_INTERVAL) => {}
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::debug!("DHT announce loop stopped");
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TCP identity server
// ---------------------------------------------------------------------------

/// Listen on `port` (TCP). For each connection, send a signed identity payload then close.
/// Silently ignores requests when no identity is loaded.
/// At most `MAX_IDENTITY_CONNECTIONS` connections are handled concurrently; excess are dropped.
pub async fn run_identity_server(
    port: u16,
    identity: SharedIdentity,
    relay: Arc<RelayClient>,
    mut cancel_rx: watch::Receiver<bool>,
) {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => {
            tracing::info!("DHT identity server listening on TCP port {port}");
            l
        }
        Err(e) => {
            tracing::warn!("DHT identity server: cannot bind port {port}: {e}");
            return;
        }
    };

    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_IDENTITY_CONNECTIONS));

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        let permit = match Arc::clone(&sem).try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                tracing::debug!("DHT identity server: connection limit reached, dropping {peer_addr}");
                                continue;
                            }
                        };
                        let id2 = Arc::clone(&identity);
                        let relay2 = Arc::clone(&relay);
                        tokio::spawn(async move {
                            let _permit = permit; // released when task completes
                            serve_identity(stream, peer_addr, id2, relay2).await;
                        });
                    }
                    Err(e) => tracing::debug!("DHT identity accept error: {e}"),
                }
            }
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::debug!("DHT identity server stopped");
                    return;
                }
            }
        }
    }
}

async fn serve_identity(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    identity: SharedIdentity,
    relay: Arc<RelayClient>,
) {
    // Compute the signed response while holding the identity read lock,
    // then drop it before the async write so we don't hold it across await.
    let response_bytes = {
        let guard = identity.read().await;
        let Some(ref kp) = *guard else { return };
        let hb_id = kp.hb_id();
        let relay_urls = relay.get_relay_urls().await;
        let payload = serde_json::json!({
            "hb_id": hb_id,
            "relay_urls": relay_urls,
            "timestamp": chrono::Utc::now().timestamp(),
        });
        let sig = kp.sign(&payload);
        let response = serde_json::json!({ "payload": payload, "sig": sig });

        match serde_json::to_vec(&response) {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("DHT identity serialise: {e}");
                return;
            }
        }
    }; // guard drops here

    if let Err(e) = stream.write_all(&response_bytes).await {
        tracing::debug!("DHT identity write to {peer_addr}: {e}");
    }
}

// ---------------------------------------------------------------------------
// Identity client (used by dht_search)
// ---------------------------------------------------------------------------

/// TCP-connect to `addr`, read a signed identity payload, verify signature,
/// and return `(hb_id, relay_urls)`.
///
/// Returns an error if the connection fails, times out, or the signature is invalid.
pub async fn fetch_peer_identity(addr: SocketAddr) -> anyhow::Result<(String, Vec<String>)> {
    let mut stream = tokio::time::timeout(IDENTITY_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("connect timed out to {addr}"))?
        .map_err(|e| anyhow::anyhow!("connect to {addr}: {e}"))?;

    let mut buf = Vec::new();
    tokio::time::timeout(IDENTITY_TIMEOUT, stream.read_to_end(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("read timed out from {addr}"))?
        .map_err(|e| anyhow::anyhow!("read from {addr}: {e}"))?;

    let response: serde_json::Value = serde_json::from_slice(&buf)
        .map_err(|e| anyhow::anyhow!("parse error from {addr}: {e}"))?;

    let payload = response
        .get("payload")
        .ok_or_else(|| anyhow::anyhow!("missing payload"))?;
    let sig = response
        .get("sig")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing sig"))?;
    let hb_id = payload
        .get("hb_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing hb_id"))?;

    let pubkey_bytes = hb_core::hb_id_decode(hb_id)
        .map_err(|e| anyhow::anyhow!("invalid hb_id from {addr}: {e}"))?;

    hb_core::crypto::verify(&pubkey_bytes, payload, sig)
        .map_err(|_| anyhow::anyhow!("invalid signature from {addr}"))?;

    // Reject replayed responses: timestamp must be within ±5 minutes of now.
    let ts = payload
        .get("timestamp")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("missing timestamp from {addr}"))?;
    let skew = (chrono::Utc::now().timestamp() - ts).abs();
    if skew > MAX_TIMESTAMP_SKEW_SECS {
        return Err(anyhow::anyhow!(
            "identity response from {addr} has stale timestamp ({skew}s skew, max {MAX_TIMESTAMP_SKEW_SECS}s)"
        ));
    }

    let relay_urls: Vec<String> = payload
        .get("relay_urls")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    Ok((hb_id.to_string(), relay_urls))
}

// ---------------------------------------------------------------------------
// DHT peer-address collection
// ---------------------------------------------------------------------------

/// Drain a `get_peers` stream until exhausted, 30 s timeout, or 300 addresses collected.
pub async fn collect_peer_addrs(
    dht: &AsyncDht,
    info_hash: mainline::Id,
) -> Vec<SocketAddrV4> {
    use futures::StreamExt as _;

    let mut stream = dht.get_peers(info_hash);
    let mut all: Vec<SocketAddrV4> = Vec::new();
    let deadline = tokio::time::Instant::now() + GET_PEERS_TIMEOUT;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(batch)) => {
                all.extend(batch);
                if all.len() >= MAX_PEERS_PER_TERM {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }

    all.sort_unstable();
    all.dedup();
    all
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_tag_key_is_deterministic() {
        let a = sha1_id(b"nature");
        let b = sha1_id(b"nature");
        assert_eq!(a, b, "same input must produce same Id");
        let c = sha1_id(b"documentary");
        assert_ne!(a, c, "different inputs must produce different Ids");
    }

    #[tokio::test]
    async fn invalid_sig_discarded() {
        let kp = hb_core::HoardbookKeypair::generate();
        let other_kp = hb_core::HoardbookKeypair::generate();

        let no_urls: Vec<String> = vec![];
        let payload = serde_json::json!({
            "hb_id": kp.hb_id(),
            "relay_urls": no_urls,
            "timestamp": 0i64,
        });
        let bad_sig = other_kp.sign(&payload); // signed by wrong key
        let response = serde_json::json!({ "payload": &payload, "sig": bad_sig });
        let bytes = serde_json::to_vec(&response).unwrap();

        // Replay parse + verify logic from fetch_peer_identity.
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let p = parsed.get("payload").unwrap();
        let sig = parsed.get("sig").and_then(|v| v.as_str()).unwrap();
        let hb_id = p.get("hb_id").and_then(|v| v.as_str()).unwrap();
        let pubkey_bytes = hb_core::hb_id_decode(hb_id).unwrap();

        assert!(
            hb_core::crypto::verify(&pubkey_bytes, p, sig).is_err(),
            "wrong signature must be rejected"
        );
    }
}
