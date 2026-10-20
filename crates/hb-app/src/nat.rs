//! QURATOR-68 — wire the pure NAT classifier (`net::classify_nat`) to a real reading at launch.
//!
//! `net.rs` ships `NatClassification` + `classify_nat(local, mapped)` as pure, dead-code-marked
//! functions: address → class, no network I/O. This module is the wiring half. At startup it binds
//! a **dial-only** iroh endpoint (owner ruling ③ of 2026-07-31 — never the listening endpoint),
//! reads the LAN-facing local address via the OS routing table and the relay-observed mapped
//! address out of iroh's net_report, runs the pure classifier, stores the result in a shared slot,
//! and writes **one** INFO line to the log carrying the classification token and never the address.
//!
//! ## Privacy — the invariant that must not go wrong
//!
//! The classification carries no address data in its variants or their `Debug`/`Display` output (see
//! `net::NatClassification`). The discovered mapped address is **local-display-only** — it must never
//! be published, never enter a presence event or listing, never leave the machine, and never be
//! written to the log. The log line emitted here carries the CLASSIFICATION (`nat`, `cgnat`,
//! `no-nat`, `unknown`), never the IP. Writing the mapped IP would mean any user pasting a log has
//! published their own address — the H4/MT2 harvest shape this project already closed
//! (`presence_carries_no_address_or_node_key`).
//!
//! ## Why a dial-only endpoint is sufficient
//!
//! `classify_nat(local, mapped)` needs a local address and a relay-observed mapped address. A
//! dial-only endpoint obtains both: the mapped address comes from iroh's net_report (`global_v4`),
//! and the local address comes from the OS routing table via a no-send `connect()` on a UDP socket
//! (the OS picks the source interface for a routable target; no packets leave the machine). There
//! is no capability reason to reach for the listening variant — and owner ruling ③ forbids it,
//! because a listening endpoint persists a stable node identity and answers anyone who holds it,
//! making every asker a probeable presence oracle. See `transport::bind_client_endpoint`'s
//! docstring for the ruling in full.
//!
//! ## Why a one-shot task, not a parallel lifecycle
//!
//! The manifest plane's `transport_state::SharedEndpoint` is identity-keyed (owner_npub + generation
//! counter) because it serves manifests signed by a snapshot of the signing key. The NAT reading has
//! no such requirement — it wants to bind, read, classify, log, and drop. So this is a one-shot
//! probe, not a parallel endpoint lifecycle: the endpoint is closed at the end of the task, and the
//! only thing kept around is the classification token in `SharedNatClassification` for the UI to
//! read on demand. If the manifest plane is already bound (DialOnly or Listen), the classifier
//! reuses that endpoint handle instead of binding a second one — the ruling-compliant reuse path.

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::{Endpoint, Watcher};
use tokio::sync::RwLock;

use crate::net::NatClassification;

/// How long to wait for iroh's net_report to populate before classifying with whatever it has.
///
/// iroh's `Endpoint::bind()` returns once relay discovery, DNS, and the initial relay handshake
/// resolve; the net_report watcher continues to populate asynchronously as probes land (4 relays
/// probed in parallel). Under cold launch or a flapping relay, the first report can take several
/// seconds. This is deliberately shorter than iroh's full probe timeout (`NET_REPORT_TIMEOUT`,
/// ~30 s) because we want *a* reading on the startup log line promptly, and the classifier is
/// designed to give a useful answer from a partial report (RFC 1918 private local alone ⇒ BehindNat;
/// a cold/offline start ⇒ Unknown, never a confident negative). The UI surfaces the latest reading on
/// demand, and a user can re-probe by reloading Settings.
// QURATOR-389: in test builds the live probe body is neutered (see probe_and_close), which leaves
// the live-only chain below unreferenced — hence the test-only dead_code allowance on each.
#[cfg_attr(test, allow(dead_code))]
pub const NET_REPORT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The target `connect()` uses to let the OS pick a source interface. Any routable public address
/// works — the socket never sends, and the port is irrelevant. `1.1.1.1:80` is the conventionally
/// used target (Cloudflare DNS) in STUN-style local-address probes. No packet leaves the machine:
/// `connect` on a UDP socket only populates the kernel routing decision.
#[cfg_attr(test, allow(dead_code))]
const LOCAL_ADDR_PROBE_TARGET: std::net::SocketAddr = std::net::SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1)),
    80,
);

