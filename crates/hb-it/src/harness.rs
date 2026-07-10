//! Shared L2 harness: the relay context every suite is handed, plus the small helpers (real
//! clock, freshness window, a publish→fetch settle) the round-trips need.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use hb_core::Identity;
use hb_net::RelayClient;

use crate::tap::TestResult;

/// Presence online window (spec / TEST_PLAN §7: "presence online window 10 min").
pub const ONLINE_WINDOW_SECS: u64 = 600;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The relay set + options a suite runs against.
pub struct Ctx {
    pub relays: Vec<String>,
    /// NIP-13 difficulty for the DISC5 `--pow` path (0 = off).
    pub pow: u8,
    /// A per-run-unique token mixed into discovery tags, so tag-search counts stay correct even
    /// against a relay that already holds events from earlier runs (CI relays are fresh; this
    /// keeps local re-runs honest).
    pub run_id: String,
    /// Survey mode (`--survey`): probe each relay's acceptance instead of running the L2 suites.
    pub survey: bool,
    /// Canary mode (`--canary`): run the live-backbone probe instead of the cooperative L2 suites.
    pub canary: bool,
    /// `--interval <secs>`: in canary mode, loop forever on this cadence (the daemon form); `None`
    /// runs one cycle and exits with its code (the oneshot/timer form).
    pub interval: Option<u64>,
    /// Same-NAT mode (`--same-nat`, devtest #9): run the live same-source-IP presence diagnosis
    /// instead of the cooperative L2 suites.
    pub same_nat: bool,
}

impl Ctx {
    /// A discovery tag namespaced to this run (e.g. `hbit-anime-9f3c…`).
    pub fn tag(&self, base: &str) -> String {
        format!("hbit-{}-{}", base, self.run_id)
    }

    /// Connect a client to the whole relay set (multi-publish + cross-relay dedup).
    pub async fn connect(&self, id: &Identity) -> Result<RelayClient> {
        Ok(RelayClient::connect(id, &self.relays, CONNECT_TIMEOUT).await?)
    }

    /// Connect a client to a single relay by index — used by the cross-relay (ID4) and
    /// withheld-event (AB8) cases where the publisher and reader must use different hosts.
    pub async fn connect_one(&self, id: &Identity, idx: usize) -> Result<RelayClient> {
        let one = std::slice::from_ref(&self.relays[idx]).to_vec();
        Ok(RelayClient::connect(id, &one, CONNECT_TIMEOUT).await?)
    }

    /// True when ≥2 relays are configured (gates the multi-relay cases).
    pub fn multi(&self) -> bool {
        self.relays.len() >= 2
    }
}

/// Current unix time in seconds (real clock — presence freshness must be honest).
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock before 1970").as_secs()
}

/// Online-status read: a presence event is "online" if refreshed within the window.
pub fn is_online(created_at: u64, now: u64) -> bool {
    now.saturating_sub(created_at) <= ONLINE_WINDOW_SECS
}

/// Small settle after a publish so the relay has indexed the event before a fetch (avoids a
/// localhost write→read race; nostr-sdk already waits for the OK, this is belt-and-braces).
pub async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Wrap an inner `Result` into a TAP TestResult, rendering the full error chain on failure.
pub fn result(name: &str, r: Result<()>) -> TestResult {
    match r {
        Ok(()) => TestResult::ok(name),
        Err(e) => TestResult::fail(name, format!("{e:#}")),
    }
}
