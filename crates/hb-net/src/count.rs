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

use std::collections::HashMap;
use std::time::Duration;

use hb_core::binding::KIND_PRESENCE;
use hb_core::event::{KIND_LISTING, KIND_TEASER};
use hb_core::{count_distinct_online, count_distinct_userbase, fresh_presence};
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

/// The same single query as [`count_online`], returning **who** was seen and **when** (author →
/// newest accepted `created_at`) instead of just how many. Same filter, same admission rules, so
/// the count is this map's length — M17 W5.2 reads the contact list's real last-seen off the poll
/// that was already running, adding **no** relay query shape and no per-contact fan-out.
///
/// Also returns the `now` the freshness was judged against, so the caller stamps ages against the
/// same reference the filter used.
pub async fn fetch_online_presence(
    client: &RelayClient,
    window_secs: u64,
    timeout: Duration,
) -> Result<(HashMap<PublicKey, u64>, u64), NetError> {
    let now = unix_now();
    let events = client.fetch(presence_count_filter(now, window_secs), timeout).await?;
    Ok((fresh_presence(&events, now, window_secs), now))
}

/// The presence filter for a SPECIFIC set of authors (W1, 2026-08-02). Unlike
/// [`presence_count_filter`]'s unbounded global aggregate, this asks the relay only for THESE
/// authors' beacons, so a contact's presence can never be displaced by the relay's global response
/// cap. kind-11111 is replaceable, so the relay keeps one beacon per author; `.limit(authors.len())`
/// declares that budget as ours, not the relay's default cap.
pub fn presence_authors_filter(authors: &[PublicKey], now: u64, window_secs: u64) -> Filter {
    Filter::new()
        .kind(Kind::from_u16(KIND_PRESENCE))
        .authors(authors.iter().copied())
        .since(Timestamp::from(now.saturating_sub(window_secs)))
        .limit(authors.len().max(1))
}

/// Author-filtered per-contact presence read (W1): fresh presence for a KNOWN set of authors,
/// immune to the global relay cap that [`fetch_online_presence`] is subject to. Returns the same
/// `(author → newest created_at, now)` shape. An empty author set short-circuits (no query).
pub async fn fetch_presence_for_authors(
    client: &RelayClient,
    authors: &[PublicKey],
    window_secs: u64,
    timeout: Duration,
) -> Result<(HashMap<PublicKey, u64>, u64), NetError> {
    let now = unix_now();
    if authors.is_empty() {
        return Ok((HashMap::new(), now));
    }
    let events = client.fetch(presence_authors_filter(authors, now, window_secs), timeout).await?;
    Ok((fresh_presence(&events, now, window_secs), now))
}

/// Count distinct **userbase** `npub`s: fetch the Hoardbook-kind events and tally distinct authors
/// (sig-verified, canary-excluded). Operator-side surface (the in-app default is online-now only —
/// Open Q#6); the cheap NIP-45/SQL alternative lives in `RELAY_DEPLOY.md`.
pub async fn count_userbase(client: &RelayClient, timeout: Duration) -> Result<usize, NetError> {
    let events = client.fetch(userbase_filter(), timeout).await?;
    Ok(count_distinct_userbase(&events))
}

/// The userbase filter for a SPECIFIC set of authors (COUNT2/COUNT3 determinism, 2026-08-10). The
/// sibling of [`presence_authors_filter`], covering all three Hoordbook kinds (teaser / presence /
/// listing) but bounded by `.authors(...)` so the relay's answer is **only ever** these authors'
/// events — immune to the global cap/window that makes [`userbase_filter`]'s unbounded read return
/// different subsets on two successive calls (measured: `73→0`, `121→20`, `336→147`).
///
/// **No `.limit()` is set deliberately.** Unlike [`presence_authors_filter`], whose
/// `.limit(authors.len())` is sound because kind-11111 is replaceable (one live event per author),
/// the userbase span includes parameterized-replaceable kinds (30117 teaser, 31111 listing) where a
/// single author can legitimately hold several events. A `limit` of `authors.len()` would silently
/// truncate an author's events and produce wrong counts. The `.authors()` bound alone already makes
/// the relay's response deterministic — that is what this filter exists to guarantee.
pub fn userbase_authors_filter(authors: &[PublicKey]) -> Filter {
    Filter::new()
        .kinds([
            Kind::from_u16(KIND_TEASER),
            Kind::from_u16(KIND_PRESENCE),
            Kind::from_u16(KIND_LISTING),
        ])
        .authors(authors.iter().copied())
}

/// Author-filtered userbase count (COUNT2/COUNT3 determinism): fetch only THESE authors'
/// Hoordbook-kind events and tally distinct authors through the same production
/// [`count_distinct_userbase`] (sig-verified, canary-excluded, deduped) — so scoping the *fetch*
/// never changes *what is counted*. The test-scoped mirror of [`fetch_presence_for_authors`]:
/// immune to the global relay cap that [`count_userbase`]'s unbounded read is subject to. An empty
/// author set short-circuits to `Ok(0)` (no query) — so a test that has minted no real authors yet
/// does not issue a global fetch against the shared relay.
pub async fn count_userbase_for(
    client: &RelayClient,
    authors: &[PublicKey],
    timeout: Duration,
) -> Result<usize, NetError> {
    if authors.is_empty() {
        return Ok(0);
    }
    let events = client.fetch(userbase_authors_filter(authors), timeout).await?;
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

    #[test]
    fn presence_authors_filter_constrains_authors_kind_and_since() {
        let pk = nostr::Keys::generate().public_key();
        let f = presence_authors_filter(&[pk], 1_700_000_000, 600);
        assert!(!f.is_empty(), "an authors+kind+since filter is constrained");
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("authors"), "authors present in the filter: {json}");
        assert!(json.contains("11111"), "presence kind present in the filter: {json}");
        assert!(
            json.contains(&(1_700_000_000u64 - 600).to_string()),
            "since floor present: {json}"
        );
    }

    #[test]
    fn userbase_authors_filter_carries_all_three_kinds_and_authors() {
        let pk = nostr::Keys::generate().public_key();
        let f = userbase_authors_filter(&[pk]);
        assert!(!f.is_empty(), "an authors+kinds filter is constrained");
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("authors"), "authors present in the filter: {json}");
        for k in ["30117", "11111", "31111"] {
            assert!(json.contains(k), "userbase_authors_filter must include kind {k}: {json}");
        }
    }

    #[test]
    fn userbase_authors_filter_sets_no_limit_so_it_cannot_truncate() {
        // The userbase span includes parameterized-replaceable kinds (30117, 31111) where one author
        // can hold several events; a `.limit(authors.len())` would silently drop events and corrupt
        // the count. `.authors()` alone is what makes the read deterministic — there must be NO
        // `limit` key in the serialized filter.
        let pk = nostr::Keys::generate().public_key();
        let f = userbase_authors_filter(&[pk]);
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            !json.contains("limit"),
            "userbase_authors_filter must NOT carry a limit (it would truncate multi-event authors): {json}"
        );
    }
}
