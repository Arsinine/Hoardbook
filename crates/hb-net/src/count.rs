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

/// **Topic aliveness window (QURATOR-148, owner ruling 2026-08-31)** — a roster member counts as
/// alive if their newest presence beacon was published within the last 30 days: *"a simple filter
/// for pings done within the last 30 days should be sufficient."* It lives here, next to the
/// filter that carries it and the tally that re-checks it, so all three share one definition.
///
/// This is a **different axis** from the 480 s `ONLINE_WINDOW_SECS` "online now" threshold
/// (contact indicators): aliveness measures *activity* (is the Topic worth joining / shown in the
/// discovery sidebar at all), not presence. Do not unify the two constants.
pub const TOPIC_ALIVE_WINDOW_SECS: u64 = 30 * 24 * 60 * 60;

/// The author-filtered presence read for **Topic aliveness** (QURATOR-148): same shape as
/// [`presence_authors_filter`] — bounded by `.authors(...)` and `.limit()` — but with a `since`
/// floor of `now - 30 days`, so the relay is asked only for THESE roster members' beacons and only
/// inside the aliveness window. ⚠ The `.authors()` bound is the load-bearing half: the three
/// presence reports (2026-06-23, -07-10, -08-01) were all a global unbounded kind-11111 query, and
/// one was a launch gate — this filter must never ship without it.
pub fn presence_authors_filter_since(
    authors: &[PublicKey],
    now: u64,
    since_floor: u64,
) -> Filter {
    Filter::new()
        .kind(Kind::from_u16(KIND_PRESENCE))
        .authors(authors.iter().copied())
        .since(Timestamp::from(now.saturating_sub(since_floor)))
        .limit(authors.len().max(1))
}

/// Tally last-seen for a set of authors from raw presence events, judged over a LONG window
/// (Topic aliveness, QURATOR-148). This is NOT [`hb_core::fresh_presence`]: that tally runs
/// `verify_binding(ev, pk, now)`, whose expiry gate refuses any beacon whose `expires_at` tag is in
/// the past — and `MAX_BINDING_TTL_SECS` is 24 h, so a beacon published 29 days ago can never pass
/// it. Aliveness is instead judged **as of the beacon's own publication**: the binding is verified
/// against `created_at` (it was live when minted) and the member counts as alive iff `created_at`
/// is within `window_secs` of `now`. Same admission rules otherwise — kind pin, canary exclusion,
/// foreign-protocol exclusion (the schema/expiry tags), author pin — so nothing new is admitted.
///
/// Returns `author → newest accepted created_at` (clamped to `now`, newest wins), i.e. WHO is alive
/// and WHEN they were last seen. A member who never published a beacon simply does not appear —
/// absence reads as not-alive, by design (no fallback treats absence as alive).
pub fn last_seen_within(
    events: &[Event],
    now: u64,
    window_secs: u64,
) -> HashMap<PublicKey, u64> {
    let floor = now.saturating_sub(window_secs);
    let ceiling = now.saturating_add(hb_core::FUTURE_SKEW_SECS);
    let mut seen: HashMap<PublicKey, u64> = HashMap::new();
    for ev in events {
        if ev.kind.as_u16() != KIND_PRESENCE {
            continue; // a relay is not trusted to honour the filter — pin the kind locally
        }
        if hb_core::is_canary(ev) {
            continue; // F-canary: synthetic presence never counts as aliveness
        }
        let created = ev.created_at.as_u64();
        if created < floor || created > ceiling {
            continue; // outside the aliveness window → not alive; future-dated beyond skew → untrusted
        }
        // Verify as of the beacon's own publication (see the doc comment): the 24 h TTL cap means a
        // 29-day-old beacon is correctly expired TODAY but was live when minted.
        if hb_core::verify_binding(ev, &ev.pubkey, created).is_err() {
            continue; // forged / tampered / foreign-protocol kind reuse — not evidence of a ping
        }
        let stamp = created.min(now); // inside the future skew: never record a future timestamp
        seen.entry(ev.pubkey).and_modify(|t| *t = (*t).max(stamp)).or_insert(stamp);
    }
    seen
}