/// Managed state: the most recent NAT classification, or `None` before the first probe completes.
/// Mirrors `SharedBeaconState`. Read by the `nat_classification` Tauri command from the UI on demand.
pub type SharedNatClassification = Arc<RwLock<Option<NatClassification>>>;

/// A fresh, empty shared-classification slot (filled by the startup probe).
pub fn new_shared_classification() -> SharedNatClassification {
    Arc::new(RwLock::new(None))
}

/// The LAN-facing source address the OS would use to reach a public target. Pure OS routing-table
/// lookup — `connect` on a UDP socket populates the kernel's source-interface decision without
/// sending any packets. This is the standard STUN-side technique for resolving the local half of a
/// NAT classification without pulling in a netlink/interface-enumeration dependency.
///
/// Returns the first non-loopback, non-unspecified IPv4 the OS offers. On a host with no routable
/// default (air-gapped / DNS-only), returns `None` — and `classify_nat` then yields `Unknown` from
/// that (or `BehindNat` if a private local is observed some other way), never a confident negative.
#[cfg_attr(test, allow(dead_code))]
fn lan_source_addr() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // Ignore failure: on a host with no default route this returns Err, and we fall through to None.
    let _ = sock.connect(LOCAL_ADDR_PROBE_TARGET);
    match sock.local_addr().ok()? {
        std::net::SocketAddr::V4(v4) => {
            let ip = IpAddr::V4(*v4.ip());
            if ip.is_loopback() || ip.is_unspecified() {
                None
            } else {
                Some(ip)
            }
        }
        _ => None,
    }
}

/// Pull the relay-observed mapped address out of iroh's net_report. Prefers IPv4 (the CGNAT/RFC 1918
/// signals are IPv4-only), falls back to IPv6. Returns `None` if the report has not yet populated a
/// global address — which is a genuinely undecided case, NOT a confident negative.
#[cfg_attr(test, allow(dead_code))]
fn mapped_addr_from_report(report: &iroh::unstable_net_report::NetReport) -> Option<IpAddr> {
    if let Some(global_v4) = report.global_v4 {
        return Some(IpAddr::V4(*global_v4.ip()));
    }
    if let Some(global_v6) = report.global_v6 {
        return Some(IpAddr::V6(*global_v6.ip()));
    }
    None
}

/// Emit the one INFO line for a classification. **This is the SINGLE logging site** — production
/// (`classify_and_log`) and the privacy guard in `tests` both call it, so the guard tests the line
/// production actually emits rather than a hand-copied lookalike.
///
/// That distinction is load-bearing, not stylistic. The guard originally re-emitted its own copy of
/// this `tracing::info!`, which meant adding `mapped = ?mapped` to production left the guard GREEN —
/// verified by mutation on 2026-08-13: **7/7 passed while production leaked the mapped IP.** A
/// privacy control that cannot observe the privacy violation it names is a decoration. CLAUDE.md §9:
/// *a round-trip test must END WHERE PRODUCTION ENDS.* Keep both callers pointed here; if you inline
/// this back into `classify_and_log`, you silently re-open that hole.
///
/// **Never add an address-bearing field to this line.** The mapped and local IPs are local-display-
/// only; a user pasting a log would otherwise publish their own address (the H4/MT2 harvest shape).
pub(crate) fn emit_classification_log(class: NatClassification, varies: Option<bool>) {
    if varies == Some(true) {
        tracing::info!(
            nat = %class.as_log_token(),
            symmetric = true,
            "NAT: {} detected (mapped address varies by destination)",
            class.as_log_token()
        );
    } else {
        tracing::info!(
            nat = %class.as_log_token(),
            "NAT: {} detected",
            class.as_log_token()
        );
    }
}

