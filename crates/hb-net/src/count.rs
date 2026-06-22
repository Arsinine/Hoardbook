//! Relay-derived count **queries** (spec §Privacy Model → Userbase metrics; Decision #16). The
//! network half of "how many people are on Hoardbook": one filtered relay read, then the pure
//! `hb-core` tally (sig-verified, distinct-by-`npub`, canary-excluded). No telemetry — this only
//! *reads* events the relay already holds.
//!
//! **Why not NIP-45 COUNT client-side?** NIP-45 `COUNT` returns a raw event total: it cannot verify
//! Schnorr signatures, cannot exclude `hb-canary`-tagged synthetic traffic, and cannot dedup
//! distinct authors for a non-replaceable kind — all of which Decision #16 requires. (strfry, the
//! relay in CI and on the live backbone, also does not implement `COUNT`.) The cheap NIP-45/SQL path
//! is therefore the **operator's** `COUNT(DISTINCT pubkey)` recipe in `RELAY_DEPLOY.md` (which
//! excludes `hb-canary` in SQL); the client uses this accurate fetch+distinct path so the in-app
//! chip and the canary-no-pollution guarantee hold end-to-end.

use std::time::Duration;

use hb_core::binding::KIND_PRESENCE;
use hb_core::event::{KIND_LISTING, KIND_TEASER};
use hb_core::{count_distinct_online, count_distinct_userbase};
use nostr::prelude::*;

use crate::client::RelayClient;
use crate::error::NetError;

/// The presence-count filter: replaceable presence events fresh within the window. `since` lets the
/// relay pre-drop stale beacons; the tally re-checks freshness (a non-compliant relay may return
/// older events). The same `now` feeds both, so the boundary is consistent.
pub fn presence_count_filter(now: u64, window_secs: u64) -> Filter {
    Filter::new()
        .kind(Kind::from_u16(KIND_PRESENCE))
        .since(Timestamp::from(now.saturating_sub(window_secs)))
}

/// The userbase filter: every Hoardbook-kind event (teaser / presence / listing). Distinct authors
/// across these kinds = the userbase.
pub fn userbase_filter() -> Filter {
    Filter::new().kinds([
        Kind::from_u16(KIND_TEASER),
        Kind::from_u16(KIND_PRESENCE),
        Kind::from_u16(KIND_LISTING),
    ])
}

/// Current unix seconds — the freshness reference for an online count.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Count distinct `npub`s **online now**: one filtered relay read for fresh presence events, then
/// the pure `count_distinct_online` tally (sig-verified, stale-dropped, canary-excluded, deduped by
/// author across the relay set). Best-effort: a relay/connect error surfaces as `Err`; the caller
/// renders it as "count unavailable", never a misleading zero.
pub async fn count_online(
    client: &RelayClient,
    window_secs: u64,
    timeout: Duration,
) -> Result<usize, NetError> {
    let now = unix_now();
    let events = client.fetch(presence_count_filter(now, window_secs), timeout).await?;
    Ok(count_distinct_online(&events, now, window_secs))
}

/// Count distinct **userbase** `npub`s: fetch the Hoardbook-kind events and tally distinct authors
/// (sig-verified, canary-excluded). Operator-side surface (the in-app default is online-now only —
/// Open Q#6); the cheap NIP-45/SQL alternative lives in `RELAY_DEPLOY.md`.
pub async fn count_userbase(client: &RelayClient, timeout: Duration) -> Result<usize, NetError> {
    let events = client.fetch(userbase_filter(), timeout).await?;
    Ok(count_distinct_userbase(&events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_filter_constrains_kind_and_since() {
        let f = presence_count_filter(1_700_000_000, 600);
        assert!(!f.is_empty(), "a kind+since filter is constrained (not an unbounded fetch)");
        // The since floor is now - window.
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("11111"), "presence kind present in the filter: {json}");
        assert!(json.contains(&(1_700_000_000u64 - 600).to_string()), "since floor present: {json}");
    }

    #[test]
    fn presence_filter_since_saturates_at_zero() {
        // window > now must not underflow; the floor clamps to 0.
        let f = presence_count_filter(100, 600);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"since\":0") || json.contains("\"since\": 0"), "since clamps to 0: {json}");
    }

    #[test]
    fn userbase_filter_covers_all_hoardbook_kinds() {
        let f = userbase_filter();
        assert!(!f.is_empty());
        let json = serde_json::to_string(&f).unwrap();
        for k in ["30117", "11111", "31111"] {
            assert!(json.contains(k), "userbase filter must include kind {k}: {json}");
        }
    }
}