/// The relay read behind Topic aliveness (QURATOR-148): last-seen for a KNOWN roster, within the
/// aliveness window. Same author-bounded shape as [`fetch_presence_for_authors`] (no global
/// unbounded kind-11111 query — the 2026-08-01 launch-gate defect class). An empty author set
/// short-circuits: no query, no alive members.
pub async fn fetch_last_seen_for_authors(
    client: &RelayClient,
    authors: &[PublicKey],
    window_secs: u64,
    timeout: Duration,
) -> Result<(HashMap<PublicKey, u64>, u64), NetError> {
    let now = unix_now();
    if authors.is_empty() {
        return Ok((HashMap::new(), now));
    }
    let events = client
        .fetch(presence_authors_filter_since(authors, now, window_secs), timeout)
        .await?;
    Ok((last_seen_within(&events, now, window_secs), now))
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

    // ── QURATOR-148 — Topic aliveness (owner ruling 2026-08-31): 30-day last-seen off presence ───
    //
    // The load-bearing property of the new read is the `.authors()` bound: the three presence
    // reports (2026-06-23, 2026-07-10, 2026-08-01 — the last a LAUNCH GATE) were all a global
    // unbounded kind-11111 query. `presence_authors_filter_since` is the roster read and must
    // never ship without the bound.

    #[test]
    fn topic_aliveness_filter_is_bounded_by_authors_kind_and_since() {
        // P-10 MUTATION (orchestrator): in `presence_authors_filter_since` (the containing fn),
        // delete the `.authors(authors.iter().copied())` line — this test must RED on the
        // `authors.len() == 2` assertion. Its siblings stay green (the boundary test touches only
        // `last_seen_within`).
        // ✓ PROVEN RED 2026-09-01: mutation applied (resolved by containing fn — the same line
        //   appears in two sibling filters) → exactly this test FAILED → reverted.
        // DEFECT GUARD (the 2026-08-01 launch-gate shape). Asserted on the SERIALIZED filter — the
        // bytes the relay evaluates — not on a method call, so an empty/absent authors set cannot
        // pass vacuously.
        let a = nostr::Keys::generate().public_key();
        let b = nostr::Keys::generate().public_key();
        let now = 1_800_000_000u64;
        let f = presence_authors_filter_since(&[a, b], now, TOPIC_ALIVE_WINDOW_SECS);
        let wire = serde_json::to_value(&f).expect("filter serializes");
        let authors = wire.get("authors").and_then(|v| v.as_array()).expect("the filter carries an authors set");
        assert_eq!(authors.len(), 2, "BOTH roster npubs ride the filter — no unbounded global read");
        let kinds = wire.get("kinds").and_then(|v| v.as_array()).expect("the filter pins the kind");
        assert!(kinds.iter().any(|k| k.as_u64() == Some(KIND_PRESENCE as u64)), "kind 11111 pinned");
        // The since floor is now − 30 days (NOT the 480 s online-now window, which is a different axis).
        assert_eq!(
            wire.get("since").and_then(|v| v.as_u64()),
            Some(now - TOPIC_ALIVE_WINDOW_SECS),
            "the since floor is the 30-day aliveness window"
        );
        assert_eq!(
            wire.get("limit").and_then(|v| v.as_u64()),
            Some(2),
            "the limit declares the replaceable one-beacon-per-author budget as ours"
        );
    }

    #[test]
    fn a_beacon_at_29_days_counts_alive_at_31_days_it_does_not() {
        // P-10 MUTATION (orchestrator): in `last_seen_within` (the containing fn), change the
        // window floor line `if created < floor || created > ceiling` to drop the `created < floor`
        // half (i.e. `if created > ceiling`) — the 31-day beacon is then admitted and the
        // `seen.len() == 1` assertion REDS. Sibling `topic_aliveness_filter_is_bounded_by_authors…`
        // stays green (it never calls the tally).
        // ✓ PROVEN RED 2026-09-01: mutation applied → exactly this test FAILED → reverted.
        use hb_core::binding::build_binding;
        let now = 1_800_000_000u64;
        let alive_keys = nostr::Keys::generate();
        let dead_keys = nostr::Keys::generate();
        let never_pk = nostr::Keys::generate().public_key(); // never published a beacon at all
        let alive_pk = alive_keys.public_key();
        let dead_pk = dead_keys.public_key();
        // build_binding caps its TTL at 24 h, so mint at the publication instant; last_seen_within
        // verifies each binding AS OF ITS OWN created_at (see its doc comment) — that is exactly the
        // property separating it from hb_core::fresh_presence, whose now-based expiry gate would
        // refuse BOTH beacons here and read the Topic dead.
        let alive_ev = build_binding(&hb_core::Identity::from_keys(alive_keys), now - 29 * 86_400, 1_800).unwrap();
        let dead_ev = build_binding(&hb_core::Identity::from_keys(dead_keys), now - 31 * 86_400, 1_800).unwrap();
        let seen = last_seen_within(&[alive_ev, dead_ev], now, TOPIC_ALIVE_WINDOW_SECS);
        assert_eq!(
            seen.len(),
            1,
            "exactly one alive member: 29 days is inside, 31 days is outside the window"
        );
        assert!(seen.contains_key(&alive_pk), "the 29-day-old beacon's author is alive");
        assert!(!seen.contains_key(&dead_pk), "the 31-day-old beacon's author is not alive");
        assert!(
            !seen.contains_key(&never_pk),
            "a member who never published reads as not-alive (correct, not a bug) — no fallback treats absence as alive"
        );
    }

    #[test]
    fn a_foreign_or_unbound_kind_11111_event_is_not_aliveness() {
        // P-10 MUTATION (orchestrator): in `last_seen_within` (the containing fn), delete the
        // `verify_binding` call (replace the `if … is_err() { continue; }` with `let _ = ev;`) — the
        // foreign event is then admitted and `seen.is_empty()` REDS.
        // ✓ PROVEN RED 2026-09-01 — on the SECOND attempt: the first run stayed green and indicted
        //   the test (wall-clock created_at sat outside the window; see the ⚠ note below), fixed by
        //   pinning created_at in-window, then the mutation redded exactly this test → reverted.
        // W2 (probe-confirmed 2026-08-04): kind 11111 is not exclusively ours. A validly-signed
        // foreign event with none of our binding tags must not read as a ping.
        // ⚠ The created_at MUST be pinned INSIDE the aliveness window: signed at the wall clock it
        // sat outside `now`'s 30-day window and the WINDOW check excluded it — the binding gate was
        // never reached, and deleting `verify_binding` left this test green (caught by exactly that
        // mutation, 2026-09-01). Only with the timestamp in-window does the exclusion this test
        // claims to pin become the binding gate's doing.
        let now = 1_800_000_000u64;
        let foreign = hb_core::Identity::generate();
        let ev = foreign
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "foreign protocol")
                    .custom_created_at(Timestamp::from(now - 100)),
            )
            .unwrap();
        let seen = last_seen_within(&[ev], now, TOPIC_ALIVE_WINDOW_SECS);
        assert!(seen.is_empty(), "an unbound kind-11111 event is not evidence of a Hoardbook ping");
    }
}
