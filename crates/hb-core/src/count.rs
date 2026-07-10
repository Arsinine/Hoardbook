//! Relay-derived count tallies — the **pure** half of "how many people are on Hoardbook"
//! (spec §Privacy Model → Userbase metrics; Decision #16). No telemetry, no phone-home: the count
//! is a *read* of public relay events, tallied here over a fetched `&[Event]` slice. The network
//! query that feeds these lives in `hb-net::count`; the L2 cases prove it end-to-end.
//!
//! Every tally is **signature-verified and distinct**: a bad-signature event is dropped before it
//! can inflate the figure, and the result is deduped by `npub` so the same author pulled from two
//! relays counts once. The number is honestly an *estimate per relay-set*, never an authoritative
//! global figure.
//!
//! **Known limits (not regressions, stated for honesty):** dedup-by-`npub` is **not** Sybil-proof —
//! a flood of cheap, validly-signed fresh keypairs inflates the count; and the `hb-canary` exclusion
//! is a *deflation*-only griefing surface (anyone can tag their own events `hb-canary` to exclude
//! themselves, never to inflate). Both are inherent to a permissionless, no-account count; the figure
//! is presented as an estimate, not a hardened metric.
//!
//! **Canary exclusion (F-canary).** The VPS canary publishes throwaway-`npub` events to validate
//! the live backbone; every one carries a `t`=[`CANARY_MARKER`] tag. The tallies **exclude** any
//! event bearing it, so the canary's synthetic traffic never pollutes the online/userbase counts —
//! the exclusion (not just the throwaway key) is what enforces "don't pollute real data."

use std::collections::HashSet;

use nostr::prelude::*;

use crate::identity::verify_event;

/// The Hoardbook-internal `t` tag stamped on **every** canary-published event. `count_distinct_*`
/// and discovery exclude events bearing it, keeping the canary's throwaway `npub`s out of the
/// online/userbase counts and out of tag search.
pub const CANARY_MARKER: &str = "hb-canary";

/// Tolerance for a `created_at` slightly ahead of our clock (matches `binding::FUTURE_SKEW_SECS`).
/// The online count does **not** trust the relay's clock: a validly-signed but *future*-dated
/// presence would otherwise read as "online" indefinitely (it never falls below the moving floor),
/// so a non-conforming/hostile relay could inflate the figure. Anything beyond this skew is dropped.
const FUTURE_SKEW_SECS: u64 = 300;

/// True iff the event carries the canary `t` tag — used to exclude synthetic canary traffic from
/// every real-data tally and from discovery.
pub fn is_canary(event: &Event) -> bool {
    event.tags.hashtags().any(|t| t == CANARY_MARKER)
}

/// Count distinct **online-now** `npub`s from a set of presence events: drop bad-signature events,
/// drop canary-tagged events, drop stale events (older than the freshness `window_secs`), then
/// dedup by author. Freshness is inclusive at the boundary (`created_at >= now - window`), matching
/// the contact-list `● Online` badge (Decision #12). Multi-relay dedup is implicit — the same
/// author pulled from N relays collapses to one.
pub fn count_distinct_online(events: &[Event], now: u64, window_secs: u64) -> usize {
    let floor = now.saturating_sub(window_secs);
    let ceiling = now.saturating_add(FUTURE_SKEW_SECS);
    let mut seen: HashSet<PublicKey> = HashSet::new();
    for ev in events {
        if is_canary(ev) {
            continue; // F-canary: synthetic presence never counts
        }
        let created = ev.created_at.as_u64();
        if created < floor || created > ceiling {
            continue; // stale → offline; future-dated beyond skew → don't trust the relay's clock
        }
        if verify_event(ev).is_err() {
            continue; // a forged/tampered presence cannot inflate the count
        }
        seen.insert(ev.pubkey);
    }
    seen.len()
}

