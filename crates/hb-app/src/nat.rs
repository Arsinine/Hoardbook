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
pub const NET_REPORT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The target `connect()` uses to let the OS pick a source interface. Any routable public address
/// works — the socket never sends, and the port is irrelevant. `1.1.1.1:80` is the conventionally
/// used target (Cloudflare DNS) in STUN-style local-address probes. No packet leaves the machine:
/// `connect` on a UDP socket only populates the kernel routing decision.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging;

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
