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
pub(crate) const PRESENCE_FIRST_DELAY_SECS: u64 = 15;

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

/// Test-only, one-shot panic injection into the publish cycle — the fixture for the 2026-08-03
/// root cause (a panic inside `RelayClient::connect` during the cold-launch relay flap killed the
/// whole loop silently). One-shot (swap) so only the arming test is affected; no other presence
/// test loads an identity, so no other test's loop can reach the injection point.
#[cfg(test)]
pub(crate) mod test_panic_injection {
    use std::sync::atomic::{AtomicBool, Ordering};
    pub(crate) static ARMED: AtomicBool = AtomicBool::new(false);
    pub(crate) fn fire() {
        if ARMED.swap(false, Ordering::SeqCst) {
            panic!("injected cycle panic (cold-launch connect panic class)");
        }
    }
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
    /// v0.12.10 diagnostic: the wakeup counter copied from the loop's `wakeups` AtomicU64 at the
    /// last state write. Proves the task is being polled (a frozen report with a rising count
    /// means the loop is alive but stuck in an await), and lets the panel distinguish "loop wedged
    /// in an await" from "loop was never spawned" — both read as `last_attempt_at == 0` without this.
    pub loop_wakeups: u64,
    /// v0.12.10 diagnostic: a breadcrumb written BEFORE each await in the cycle, so if an await
    /// never returns the panel still shows exactly where it wedged. Stages (exact strings):
    /// `"loop-started"` (once, before the loop — proves the task got polled at all), `"sleeping"`
    /// (before the select), `"snapshotting-identity"`, `"acquiring-client"`, `"publishing"`,
    /// `"idle"` (cycle complete). Plain RwLock writes — the instrument itself, not its log.
    pub stage: String,
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
            BeaconReport {
                last_attempt_at: now,
                last_success_at: now,
                relays,
                last_error: None,
                // Diagnostic fields carry forward from `prev` — `record_outcome` is pure and
                // knows nothing about the loop's wakeup counter or stage breadcrumb. The loop
                // writes those via `set_stage` around this call; carrying them stops a fresh Ok
                // from blanking the trail mid-cycle.
                loop_wakeups: prev.loop_wakeups,
                stage: prev.stage.clone(),
            }
        }
        Err(msg) => BeaconReport {
            last_attempt_at: now,
            last_success_at: prev.last_success_at,
            relays: prev.relays.clone(),
            last_error: Some(msg.to_string()),
            loop_wakeups: prev.loop_wakeups,
            stage: prev.stage.clone(),
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

/// v0.12.10 diagnostic: write the stage breadcrumb + wakeup count into the shared beacon state
/// BEFORE an await, so if the await never returns the panel still shows where the loop wedged.
/// These are plain RwLock writes that provably work — the stage field IS the instrument, not its
/// log. Reads `wakeups` once (Relaxed — a snapshot is sufficient; exactness is not required to
/// distinguish "frozen" from "rising").
async fn set_stage(beacon: &SharedBeaconState, wakeups: &std::sync::atomic::AtomicU64, stage: &str) {
    let w = wakeups.load(std::sync::atomic::Ordering::Relaxed);
    let mut guard = beacon.write().await;
    guard.stage = stage.to_string();
    guard.loop_wakeups = w;
}

/// One publish cycle: acquire the shared client, publish the beacon, record the outcome. Runs in
/// its own task (spawned by the loop) so a panic anywhere inside — client connect included — is
/// contained to the cycle. Owns its arguments for the `'static` spawn bound; the Arcs/DataStore
/// are cheap clones of the loop's handles.
async fn publish_cycle(
    id: Identity,
    store: DataStore,
    relay: SharedRelay,
    beacon: SharedBeaconState,
    wakeups: std::sync::Arc<std::sync::atomic::AtomicU64>,
    now: u64,
) -> bool {
    #[cfg(test)]
    test_panic_injection::fire();
    match crate::net::client(&id, &store, &relay).await {
        Ok(client) => {
            set_stage(&beacon, &wakeups, "publishing").await;
            match publish_presence(&client, &id).await {
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
            }
        }
        Err(e) => {
            tracing::debug!("presence: no relay this cycle ({e})");
            let prev = beacon.read().await.clone();
            *beacon.write().await =
                record_outcome(&prev, Err(&format!("no relay this cycle: {e}")), now);
            false
        }
    }
}

/// Render a `JoinError`'s panic payload for the beacon report. Cancellation can't happen here —
/// the cycle handle is awaited, never aborted — but is rendered rather than unwrapped.
fn panic_message(e: tokio::task::JoinError) -> String {
    if !e.is_panic() {
        return "cycle task cancelled".into();
    }
    let payload = e.into_panic();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".into()
    }
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
    // QURATOR-66: one INFO line on spawn so a log shows the loop was created at all. The fifth
    // presence report root-caused a loop that died silently in a detached task — a bare "loop
    // started" line is the cheapest possible proof-of-life in the log a user pastes.
    tracing::info!(first_delay_secs = PRESENCE_FIRST_DELAY_SECS, "presence loop: started");
    // v0.12.10 diagnostic: prove the task got polled at all BEFORE the first await. If the loop is
    // never spawned, or spawned but never polled, the stage stays "" — distinguishable from every
    // in-cycle stage below.
    set_stage(&beacon, &wakeups, "loop-started").await;
    loop {
        wakeups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Diagnostic: record that we are about to await the select (sleep or cancel). If neither
        // future ever resolves, the panel shows "sleeping" — the wedge is in the runtime, not the
        // publish path.
        set_stage(&beacon, &wakeups, "sleeping").await;
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::info!("presence loop: cancelled (shutdown)");
                    break;
                }
            }
        }

        // Snapshot the identity (clone the secp256k1 key) without holding the lock across the
        // network call.
        set_stage(&beacon, &wakeups, "snapshotting-identity").await;
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
            set_stage(&beacon, &wakeups, "idle").await;
            delay = Duration::from_secs(PRESENCE_REFRESH_SECS);
            retry_idx = 0;
            continue;
        };

        // M12 W1: ride the persistent shared client (one cheap publish, not a reconnect). Never
        // disconnect — the client lives for the session.
        set_stage(&beacon, &wakeups, "acquiring-client").await;
        // v0.12.11 (fifth presence report, root-caused 2026-08-03): the cycle body runs in its OWN
        // task so a panic inside it — observed live inside `RelayClient::connect` during the
        // cold-launch relay flap — kills the cycle, not the loop. Pre-fix, the whole detached loop
        // task died silently (no console, panics bypass tracing) and presence was dead for the
        // session with the panel frozen at "acquiring-client". Now the panic is recorded like any
        // other failed cycle and the backoff retry self-heals once the flap clears.
        let npub_disp = crate::logging::trunc_npub(&id.npub());
        let cycle = tokio::spawn(publish_cycle(
            id,
            store.clone(),
            relay.clone(),
            Arc::clone(&beacon),
            Arc::clone(&wakeups),
            now,
        ));
        let succeeded = match cycle.await {
            Ok(succeeded) => succeeded,
            Err(e) => {
                let msg = panic_message(e);
                // QURATOR-66: the panic-and-restart signal. This loop died silently in a detached
                // task for four releases; ERROR here (with the panic payload) is the one line a log
                // MUST show to explain "presence went dead then came back".
                tracing::error!("presence cycle aborted: {msg}");
                let prev = beacon.read().await.clone();
                *beacon.write().await =
                    record_outcome(&prev, Err(&format!("cycle panicked: {msg}")), now);
                false
            }
        };

        // QURATOR-66: the cycle outcome at DEBUG. A success is once per ~5 min (not a hot loop),
        // so this is a handful of lines per session — readable at the default level. The truncated
        // npub identifies which identity published without leaking the full npub (INV-2).
        if succeeded {
            tracing::debug!(
                npub = %npub_disp,
                "presence cycle: beacon published"
            );
        } else {
            tracing::debug!(
                npub = %npub_disp,
                "presence cycle: failed (will retry with backoff)"
            );
        }

        // W1: a failed cycle retries fast (backoff inside the 600s window); a success resets to the
        // normal 300s cadence.
        delay = next_delay(succeeded, retry_idx);
        retry_idx = if succeeded { 0 } else { retry_idx.saturating_add(1) };
        // Diagnostic: the cycle completed (record_outcome ran). Written last so a panel reading
        // "idle" between cycles knows the previous await returned and the loop is sleeping, not
        // wedged mid-publish.
        set_stage(&beacon, &wakeups, "idle").await;
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
            ..Default::default()
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
            ..Default::default()
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

    // ── v0.12.10 diagnostic build: loop-liveness breadcrumbs ────────────────────────
    //
    // The fifth presence-class bug: on two packaged-Windows machines the loop never records a
    // single cycle, yet every await is provably bounded. The stage breadcrumb is written BEFORE
    // each await so a wedged await still shows where it stopped. These pin that behavior.

    /// The stage breadcrumb is non-empty as soon as the task is polled — the `loop-started` write
    /// fires before the first select, then is immediately overwritten by `sleeping` on the same poll
    /// batch (the task parks at the select because the 15 s sleep hasn't elapsed). An empty stage
    /// after spawn is the wedge signature: the task was never polled at all. Paused clock ⇒ the
    /// first delay never elapses, so no cycle logic runs — this isolates the "task got polled"
    /// property from the "cycle completed" property.
    #[tokio::test(start_paused = true)]
    async fn stage_written_before_first_cycle() {
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
        // Advance 1 s of paused time — enough for the runtime to poll the task once (writing
        // loop-started → sleeping), but well under the 15 s first delay so no cycle runs.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stage = beacon.read().await.stage.clone();
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        assert_ne!(
            stage, "",
            "stage must be non-empty after the task is polled; empty means the task was never \
             polled at all (the wedge hypothesis the diagnostic build exists to confirm or refute)"
        );
        // The only stages reachable before the first delay elapses are loop-started and sleeping
        // (both written before the select). Anything else means the breadcrumb logic is wrong.
        assert!(
            stage == "loop-started" || stage == "sleeping",
            "pre-cycle stage must be loop-started or sleeping; got {stage:?}"
        );
    }

    /// After one full cycle (no-identity skip), the stage must still be non-empty — proving the
    /// breadcrumb writes are wired through the whole cycle, not just the entry. The steady state
    /// between cycles is `sleeping` (the loop parked at the next select), because `idle` is written
    /// last and then immediately overwritten when the loop iterates. A stage stuck at
    /// `snapshotting-identity` would mean the loop wedged in that await. Paused clock ⇒ no wall cost.
    #[tokio::test(start_paused = true)]
    async fn stage_progresses_through_a_cycle() {
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
        // Past the first delay → exactly one cycle (no-identity skip) has completed.
        tokio::time::sleep(Duration::from_secs(PRESENCE_FIRST_DELAY_SECS + 5)).await;
        let report = beacon.read().await.clone();
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        // The cycle completed (record_outcome ran for the skip), so the stage must be non-empty —
        // the breadcrumb writes ran through the whole cycle. An empty stage would mean the
        // diagnostic writes only fire on the entry path, not the cycle body.
        assert_ne!(
            report.stage, "",
            "stage must be non-empty after a completed cycle; empty means the breadcrumb writes \
             are not wired through the cycle body"
        );
        // The skip cycle still recorded an attempt (existing behavior, unchanged).
        assert_ne!(report.last_attempt_at, 0);
    }

    /// `loop_wakeups` is surfaced from the loop's AtomicU64 into the report — a rising count on a
    /// frozen report proves the loop is alive but stuck in an await (vs. never spawned at all).
    #[tokio::test(start_paused = true)]
    async fn loop_wakeups_surfaced_into_report() {
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
        tokio::time::sleep(Duration::from_secs(PRESENCE_FIRST_DELAY_SECS + 5)).await;
        let report = beacon.read().await.clone();
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        // After one cycle the wakeup counter must have been copied into the report at least once
        // (set_stage writes it on every stage transition). Zero would mean set_stage never ran.
        assert!(
            report.loop_wakeups > 0,
            "loop_wakeups must be surfaced into the report after a cycle; got 0 — \
             set_stage never ran (the diagnostic instrument itself failed to fire)"
        );
    }

    /// 2026-08-03, fifth presence report, root-caused from the v0.12.10 log + panel: a panic inside
    /// the cycle body (observed live inside `RelayClient::connect` during the cold-launch relay
    /// flap) must kill the CYCLE, not the loop. The panicking cycle records "cycle panicked: …" in
    /// the report and the loop keeps running — the backoff retry lands after the flap and
    /// self-heals. Red on the pre-fix code: the detached loop task dies, nothing is recorded, and
    /// presence is silently dead for the whole session (panel frozen at "acquiring-client").
    #[tokio::test(start_paused = true)]
    async fn a_panicking_cycle_is_recorded_and_the_loop_survives() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity: SharedIdentity = Arc::new(RwLock::new(Some(
            crate::identity_state::AppIdentity::generate(),
        )));
        let relay = crate::net::new_shared();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let beacon: SharedBeaconState = Arc::default();

        test_panic_injection::ARMED.store(true, Ordering::SeqCst);
        let handle = tokio::spawn(run_presence_loop(
            identity,
            store,
            relay,
            cancel_rx,
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&beacon),
        ));
        // Past the first delay: cycle 1 fires, hits the injected panic. The loop must record it
        // and wrap into iteration 2 (parked on the backoff timer) — it must NOT die. The test
        // reads the report BEFORE the backoff expires, so cycle 2 never runs and no real network
        // is ever touched.
        tokio::time::sleep(Duration::from_secs(PRESENCE_FIRST_DELAY_SECS + 5)).await;
        let report = beacon.read().await.clone();
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        let err = report.last_error.as_deref().unwrap_or("");
        assert!(
            err.contains("cycle panicked"),
            "a panicking cycle must be recorded in the report; got last_error={err:?} \
             (empty means the loop task died with the cycle — the pre-fix behavior)"
        );
        assert_eq!(
            report.stage, "sleeping",
            "the loop must survive the panicking cycle and park on the retry timer; \
             stage {:?} means it never wrapped into the next iteration",
            report.stage
        );
        assert_eq!(
            report.loop_wakeups, 2,
            "iteration 2 must have started after the panicked cycle 1"
        );
    }

    /// `panic_message` must extract both payload shapes a Rust panic carries (`&str` from
    /// `panic!("literal")`, `String` from `panic!("{x}")`) — the report string is the only place
    /// the packaged app surfaces the panic, so "non-string panic payload" on a common shape would
    /// re-blind the panel.
    #[tokio::test]
    async fn panic_message_extracts_both_payload_shapes() {
        let e = tokio::spawn(async { panic!("plain literal") }).await.unwrap_err();
        assert_eq!(panic_message(e), "plain literal");

        let e = tokio::spawn(async {
            let detail = "formatted";
            panic!("with {detail}")
        })
        .await
        .unwrap_err();
        assert_eq!(panic_message(e), "with formatted");
    }

    /// `record_outcome` must carry the diagnostic fields (stage, loop_wakeups) forward from `prev`
    /// so a fresh Ok/Err result does not blank the trail mid-cycle. The loop writes them via
    /// set_stage around the record_outcome call; record_outcome itself is pure and must preserve them.
    #[test]
    fn record_outcome_preserves_diagnostic_fields() {
        let prev = BeaconReport {
            last_attempt_at: 10,
            last_success_at: 10,
            relays: vec![],
            last_error: None,
            loop_wakeups: 42,
            stage: "publishing".into(),
        };
        let got = record_outcome(&prev, Err("transient"), 20);
        assert_eq!(got.loop_wakeups, 42, "loop_wakeups must carry forward");
        assert_eq!(got.stage, "publishing", "stage must carry forward");
    }
}