/// Count distinct **userbase** `npub`s from a set of Hoardbook-kind events (teaser / presence /
/// listing): drop bad-signature events, drop canary-tagged events, then dedup by author across all
/// kinds. No freshness filter — any author who has ever published is part of the userbase.
pub fn count_distinct_userbase(events: &[Event]) -> usize {
    let mut seen: HashSet<PublicKey> = HashSet::new();
    for ev in events {
        if is_canary(ev) {
            continue; // F-canary: the canary's throwaway npub is not a user
        }
        if verify_event(ev).is_err() {
            continue;
        }
        seen.insert(ev.pubkey);
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::build_binding;
    use crate::event::{build_teaser, Teaser};
    use crate::Identity;

    const NOW: u64 = 1_700_000_000;
    const WINDOW: u64 = 600; // 10 min online window (Decision #12)

    /// A signed presence event for `id`, created at `created_at`.
    fn presence_at(id: &Identity, created_at: u64) -> Event {
        // build_binding stamps created_at = the `now` it is handed.
        build_binding(id, created_at, 30 * 60).unwrap()
    }

    /// A signed presence event carrying the canary marker (a throwaway-npub canary beacon).
    fn canary_presence(id: &Identity, created_at: u64) -> Event {
        let base = build_binding(id, created_at, 30 * 60).unwrap();
        let mut tags: Vec<Tag> = base.tags.iter().cloned().collect();
        tags.push(Tag::hashtag(CANARY_MARKER));
        id.sign(
            EventBuilder::new(base.kind, base.content)
                .tags(tags)
                .custom_created_at(Timestamp::from(created_at)),
        )
        .unwrap()
    }

    #[test]
    fn online_dedups_same_npub_across_relays() {
        // The same author's presence pulled from two relays counts once.
        let id = Identity::generate();
        let a = presence_at(&id, NOW);
        let b = presence_at(&id, NOW);
        assert_eq!(count_distinct_online(&[a, b], NOW, WINDOW), 1);
    }

    #[test]
    fn online_counts_distinct_fresh_npubs() {
        let ids: Vec<Identity> = (0..3).map(|_| Identity::generate()).collect();
        let evs: Vec<Event> = ids.iter().map(|id| presence_at(id, NOW)).collect();
        assert_eq!(count_distinct_online(&evs, NOW, WINDOW), 3);
    }

    #[test]
    fn online_drops_stale_events() {
        let fresh = Identity::generate();
        let stale = Identity::generate();
        let evs = vec![
            presence_at(&fresh, NOW - 60),            // within window
            presence_at(&stale, NOW - WINDOW - 1),    // just past the window → stale
        ];
        assert_eq!(count_distinct_online(&evs, NOW, WINDOW), 1, "only the fresh npub is online");
    }

    #[test]
    fn online_window_boundary_is_inclusive() {
        // created_at exactly at (now - window) is still online (inclusive boundary).
        let id = Identity::generate();
        let ev = presence_at(&id, NOW - WINDOW);
        assert_eq!(count_distinct_online(&[ev], NOW, WINDOW), 1);
        // one second older is stale.
        let id2 = Identity::generate();
        let ev2 = presence_at(&id2, NOW - WINDOW - 1);
        assert_eq!(count_distinct_online(&[ev2], NOW, WINDOW), 0);
    }

    #[test]
    fn online_drops_future_dated_events_beyond_skew() {
        // A validly-signed but future-dated presence (a hostile/non-conforming relay's clock) must
        // not read as "online forever" — it is dropped beyond the skew tolerance. Within skew is fine.
        let far_future = Identity::generate();
        let within_skew = Identity::generate();
        let evs = vec![
            presence_at(&far_future, NOW + 10_000),    // way ahead → dropped
            presence_at(&within_skew, NOW + 60),       // within the 300 s skew → counted
        ];
        assert_eq!(count_distinct_online(&evs, NOW, WINDOW), 1, "only the within-skew npub counts");
    }

    #[test]
    fn online_drops_bad_signature_events() {
        let id = Identity::generate();
        let mut tampered = presence_at(&id, NOW);
        tampered.content = "mutated after signing".into(); // id/sig no longer match
        assert_eq!(count_distinct_online(&[tampered], NOW, WINDOW), 0, "a forged presence cannot count");
    }

    #[test]
    fn online_excludes_canary_marked_events() {
        // F-canary: a fresh, validly-signed presence bearing the canary marker is NOT counted.
        let real = Identity::generate();
        let canary = Identity::generate();
        let evs = vec![presence_at(&real, NOW), canary_presence(&canary, NOW)];
        assert_eq!(count_distinct_online(&evs, NOW, WINDOW), 1, "the canary npub must not be counted");
    }

    #[test]
    fn userbase_counts_distinct_authors_across_kinds() {
        // A presence event and a teaser event from the same author count once; a second author adds one.
        let a = Identity::generate();
        let b = Identity::generate();
        let teaser = build_teaser(
            &a,
            &Teaser { display_name: "a".into(), bio: String::new(), tags: vec![], content_types: vec![], picture: None },
            true,
        )
        .unwrap();
        let evs = vec![presence_at(&a, NOW), teaser, presence_at(&b, NOW)];
        assert_eq!(count_distinct_userbase(&evs), 2, "two distinct authors across three events");
    }

    #[test]
    fn userbase_excludes_canary_marked_events() {
        let real = Identity::generate();
        let canary = Identity::generate();
        let evs = vec![presence_at(&real, NOW), canary_presence(&canary, NOW)];
        assert_eq!(count_distinct_userbase(&evs), 1, "the canary npub is not a user");
    }

    #[test]
    fn userbase_drops_bad_signature_events() {
        let id = Identity::generate();
        let mut tampered = presence_at(&id, NOW);
        tampered.content = "mutated".into();
        assert_eq!(count_distinct_userbase(&[tampered]), 0);
    }

    #[test]
    fn is_canary_detects_the_marker() {
        let id = Identity::generate();
        assert!(is_canary(&canary_presence(&id, NOW)));
        assert!(!is_canary(&presence_at(&id, NOW)));
    }

    /// L4 / F2 — the Schnorr-verify-per-event tally runs on the command thread (the online-count
    /// chip's poll runs it), so a slow tally is exactly the CPU drain L4 exists to catch. Over a
    /// 500-event fixture it must complete well under a generous wall-clock budget; a pathological
    /// regression (per-event reallocation, an accidental O(n²)) blows past it. The fixture build
    /// (500 keygens + signs) is **not** timed — only the tally is.
    #[test]
    fn count_tally_over_500_events_is_under_budget_f2() {
        use std::time::Instant;
        let events: Vec<Event> = (0..500).map(|_| presence_at(&Identity::generate(), NOW)).collect();

        let t0 = Instant::now();
        let online = count_distinct_online(&events, NOW, WINDOW);
        let users = count_distinct_userbase(&events);
        let elapsed = t0.elapsed();

        assert_eq!(online, 500, "500 distinct fresh npubs");
        assert_eq!(users, 500);
        // 500 Schnorr verifies ≈ tens of ms on CI hardware; 5 s is a generous ceiling that only a
        // genuine regression trips. (Seeded generously — tighten on a clean run, like the FE budgets.)
        assert!(
            elapsed.as_secs_f64() < 5.0,
            "count tally over 500 events took {elapsed:?} — over the L4 wall-clock budget (F2)"
        );
    }
}