/// Classify, store, and log the NAT reading from a bound endpoint. Reads the endpoint's net_report
/// for the relay-observed mapped address and the OS routing table for the LAN source address, runs
/// the pure classifier, publishes the result to `shared`, and emits **one** INFO line carrying the
/// classification token (never the address).
///
/// Returns the classification so a caller that already holds an endpoint can use the value directly.
/// Errors are logged and returned but never panic — a classification failure leaves the slot at its
/// prior value (likely `None` ⇒ Unknown on the first run), which is the correct "not yet
/// determined" surface.
#[cfg_attr(test, allow(dead_code))]
pub async fn classify_and_log(
    endpoint: &Endpoint,
    shared: &SharedNatClassification,
) -> Result<NatClassification> {
    // Read the net_report watcher with a bounded wait. `initialized()` resolves immediately if a
    // report is already available; otherwise it waits for the first `Some`. We wrap it so a stuck
    // probe does not hang startup — the classifier gives a useful answer from a partial report.
    // On timeout we fall back to whatever the watcher last observed (which may be `None`).
    let report = {
        let mut watcher = endpoint.net_report();
        match tokio::time::timeout(NET_REPORT_WAIT, watcher.initialized()).await {
            Ok(report) => Some(report),            // a real report came in (disconnected ⇒ Pending forever, hit by the timeout)
            Err(_) => watcher.peek().clone(),      // timed out — use whatever it last saw
        }
    };
    let mapped = report.as_ref().and_then(mapped_addr_from_report);
    let local = lan_source_addr().context("no LAN source address from the OS routing table")?;
    let class = crate::net::classify_nat(local, mapped);

    // Symmetric-NAT signal: if the report says the mapped address varies by destination, that is
    // the strongest indicator of symmetric NAT (a cone NAT maps consistently). We do NOT add a new
    // variant for this (the enum is `#[non_exhaustive]` and leaves room) — instead we log it as a
    // diagnostic detail alongside the classification. The `as_log_token` output stays the single
    // source of truth for the UI's token. RFC 6598 CGNAT dominates this signal if both fire, because
    // a CGNAT face is structurally also a symmetric NAT and the stronger claim wins.
    let varies = report.as_ref().and_then(|r| r.mapping_varies_by_dest_ipv4);

    // Store the classification BEFORE the log line, so the UI command reading the slot after the
    // log line lands always sees a value at least as fresh as what the log announced.
    *shared.write().await = Some(class);

    // ONE INFO line carrying the classification token, never the address. This is the line a user
    // pasting a log contributes to a support thread — "nat: cgnat" carries all the debugging value
    // without publishing the reporter's IP. The `symmetric` detail is logged as a bare boolean
    // signal, still no address data.
    //
    // INV ("presence carries no address or node key"): the mapped/local IPs are NEVER in this line.
    // The leak guard test in this file reads the produced log back and asserts the mapped address
    // literal does not appear, red-green (a deliberate injection reds it).
    emit_classification_log(class, varies);
    Ok(class)
}

