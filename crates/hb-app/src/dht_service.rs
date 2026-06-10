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
// Loop decisions (extracted so the lazy-build / initial-delay invariants are
// unit-testable without building a real DHT or running the loop forever)
// ---------------------------------------------------------------------------

/// What `run_dht_announce_loop` does on a given cycle.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DhtCycle {
    /// Announce disabled — idle without ever building the DHT node.
    Idle,
    /// Enabled, no node yet — lazily build one (once).
    Build,
    /// Enabled, node already built — announce on the existing node.
    Announce,
}

/// The loop's single build gate. Locks HANDOVER Bug #2: when announce is disabled
/// the action is always `Idle` and the DHT is **never** built — regardless of node
/// state — so the default, never-opted-in user pays zero DHT bootstrap cost (the old
/// code built unconditionally, spinning a no-backoff UDP retry storm on networks that
/// block DHT). Built lazily on the first enabled cycle, then reused (`Announce`).
pub(crate) fn dht_cycle_action(dht_announce_enabled: bool, dht_built: bool) -> DhtCycle {
    if !dht_announce_enabled {
        DhtCycle::Idle
    } else if !dht_built {
        DhtCycle::Build
    } else {
        DhtCycle::Announce
    }
}

/// Wait out `INITIAL_DELAY` before the first DHT build, returning early only if
/// cancellation fires (returns `true` = stop). Arms `cancel_rx` with
/// `mark_unchanged()` first — HANDOVER Bug #1: a watch receiver whose current value
/// is still marked *unseen* makes `changed()` resolve on the first poll, which would
/// let the select fall through immediately and skip the delay, building the DHT at
/// process start. A real cancel sent *during* the delay still wins via `changed()`.
async fn await_initial_delay(cancel_rx: &mut watch::Receiver<bool>) -> bool {
    cancel_rx.mark_unchanged();
    tokio::select! {
        _ = tokio::time::sleep(INITIAL_DELAY) => false,
        _ = cancel_rx.changed() => *cancel_rx.borrow(),
    }
}

// ---------------------------------------------------------------------------
// Background announce loop
// ---------------------------------------------------------------------------

/// Announce opted-in tags/content-types on the mainline DHT every 30 minutes.
///
/// DHT discovery is **opt-in**, so the mainline node is built *lazily* — only
/// once `dht_announce_enabled` is set — and then reused across cycles so its
/// routing table warms up instead of bootstrapping from scratch every 30 min.
/// Building it unconditionally bootstraps mainline on every launch; on networks
/// that block outbound DHT UDP that bootstrap spins in a no-backoff retry storm
/// that pegs the CPU and starves the rest of the app (e.g. directory scans hang).
///
/// Wakes immediately when `cancel_rx` changes: `false` = re-check settings now
/// (e.g. the user just toggled announce), `true` = shut down.
pub async fn run_dht_announce_loop(
    identity: SharedIdentity,
    _relay: Arc<RelayClient>,
    store: crate::store::DataStore,
    mut cancel_rx: watch::Receiver<bool>,
) {
    let mut dht: Option<AsyncDht> = None;

    loop {
        let settings = store.load_settings().ok().flatten().unwrap_or_default();

        match dht_cycle_action(settings.dht_announce_enabled, dht.is_some()) {
            DhtCycle::Idle => {
                // Announce disabled: idle until the user enables it or the app shuts
                // down. Crucially, never bootstrap the DHT here — the default,
                // never-opted-in user must pay no DHT cost.
                if cancel_rx.changed().await.is_err() || *cancel_rx.borrow() {
                    return;
                }
                continue;
            }
            DhtCycle::Build => {
                // First announce since launch: wait out INITIAL_DELAY (giving the
                // identity a moment to load), then bootstrap the node once.
                if await_initial_delay(&mut cancel_rx).await {
                    return; // cancelled during the delay
                }
                match mainline::Dht::builder().build() {
                    Ok(d) => dht = Some(d.as_async()),
                    Err(e) => {
                        tracing::warn!("DHT announce: failed to create DHT node: {e}");
                        // Back off before retrying so a persistent failure doesn't spin.
                        tokio::select! {
                            _ = tokio::time::sleep(ANNOUNCE_INTERVAL) => {}
                            _ = cancel_rx.changed() => {
                                if *cancel_rx.borrow() { return; }
                            }
                        }
                        continue;
                    }
                }
            }
            DhtCycle::Announce => {}
        }
        let dht = dht.as_ref().expect("DHT built above");

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

    // HANDOVER scenario 5, Bug #2: `dht_cycle_action` is the loop's single build
    // gate (the loop `match`es on it). When announce is disabled the action must be
    // `Idle` — never `Build` — no matter the node state, so the default user never
    // bootstraps the DHT. A regression that builds while disabled has to break this.
    #[test]
    fn dht_cycle_idles_and_never_builds_when_disabled() {
        assert_eq!(dht_cycle_action(false, false), DhtCycle::Idle);
        // Disabled => Idle even if a node somehow already exists: never (re)build.
        assert_eq!(dht_cycle_action(false, true), DhtCycle::Idle);
        // Enabled, no node yet => build exactly once.
        assert_eq!(dht_cycle_action(true, false), DhtCycle::Build);
        // Enabled, node present => reuse it, never rebuild.
        assert_eq!(dht_cycle_action(true, true), DhtCycle::Announce);
    }

    // HANDOVER scenario 5, Bug #1: `await_initial_delay` (used by the loop before the
    // first build) must wait out INITIAL_DELAY and must NOT short-circuit when
    // `cancel_rx`'s value is still marked *unseen* — the state that skipped the delay
    // and let the DHT bootstrap at process start. The helper arms the receiver via
    // `mark_unchanged()`; reverting that call regresses this test (the timeout would
    // resolve `Ok` immediately instead of `Err`). INITIAL_DELAY is 30s, so a correct
    // helper is still sleeping well past the 50ms probe.
    #[tokio::test]
    async fn await_initial_delay_does_not_short_circuit_on_unseen_cancel() {
        use std::time::Duration;
        use tokio::sync::watch;

        let (tx, mut cancel_rx) = watch::channel(false);
        tx.send(false).unwrap(); // bump version → cancel_rx's current value is unseen
        assert!(cancel_rx.has_changed().unwrap(), "precondition: value is unseen");

        let waited =
            tokio::time::timeout(Duration::from_millis(50), await_initial_delay(&mut cancel_rx))
                .await;
        assert!(
            waited.is_err(),
            "await_initial_delay must wait out INITIAL_DELAY, not short-circuit on an unseen cancel value"
        );
    }

    // Companion to the above: a real cancel sent *during* the delay must still stop
    // the loop promptly (mark_unchanged must not swallow a genuine shutdown signal).
    #[tokio::test]
    async fn await_initial_delay_returns_true_when_cancelled_during_wait() {
        use tokio::sync::watch;

        let (tx, cancel_rx) = watch::channel(false);
        let mut cancel_rx = cancel_rx;
        let handle = tokio::spawn(async move { await_initial_delay(&mut cancel_rx).await });
        // current-thread test runtime: yield so the spawned helper arms + reaches the
        // select await point, then request shutdown.
        tokio::task::yield_now().await;
        tx.send(true).unwrap();
        assert!(
            handle.await.unwrap(),
            "a shutdown sent during the initial delay must return true (stop the loop)"
        );
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
