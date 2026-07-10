//! Presence: a status-only online beacon. Hoardbook moves no files (transfer lives in the Mascara
//! companion — INV-4), so presence carries **no dialable address and no node key** — it is purely a
//! freshness signal so peers can see you're recently online.
//!
//! Republished to the configured relays on a ~5-minute cadence as a signed, kind-11111 event
//! (`build_binding`); `verify_binding` on the reader side checks signature + author-pin +
//! freshness/expiry for online status.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use hb_core::{build_binding, Identity};
use hb_net::{PublishOutcome, RelayClient};
use nostr::prelude::*;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::identity_state::SharedIdentity;
use crate::net::SharedRelay;
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

/// Per-relay outcome of the most recent beacon publish attempt (devtest #9 same-NAT diagnosis) —
/// surfaces `hb-net::PublishOutcome`'s per-relay accept/reject evidence to Settings instead of
/// swallowing it at `tracing::debug`. The beacon rides the same relay pool as every other outbound
/// write (DMs, discovery), so its health is a generic canary for the write path, not presence-only.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BeaconRelayOutcome {
    pub url: String,
    /// `"accepted"` or `"rejected"`.
    pub outcome: String,
    pub reason: Option<String>,
}

/// Rolling beacon-health snapshot, read by the `beacon_status` command.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BeaconReport {
    /// Unix seconds of the most recent attempt (0 = never attempted).
    pub last_attempt_at: u64,
    /// Unix seconds of the most recent attempt that reached a relay (Ok, regardless of per-relay
    /// accept/reject) — distinct from `last_attempt_at` so a run of client-acquire errors doesn't
    /// look like a stale-but-successful beacon.
    pub last_success_at: u64,
    pub relays: Vec<BeaconRelayOutcome>,
    /// Set when the whole attempt failed before reaching any relay (e.g. no client this cycle);
    /// cleared on the next attempt that reaches a relay.
    pub last_error: Option<String>,
}

pub type SharedBeaconState = Arc<RwLock<BeaconReport>>;

/// Pure state transition for a beacon attempt — testable without a relay. `Ok` carries the publish
/// outcome (mapped into per-relay rows, success timestamp bumped, error cleared); `Err` carries the
/// failure message (attempt timestamp bumped, error set, but the last-known-good `relays` +
/// `last_success_at` are preserved so a transient failure doesn't blank the panel).
fn record_outcome(prev: &BeaconReport, result: Result<&PublishOutcome, &str>, now: u64) -> BeaconReport {
    match result {
        Ok(outcome) => {
            let mut relays: Vec<BeaconRelayOutcome> = outcome
                .accepted
                .iter()
                .map(|url| BeaconRelayOutcome { url: url.clone(), outcome: "accepted".into(), reason: None })
                .collect();
            relays.extend(outcome.rejected.iter().map(|(url, reason)| BeaconRelayOutcome {
                url: url.clone(),
                outcome: "rejected".into(),
                reason: Some(reason.clone()),
            }));
            BeaconReport { last_attempt_at: now, last_success_at: now, relays, last_error: None }
        }
        Err(msg) => BeaconReport {
            last_attempt_at: now,
            last_success_at: prev.last_success_at,
            relays: prev.relays.clone(),
            last_error: Some(msg.to_string()),
        },
    }
}