/// Bind a dial-only endpoint with `transport_key`, classify, log, store, and close the endpoint.
///
/// This is the one-shot probe spawned at startup. It uses [`crate::transport::bind_client_endpoint`]
/// (owner ruling ③ — never the listening variant) and the session's transport secret so no new
/// identity surface is introduced. The endpoint is closed at the end; no accept loop runs, no ALPN
/// is advertised, and the endpoint does not outlive this task.
///
/// If `existing` is `Some`, the probe reuses that already-bound endpoint handle instead of binding
/// a second one — the ruling-compliant reuse path when the manifest plane has already bound one.
pub async fn probe_and_close(
    transport_key: &[u8; 32],
    shared: &SharedNatClassification,
    existing: Option<Endpoint>,
) {
    // QURATOR-389: the live probe binds a real iroh endpoint and contacts relays (network I/O);
    // hb-app unit tests must never do that. This is the EXACT network seam, so the post-identity
    // tests observe INVOCATION here — under cfg(test) the body neuters to a counter increment
    // instead of a live bind. `let _ =` discards the parameters only the live body uses, keeping
    // the test build warning-free under `-D warnings`. Production runs the block below unchanged.
    #[cfg(test)]
    {
        let _ = (transport_key, existing);
        tests::record_probe_body_run(shared);
    }

    #[cfg(not(test))]
    {
        // Reuse path: the manifest plane (or a prior probe) already bound a dial-only endpoint. Cloning
        // an `Endpoint` is cheap (it's a handle to a shared inner state), and we do NOT close a reused
        // endpoint — the manifest plane owns its lifecycle.
        //
        // We split the borrow from the close: the classify call borrows the endpoint immutably, and
        // only an endpoint we bound (not a reused one) is closed afterwards.
        if let Some(endpoint) = existing.as_ref() {
            if let Err(e) = classify_and_log(endpoint, shared).await {
                tracing::warn!("NAT: classification failed: {e}");
            }
            // Reused endpoint — do NOT close. The manifest plane owns its lifecycle.
            return;
        }

        // Bind a fresh dial-only endpoint with the session transport key (owner ruling ③).
        let endpoint = match crate::transport::bind_client_endpoint(transport_key).await {
            Ok(ep) => ep,
            Err(e) => {
                tracing::warn!("NAT: could not bind a dial-only endpoint to classify: {e}");
                return;
            }
        };
        if let Err(e) = classify_and_log(&endpoint, shared).await {
            tracing::warn!("NAT: classification failed: {e}");
        }
        endpoint.close().await;
    }
}

// ---------------------------------------------------------------------------
// QURATOR-389 — schedule the probe when an identity appears mid-session
// ---------------------------------------------------------------------------

/// Single-flight flag for the NAT probe (QURATOR-389). Both entry points — the startup task in
/// `lib.rs` and the post-identity path — route through [`probe_after_identity`], and the
/// check-and-insert under one lock in `claim_probe` is what makes two overlapping attempts resolve to at most ONE
/// running probe. Released by the spawned body when the probe finishes, so a probe that failed to
/// classify (slot still empty) can be retried by a later identity event; a probe that DID classify
/// is held off by the slot check instead.
///
/// Keyed by the slot (its `Arc` address), not one process-wide flag: production manages exactly ONE
/// slot, so this is still "one probe per process", while each test's own slot is its own world and
/// concurrent tests can no longer refuse each other's probe through a shared flag.
static NAT_PROBE_IN_FLIGHT: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// The single-flight key of a slot: its shared allocation's address, stable for the slot's life.
fn slot_key(slot: &SharedNatClassification) -> usize {
    Arc::as_ptr(slot) as usize
}

fn probe_in_flight(key: usize) -> bool {
    NAT_PROBE_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()).contains(&key)
}

/// The pure go/no-go decision for scheduling a NAT probe (QURATOR-389). Probe only when the slot
/// holds no classification yet AND no probe is in flight. An already-classified slot means the
/// reading exists — re-probing on every identity command would re-bind a dial-only endpoint (and
/// contact relays) on every generate/import/restore. An in-flight probe means a reading is on its
/// way — racing a second one buys nothing but a second dial-only bind.
pub(crate) fn should_probe(slot_filled: bool, in_flight: bool) -> bool {
    !slot_filled && !in_flight
}

/// Atomic claim of the single-flight slot. `false` ⇒ another caller holds it; do not probe. The
/// check and the insert under ONE lock hold (not load-then-store) is the load-bearing part: two tasks racing through
/// [`should_probe`] at the same instant must not BOTH win.
fn claim_probe(key: usize) -> bool {
    let mut held = NAT_PROBE_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
    if held.contains(&key) {
        return false;
    }
    held.push(key);
    true
}

/// Release the single-flight slot — called by the spawned probe body, on every path.
fn release_probe(key: usize) {
    NAT_PROBE_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()).retain(|k| *k != key);
}

