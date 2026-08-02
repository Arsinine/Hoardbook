//! Presence: a status-only online beacon. It carries **no dialable address and no node key** — it is
//! purely a freshness signal so peers can see you're recently online.
//!
//! **That rule survived M18 and got MORE load-bearing, not less** (2026-07-26 ruling). It used to
//! follow from "Hoardbook has no transport at all". It no longer does: a manifest plane exists
//! (INV-4′), and this node has a stable, dialable QUIC identity. The reason presence still carries no
//! address is now the original one, standing on its own — **a public, always-on `npub`→endpoint map
//! is an IP-harvesting surface** (the H4/MT2 hole). An address reaches a peer through a *sealed
//! ticket in a DM*, issued per approved request, and never through a broadcast event.
//!
//! `presence_carries_no_address_or_node_key` is the behavioural guard, and it stays green.
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

/// The retry backoff schedule for a FAILED beacon cycle (W1, 2026-08-02). A successful cycle waits
/// the normal `PRESENCE_REFRESH_SECS` (300 s) cadence; a failed cycle retries fast — inside the 600 s
/// online window instead of after the full 300 s — with a bounded, increasing backoff so a transient
/// relay flap self-heals before two missed windows read as "offline". `retry_idx` is the count of
/// consecutive prior failures (0 = first failure this streak).
const RETRY_BACKOFF_SECS: [u64; 4] = [15, 30, 60, 120];

/// Pure delay selector — testable without a relay. Success → normal cadence; failure → the backoff
/// step for this streak position, clamped to the last (longest) step once the schedule is exhausted.
pub(crate) fn next_delay(succeeded: bool, retry_idx: u32) -> Duration {
    if succeeded {
        return Duration::from_secs(PRESENCE_REFRESH_SECS);
    }
    let idx = (retry_idx as usize).min(RETRY_BACKOFF_SECS.len() - 1);
    Duration::from_secs(RETRY_BACKOFF_SECS[idx])
}

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
/// freshness/expiry — no node key, no dialable address (an address rides a sealed ticket, never a
/// broadcast beacon — see the module header). The reader only checks signature + author-pin +
/// freshness for online status. Returns the per-relay
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
    let mut retry_idx: u32 = 0;
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

        // Snapshot the identity (clone the secp256k1 key) without holding the lock across the
        // network call.
        let snapshot = {
            let guard = identity.read().await;
            guard.as_ref().map(|id| id.identity.clone())
        };
        let now = unix_now();
        let Some(id) = snapshot else {
            // Record the skip (2026-08-02). This branch used to `continue` silently, leaving the
            // report at its `Default` — indistinguishable from "the loop never ran at all", which is
            // the one state the panel cannot diagnose its way out of. The cadence is deliberately
            // unchanged: no identity is not a relay failure, so it waits the normal 300 s rather
            // than entering the failure backoff.
            let prev = beacon.read().await.clone();
            *beacon.write().await =
                record_outcome(&prev, Err("no identity loaded — nothing to publish"), now);
            delay = Duration::from_secs(PRESENCE_REFRESH_SECS);
            retry_idx = 0;
            continue;
        };

        // M12 W1: ride the persistent shared client (one cheap publish, not a reconnect). Never
        // disconnect — the client lives for the session.
        let succeeded = match crate::net::client(&id, &store, &relay).await {
            Ok(client) => match publish_presence(&client, &id).await {
                Ok(outcome) => {
                    let prev = beacon.read().await.clone();
                    *beacon.write().await = record_outcome(&prev, Ok(&outcome), now);
                    true
                }
                Err(e) => {
                    tracing::debug!("presence publish failed: {e}");
                    let prev = beacon.read().await.clone();
                    *beacon.write().await = record_outcome(&prev, Err(&e.to_string()), now);
                    false
                }
            },
            Err(e) => {
                tracing::debug!("presence: no relay this cycle ({e})");
                let prev = beacon.read().await.clone();
                *beacon.write().await =
                    record_outcome(&prev, Err(&format!("no relay this cycle: {e}")), now);
                false
            }
        };

        // W1: a failed cycle retries fast (backoff inside the 600s window); a success resets to the
        // normal 300s cadence.
        delay = next_delay(succeeded, retry_idx);
        retry_idx = if succeeded { 0 } else { retry_idx.saturating_add(1) };
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

    /// A cycle with no identity must RECORD the skip, not `continue` silently (2026-08-02). Left
    /// unrecorded, the report stays at `Default` — byte-identical to "the loop never ran", so the
    /// Settings panel showed "not sent yet" for two unrelated faults and the app ships with no log
    /// subscriber to tell them apart. Time is paused, so the 15 s first-delay costs no wall clock.
    #[tokio::test(start_paused = true)]
    async fn presence_loop_records_a_skipped_cycle_when_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity: SharedIdentity = Arc::new(RwLock::new(None));
        let relay = crate::net::new_shared();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let beacon: SharedBeaconState = Arc::default();

        let handle = tokio::spawn(run_presence_loop(
            identity,
            store,
            relay,
            cancel_rx,
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&beacon),
        ));
        // Past the first delay, so exactly one cycle has run.
        tokio::time::sleep(Duration::from_secs(PRESENCE_FIRST_DELAY_SECS + 5)).await;
        let report = beacon.read().await.clone();
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        assert_ne!(report.last_attempt_at, 0, "a skipped cycle must still count as an attempt");
        assert!(
            report.last_error.as_deref().is_some_and(|e| e.contains("no identity")),
            "the skip must name itself, got {:?}",
            report.last_error
        );
        // Nothing was published, so the success clock must NOT move.
        assert_eq!(report.last_success_at, 0, "a skip is not a success");
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

    #[test]
    fn next_delay_success_is_normal_cadence() {
        assert_eq!(next_delay(true, 0), Duration::from_secs(PRESENCE_REFRESH_SECS));
        assert_eq!(next_delay(true, 5), Duration::from_secs(PRESENCE_REFRESH_SECS));
    }

    #[test]
    fn next_delay_first_failure_retries_inside_the_window() {
        // The P4 property: a failed cycle does NOT wait the full 300s cadence.
        let d = next_delay(false, 0);
        assert!(d < Duration::from_secs(PRESENCE_REFRESH_SECS), "first retry must beat the 300s cadence");
        assert!(d > Duration::from_secs(0));
        assert!(d < Duration::from_secs(600), "and land inside the 600s online window");
    }

    #[test]
    fn next_delay_backoff_is_monotonic_and_clamped() {
        let d0 = next_delay(false, 0);
        let d1 = next_delay(false, 1);
        let d2 = next_delay(false, 2);
        assert!(d0 <= d1 && d1 <= d2, "backoff increases");
        // Exhausted schedule clamps to the last step, never grows unbounded.
        assert_eq!(next_delay(false, 99), next_delay(false, 3));
    }
}