/// Build + publish a status-only presence beacon: a signed kind-11111 event carrying only
/// freshness/expiry — no node key, no dialable address (transfer moved to Mascara). The reader only
/// checks signature + author-pin + freshness for online status. Returns the per-relay
/// [`PublishOutcome`] so the caller can surface beacon health (devtest #9).
pub(crate) async fn publish_presence(client: &RelayClient, identity: &Identity) -> Result<PublishOutcome> {
    let event = build_binding(identity, unix_now(), PRESENCE_TTL_SECS)
        .map_err(|e| anyhow!("build presence beacon: {e}"))?;
    client.publish(&event).await.map_err(|e| anyhow!("publish presence: {e}"))
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
    relay: SharedRelay,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    wakeups: std::sync::Arc<std::sync::atomic::AtomicU64>,
    beacon: SharedBeaconState,
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

        // M12 W1: ride the persistent shared client (one cheap publish, not a reconnect). Never
        // disconnect — the client lives for the session.
        let now = unix_now();
        match crate::net::client(&id, &store, &relay).await {
            Ok(client) => match publish_presence(&client, &id).await {
                Ok(outcome) => {
                    let prev = beacon.read().await.clone();
                    *beacon.write().await = record_outcome(&prev, Ok(&outcome), now);
                }
                Err(e) => {
                    tracing::debug!("presence publish failed: {e}");
                    let prev = beacon.read().await.clone();
                    *beacon.write().await = record_outcome(&prev, Err(&e.to_string()), now);
                }
            },
            Err(e) => {
                tracing::debug!("presence: no relay this cycle ({e})");
                let prev = beacon.read().await.clone();
                *beacon.write().await =
                    record_outcome(&prev, Err(&format!("no relay this cycle: {e}")), now);
            }
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
        let relay = crate::net::new_shared();
        let wakeups = Arc::new(AtomicU64::new(0));
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        let beacon: SharedBeaconState = Arc::default();
        let handle = tokio::spawn(run_presence_loop(
            identity,
            store,
            relay,
            cancel_rx,
            Arc::clone(&wakeups),
            beacon,
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        let woke = wakeups.load(Ordering::Relaxed);
        assert!(woke < 100, "idle presence loop woke {woke} times in 300ms — busy-spinning?");
    }

    /// Default report reads as "never attempted" (devtest #9: the panel must not claim a beacon
    /// that never fired).
    #[test]
    fn beacon_report_default_is_never_attempted() {
        let report = BeaconReport::default();
        assert_eq!(report.last_attempt_at, 0);
        assert_eq!(report.last_success_at, 0);
        assert!(report.relays.is_empty());
        assert!(report.last_error.is_none());
    }

    /// Ok with a mixed accept/reject outcome maps each relay to its own row and bumps both
    /// timestamps, clearing any stale error.
    #[test]
    fn record_outcome_ok_maps_mixed_relays() {
        let prev = BeaconReport {
            last_attempt_at: 10,
            last_success_at: 10,
            relays: vec![],
            last_error: Some("stale error".into()),
        };
        let outcome = PublishOutcome {
            accepted: vec!["wss://a".into()],
            rejected: vec![("wss://b".into(), "rate-limited".into())],
        };
        let got = record_outcome(&prev, Ok(&outcome), 20);

        assert_eq!(got.last_attempt_at, 20);
        assert_eq!(got.last_success_at, 20);
        assert!(got.last_error.is_none());
        assert_eq!(got.relays.len(), 2);
        let accepted = got.relays.iter().find(|r| r.url == "wss://a").unwrap();
        assert_eq!(accepted.outcome, "accepted");
        assert!(accepted.reason.is_none());
        let rejected = got.relays.iter().find(|r| r.url == "wss://b").unwrap();
        assert_eq!(rejected.outcome, "rejected");
        assert_eq!(rejected.reason.as_deref(), Some("rate-limited"));
    }

    /// Err updates the attempt timestamp + error, but preserves the last-known-good success time
    /// and relay rows — a transient failure must not blank a previously healthy panel.
    #[test]
    fn record_outcome_err_preserves_last_known_good() {
        let prev = BeaconReport {
            last_attempt_at: 10,
            last_success_at: 10,
            relays: vec![BeaconRelayOutcome {
                url: "wss://a".into(),
                outcome: "accepted".into(),
                reason: None,
            }],
            last_error: None,
        };
        let got = record_outcome(&prev, Err("no relay this cycle: pool empty"), 30);

        assert_eq!(got.last_attempt_at, 30);
        assert_eq!(got.last_success_at, 10);
        assert_eq!(got.relays, prev.relays);
        assert_eq!(got.last_error.as_deref(), Some("no relay this cycle: pool empty"));
    }
}
