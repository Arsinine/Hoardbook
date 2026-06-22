//! Presence: a status-only online beacon. Hoardbook moves no files (transfer lives in the Mascara
//! companion — INV-4), so presence carries **no dialable address and no node key** — it is purely a
//! freshness signal so peers can see you're recently online.
//!
//! Republished to the configured relays on a ~5-minute cadence as a signed, kind-11111 event
//! (`build_binding`); `verify_binding` on the reader side checks signature + author-pin +
//! freshness/expiry for online status.

use std::time::Duration;

use anyhow::{anyhow, Result};
use hb_core::{build_binding, Identity};
use hb_net::RelayClient;
use nostr::prelude::*;

use crate::identity_state::SharedIdentity;
use crate::store::DataStore;

/// Binding validity window. Presence refreshes every ~5 min, so 30 min is a generous backstop
/// (and well within the `MAX_BINDING_TTL_SECS` cap hb-core enforces).
pub const PRESENCE_TTL_SECS: u64 = 30 * 60;
/// Republish cadence.
pub const PRESENCE_REFRESH_SECS: u64 = 5 * 60;
/// First publish fires shortly after launch (the endpoint needs a moment to bind).
const PRESENCE_FIRST_DELAY_SECS: u64 = 15;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build + publish a status-only presence beacon: a signed kind-11111 event carrying only
/// freshness/expiry — no node key, no dialable address (transfer moved to Mascara). The reader only
/// checks signature + author-pin + freshness for online status.
pub(crate) async fn publish_presence(client: &RelayClient, identity: &Identity) -> Result<()> {
    let event = build_binding(identity, unix_now(), PRESENCE_TTL_SECS)
        .map_err(|e| anyhow!("build presence beacon: {e}"))?;
    client.publish(&event).await.map_err(|e| anyhow!("publish presence: {e}"))?;
    Ok(())
}

/// Fetch a peer's newest presence event (kind 11111, author-pinned). The caller verifies the
/// binding (`hb-core::verify_binding`) before trusting it for online status.
pub(crate) async fn fetch_peer_presence(
    client: &RelayClient,
    peer: &PublicKey,
    timeout: Duration,
) -> Result<Option<Event>> {
    let events = client
        .fetch(Filter::new().author(*peer).kind(Kind::from_u16(hb_core::binding::KIND_PRESENCE)), timeout)
        .await
        .map_err(|e| anyhow!("fetch presence: {e}"))?;
    Ok(hb_net::select_newest_by_created_at(events))
}

/// Background loop: republish presence on a fixed cadence while an identity + bound endpoint exist.
/// Replaces the legacy keepalive task. Best-effort — a missing relay/endpoint just skips the
/// cycle. `false` on the cancel channel wakes it early; `true` shuts it down. `wakeups` counts loop
/// iterations so the L4 idle guard can assert the loop sleeps between cycles (never busy-spins — the
/// 2026-06-07 GUI-loop-spin class).
pub(crate) async fn run_presence_loop(
    identity: SharedIdentity,
    store: DataStore,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    wakeups: std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    let mut delay = Duration::from_secs(PRESENCE_FIRST_DELAY_SECS);
    loop {
        wakeups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::debug!("presence loop cancelled");
                    break;
                }
            }
        }
        delay = Duration::from_secs(PRESENCE_REFRESH_SECS);

        // Snapshot the identity (clone the secp256k1 key) without holding the lock across the
        // network call.
        let snapshot = {
            let guard = identity.read().await;
            guard.as_ref().map(|id| id.identity.clone())
        };
        let Some(id) = snapshot else { continue };

        match crate::net::connect(&id, &store).await {
            Ok(client) => {
                if let Err(e) = publish_presence(&client, &id).await {
                    tracing::debug!("presence publish failed: {e}");
                }
                client.disconnect().await;
            }
            Err(e) => tracing::debug!("presence: no relay this cycle ({e})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// L4 idle guard (presence half): with no identity loaded, the loop sleeps on its first-delay
    /// timer and does **not** busy-spin. Over a 300 ms window it must wake only a handful of times —
    /// the spinning-loop counter-fixture in `watch.rs` proves the same measure flags a hot loop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn presence_loop_idles_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity: SharedIdentity = Arc::new(RwLock::new(None));
        let wakeups = Arc::new(AtomicU64::new(0));
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        let handle = tokio::spawn(run_presence_loop(
            identity,
            store,
            cancel_rx,
            Arc::clone(&wakeups),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        let woke = wakeups.load(Ordering::Relaxed);
        assert!(woke < 100, "idle presence loop woke {woke} times in 300ms — busy-spinning?");
    }
}