/// QURATOR-389 (owner devtest 2026-10-20: "A new user cannot determine its Network type") —
/// schedule the NAT probe once an identity becomes available mid-session.
///
/// The startup task (lib.rs) reads the identity once at spawn and early-returns when there is
/// none, so on a fresh install the Settings → Diagnostics "Network type" stayed "undetermined"
/// until restart. The identity commands (generate / import / restore, and get_identity's
/// load-from-disk branch) call this after `*identity.write().await = Some(..)`, and the startup
/// path calls it too — ONE helper, both paths, so the guard and single-flight logic cannot drift
/// between them (CLAUDE.md §9: hardened-path/unhardened-sibling drift is this repo's recurring
/// fault shape).
///
/// - Guard: [`should_probe`] — skip when the slot already holds a classification.
/// - Single-flight: `claim_probe` — never two probes concurrently, even when two identity
///   commands race; the loser drops, and a later identity event retries after the winner
///   releases.
/// - Reuse: reads the manifest plane's bound endpoint exactly as the startup path did, so an
///   already-bound endpoint is reused instead of a second dial-only bind.
/// - Fire-and-forget: the probe runs on a spawned task and never blocks the identity command's
///   return. Returns whether a probe was scheduled (tests observe this; commands ignore it).
pub async fn probe_after_identity(
    transport_key: &[u8; 32],
    nat_slot: &SharedNatClassification,
    endpoint: &crate::transport_state::SharedEndpoint,
) -> bool {
    let key = slot_key(nat_slot);
    let slot_filled = nat_slot.read().await.is_some();
    if !should_probe(slot_filled, probe_in_flight(key)) {
        return false;
    }
    if !claim_probe(key) {
        return false;
    }
    // Same reuse logic as the startup path: a manifest plane that already bound an endpoint
    // (outstanding ticket ⇒ Listen, or any future always-bind path) lends its handle instead of
    // taking a second dial-only bind.
    let existing = endpoint.read().await.bound.as_ref().map(|b| b.endpoint.clone());
    tracing::info!("NAT: probe scheduled after an identity became available");
    let transport_key = *transport_key;
    let slot = Arc::clone(nat_slot);
    tokio::spawn(async move {
        probe_and_close(&transport_key, &slot, existing).await;
        release_probe(key);
    });
    true
}

// pub(crate) under cfg(test) so the identity.rs command tests can observe the probe scheduler's
// per-slot test hooks (`record_probe_body_run` / `probe_body_runs`) across the crate.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::logging;

    // ── QURATOR-389 — the post-identity probe scheduler ────────────────────────────────────────

    /// Test-only invocation counter for the probe body (see the cfg(test) neuter in
    /// `probe_and_close`). pub(crate) so the identity.rs command tests can observe that the
    /// command actually scheduled the probe without performing any network I/O.
    ///
    /// Counted PER SLOT (keyed like the single-flight set), so a test observes only the probes
    /// scheduled against the slot it built — a concurrent identity test cannot move its count.
    #[cfg(test)]
    static TEST_PROBE_BODY_RUNS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

    #[cfg(test)]
    pub(crate) fn record_probe_body_run(slot: &SharedNatClassification) {
        TEST_PROBE_BODY_RUNS.lock().unwrap_or_else(|p| p.into_inner()).push(slot_key(slot));
    }

    /// How many neutered probe bodies have run against `slot`.
    #[cfg(test)]
    pub(crate) fn probe_body_runs(slot: &SharedNatClassification) -> usize {
        let key = slot_key(slot);
        TEST_PROBE_BODY_RUNS.lock().unwrap_or_else(|p| p.into_inner()).iter().filter(|k| **k == key).count()
    }


    /// QURATOR-389 regression pin: both inputs of the go/no-go decision are load-bearing. An
    /// already-classified slot means the reading exists (re-probing re-binds a dial-only endpoint
    /// for nothing); an in-flight probe means a reading is on its way (never two probes
    /// concurrently).
    // MUTATION (P-10): in `should_probe`, delete `!slot_filled && ` (leaving `!in_flight`) — this
    // test reds on the two slot-filled rows. The companion mutation deleting `&& !in_flight` reds
    // `probe_after_identity_is_single_flight..` below.
    #[test]
    fn should_probe_decision_table_pins_both_conditions() {
        let cases: [(bool, bool, bool, &str); 4] = [
            (
                false, false, true,
                "empty slot, idle — the fresh-install case this ticket fixes",
            ),
            (
                true, false, false,
                "slot already classified — the reading exists, a re-probe is a wasted dial-only bind",
            ),
            (
                false, true, false,
                "a probe is in flight — never two probes concurrently",
            ),
            (
                true, true, false,
                "both — classified wins, nothing to do",
            ),
        ];
        for (slot_filled, in_flight, want, why) in cases {
            assert_eq!(should_probe(slot_filled, in_flight), want, "case: {why}");
        }
    }

    /// QURATOR-389: the guard must reject BEFORE anything is claimed or spawned when the slot
    /// already holds a classification — otherwise every generate/import/restore would re-bind a
    /// dial-only endpoint and re-contact relays for a reading the app already has.
    // MUTATION (P-10): in `should_probe`, delete `!slot_filled && ` (leaving `!in_flight`) — this
    // test reds: the helper then claims and schedules a probe (the slot's probe-body count increments)
    // despite the filled slot.
    #[tokio::test]
    async fn probe_after_identity_skips_when_the_slot_already_holds_a_classification() {
        let slot = new_shared_classification();
        *slot.write().await = Some(NatClassification::Unknown);
        let endpoint = crate::transport_state::new_shared_endpoint();
        let before = probe_body_runs(&slot);

        let spawned = probe_after_identity(&[7u8; 32], &slot, &endpoint).await;

        assert!(!spawned, "a classified slot must not schedule another probe");
        assert_eq!(
            probe_body_runs(&slot),
            before,
            "no probe body may run for an already-classified slot"
        );
        assert!(
            !super::probe_in_flight(super::slot_key(&slot)),
            "the guard must reject before the single-flight claim is even taken"
        );
    }

    /// QURATOR-389: two overlapping schedule attempts must resolve to ONE probe. Under the
    /// current-thread test runtime the first call wins the claim and its spawned body has not run
    /// yet, so the second call must be refused while the claim is held; once the body releases the
    /// claim, the next identity event may retry (that retry path is what lets a FAILED
    /// classification — slot still empty — be re-attempted instead of wedging the scheduler).
    // MUTATION (P-10) — the in-flight guard is TWO checks (`should_probe`'s `!in_flight` and
    // `claim_probe`'s `held.contains`), so mutating either ALONE stays GREEN (seen 2026-10-20).
    // Mutate BOTH — `!slot_filled && !in_flight` → `!slot_filled` AND `if held.contains(&key)` →
    // `if false` — and this test reds at the second-attempt assertion (seen red 2026-10-20).
    #[tokio::test]
    async fn probe_after_identity_is_single_flight_two_overlapping_attempts_yield_one_probe() {
        let slot = new_shared_classification();
        let endpoint = crate::transport_state::new_shared_endpoint();

        assert!(
            probe_after_identity(&[7u8; 32], &slot, &endpoint).await,
            "the first attempt claims and schedules"
        );
        assert!(
            !probe_after_identity(&[7u8; 32], &slot, &endpoint).await,
            "the second overlapping attempt must be refused while the first holds the claim"
        );

        // Let the neutered (cfg(test)) body run and release the claim.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while probe_body_runs(&slot) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the scheduled probe body never ran"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        while super::probe_in_flight(super::slot_key(&slot)) {
            assert!(
                std::time::Instant::now() < deadline,
                "the probe body never released the single-flight claim"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Released claim + still-empty slot (the neutered body does not classify) ⇒ a retry is
        // allowed. This is the failed-classification retry path.
        assert!(
            probe_after_identity(&[7u8; 32], &slot, &endpoint).await,
            "after the claim releases, the next identity event may retry"
        );

        // Cleanup: let the retry's neutered body run and release this slot's claim.
        while super::probe_in_flight(super::slot_key(&slot)) {
            assert!(
                std::time::Instant::now() < deadline,
                "the retry's probe body never released the single-flight claim"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Build a test subscriber writing to `log_path`, returning a guard and a shared file handle.
    /// Mirrors `logging::tests::install_test_subscriber` but lives here so this module's privacy
    /// test is self-contained (the `hb-app` test suite runs in one process; we must not assume a
    /// particular test's subscriber is still installed).
    fn install_test_subscriber(
        log_path: &std::path::Path,
    ) -> (
        tracing::dispatcher::DefaultGuard,
        std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    ) {
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;
        struct FileMaker(std::sync::Arc<std::sync::Mutex<std::fs::File>>);
        impl<'a> MakeWriter<'a> for FileMaker {
            type Writer = FileWriter;
            fn make_writer(&'a self) -> Self::Writer {
                FileWriter(self.0.clone())
            }
        }
        struct FileWriter(std::sync::Arc<std::sync::Mutex<std::fs::File>>);
        impl std::io::Write for FileWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().map_err(|_| std::io::Error::other("poisoned"))?.write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.lock().map_err(|_| std::io::Error::other("poisoned"))?.flush()
            }
        }
        let file = std::fs::File::create(log_path).expect("create test log file");
        let shared = std::sync::Arc::new(std::sync::Mutex::new(file));
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("trace"))
            .with(logging::fmt_layer(FileMaker(shared.clone())));
        let guard = tracing::subscriber::set_default(subscriber);
        (guard, shared)
    }

    /// The log line emitted by `classify_and_log` must carry the CLASSIFICATION and not the mapped
    /// address. This drives the real logging path through a real subscriber into a real file, greped
    /// for the mapped-address literal. GREEN half.
    ///
    /// We can't easily build an iroh `Endpoint` in a unit test (it needs real sockets + relay
    /// discovery), so we drive the exact logging site in isolation: construct the classification
    /// we'd get from a CGNAT mapped address, emit the same shape of line `classify_and_log` emits,
    /// and assert the sentinel never appears. The integration suites exercise the real
    /// endpoint→report→classify path end-to-end.
    #[tokio::test]
    async fn log_line_carries_classification_not_address() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nat-privacy.log");
        let mapped: IpAddr = "100.64.42.99".parse().unwrap(); // RFC 6598 CGNAT, never to appear
        let local: IpAddr = "10.0.0.5".parse().unwrap();
        let class = crate::net::classify_nat(local, Some(mapped));
        assert_eq!(class, NatClassification::BehindCgnat, "test setup: expected CGNAT");
        {
            let (_guard, file_handle) = install_test_subscriber(&log_path);
            // Call PRODUCTION's logging site, not a copy of it. This is the whole point: an earlier
            // version of this test re-emitted its own `tracing::info!` with the same shape, and a
            // mutation adding `mapped = ?mapped` to `emit_classification_log` left it GREEN (7/7)
            // while production leaked the IP. Asserting against a lookalike proves nothing about
            // production (CLAUDE.md §9 — a round-trip test must end where production ends).
            super::emit_classification_log(class, None);
            let _ = file_handle.lock().map(|mut f| f.flush());
        }
        let emitted = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(!emitted.is_empty(), "the test subscriber must have produced a log");
        assert!(
            emitted.contains("cgnat"),
            "the classification token must appear in the log so a support paste is actionable:\n{emitted}"
        );
        assert!(
            !emitted.contains("100.64.42.99"),
            "INV VIOLATION: the mapped IP address leaked into the log. A user pasting this log has \
             published their own address — the H4/MT2 harvest shape. First 400 chars:\n{}",
            &emitted[..400.min(emitted.len())]
        );
        // Also assert neither the local private IP nor any address-shaped substring leaks.
        assert!(!emitted.contains("10.0.0.5"), "the local private IP leaked into the log");
    }

    /// RED-GREEN, RED half: prove the leak guard reddens the moment an address IS logged. Without
    /// this the GREEN half could be passing because the guard is vacuous. Mirrors the pattern in
    /// `logging::tests::log_emission_guard_reddens_when_a_key_is_logged`.
    #[tokio::test]
    async fn log_line_guard_reddens_when_an_address_is_logged() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nat-privacy-red.log");
        let mapped: IpAddr = "100.64.42.99".parse().unwrap();
        {
            let (_guard, file_handle) = install_test_subscriber(&log_path);
            // Deliberately log the address — simulating a buggy logging site that puts the mapped
            // IP into the line instead of the classification token.
            tracing::info!("NAT: cgnat detected, mapped={:?}", mapped);
            let _ = file_handle.lock().map(|mut f| f.flush());
        }
        let emitted = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(!emitted.is_empty(), "the red-half subscriber must have produced a log");
        assert!(
            emitted.contains("100.64.42.99"),
            "RED HALF: a deliberately-logged mapped address MUST appear in the red run; if it does \
             not, the GREEN half's assertion is vacuous (proves nothing). This probe must REDDEN \
             the GREEN assertion — if you see this message, the GREEN test is no longer a real \
             control."
        );
    }

    /// Pin the classifier's decision on the CGNAT-dominant case: an RFC 6598 mapped address yields
    /// `BehindCgnat` regardless of a symmetric-NAT signal. This is the invariant the
    /// classify-and-log path relies on — CGNAT is the stronger claim and wins.
    #[test]
    fn cgnat_mapped_address_dominates_symmetric_signal() {
        let local: IpAddr = "10.0.0.5".parse().unwrap();
        let mapped: IpAddr = "100.64.1.5".parse().unwrap();
        assert_eq!(
            crate::net::classify_nat(local, Some(mapped)),
            NatClassification::BehindCgnat,
            "CGNAT mapped address ⇒ BehindCgnat regardless of other signals"
        );
    }

    /// Pin that a cold/offline start (no mapped, non-private local) yields Unknown, not NoNat. This
    /// is the ticket's "unknown must not render as a confident negative" rule, and the UI's empty
    /// state depends on it.
    #[test]
    fn cold_start_with_no_mapped_is_unknown_not_no_nat() {
        let local: IpAddr = "203.0.113.10".parse().unwrap();
        assert_eq!(
            crate::net::classify_nat(local, None),
            NatClassification::Unknown,
            "no mapped + non-private local ⇒ Unknown, never a confident NoNat"
        );
    }

    /// `SharedNatClassification` starts empty — the UI must show "not yet determined" before the
    /// first probe completes, not silently imply "no NAT".
    #[tokio::test]
    async fn shared_classification_starts_none() {
        let shared = new_shared_classification();
        assert!(shared.read().await.is_none(), "slot starts empty — UI shows 'not yet determined'");
    }

    /// After `classify_and_log`-equivalent logic stores a value, the slot holds it. We can't drive
    /// the real function without sockets, but we pin the store contract: writing the classification
    /// to the slot is what the UI reads.
    #[tokio::test]
    async fn writing_the_slot_publishes_the_classification() {
        let shared = new_shared_classification();
        *shared.write().await = Some(NatClassification::BehindCgnat);
        assert_eq!(
            shared.read().await.as_ref(),
            Some(&NatClassification::BehindCgnat),
            "the UI reads the slot the probe wrote"
        );
    }

    /// The classification token set is exactly the four loggable words. If a future variant grows a
    /// payload (it must not be the raw address — see INV), this reds because the set size changes.
    #[test]
    fn classification_token_set_is_exactly_four_words() {
        let all = [
            NatClassification::NoNat,
            NatClassification::BehindNat,
            NatClassification::BehindCgnat,
            NatClassification::Unknown,
        ];
        let tokens: std::collections::HashSet<&str> = all.iter().map(|c| c.as_log_token()).collect();
        assert_eq!(tokens.len(), 4, "exactly four distinct tokens (one per variant)");
        for token in ["no-nat", "nat", "cgnat", "unknown"] {
            assert!(tokens.contains(token), "missing token {token:?}");
        }
    }

    // ── Mutation probes (documented, not run) ────────────────────────────────────────────────
    //
    // The project rule is "a green test proves nothing until you have seen it red". The pure
    // classifier is already probed exhaustively in `net::tests` (PROBE-1..5). For this module, the
    // load-bearing assertions are the two log-privacy tests above. Their red halves are run inline
    // (`log_line_guard_reddens_when_an_address_is_logged`), which is the discriminator that proves
    // the green half is not vacuous — exactly the pattern the logging-module privacy tests use.
}
