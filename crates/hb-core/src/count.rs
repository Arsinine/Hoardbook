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
//! **Online vs userbase admission (W2).** Kind 11111 is not exclusively Hoardbook's — a foreign
//! protocol reuses it with validly-signed events that carry none of our binding tags
//! (probe-confirmed 2026-08-04). The online tally therefore admits a presence event on more than
//! a signature: `fresh_presence` requires `verify_binding` (signature + kind pin + author pin +
//! schema/expiry tags), so foreign kind-11111 traffic cannot inflate the chip. The userbase tally
//! is a different count over teaser/listing/presence kinds where bindings do not apply; it remains
//! signature-only.
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

use std::collections::{HashMap, HashSet};

use nostr::prelude::*;

use crate::binding::{verify_binding, FUTURE_SKEW_SECS, KIND_PRESENCE};
use crate::identity::verify_event;

/// The Hoardbook-internal `t` tag stamped on **every** canary-published event. `count_distinct_*`
/// and discovery exclude events bearing it, keeping the canary's throwaway `npub`s out of the
/// online/userbase counts and out of tag search.
pub const CANARY_MARKER: &str = "hb-canary";

/// True iff the event carries the canary `t` tag — used to exclude synthetic canary traffic from
/// every real-data tally and from discovery.
pub fn is_canary(event: &Event) -> bool {
    event.tags.hashtags().any(|t| t == CANARY_MARKER)
}

/// Count distinct **online-now** `npub`s from a set of presence events: drop events without a
/// valid Hoardbook binding (`verify_binding` — signature + kind pin + author pin + schema/expiry),
/// drop canary-tagged events, drop stale events (older than the freshness `window_secs`), then
/// dedup by author. Freshness is inclusive at the boundary (`created_at >= now - window`), matching
/// the contact-list `● Online` badge (Decision #12). Multi-relay dedup is implicit — the same
/// author pulled from N relays collapses to one.
pub fn count_distinct_online(events: &[Event], now: u64, window_secs: u64) -> usize {
    fresh_presence(events, now, window_secs).len()
}

/// The same fresh-presence pass as [`count_distinct_online`], but keeping **who** and **when**:
/// author → the newest accepted `created_at`. Identical admission rules (canary-excluded,
/// binding-verified, stale-dropped, future-skew-capped), so the count is exactly this map's
/// length — one tally, two readings.
///
/// M17 W5.2: the contact list needs a *real* last-seen. The 60s online poll already fetches these
/// events for the aggregate chip, so returning the pairs costs **no** extra relay query and no
/// per-contact fan-out — the caller matches its own contacts against the map.
pub fn fresh_presence(events: &[Event], now: u64, window_secs: u64) -> HashMap<PublicKey, u64> {
    let floor = now.saturating_sub(window_secs);
    // The online count does **not** trust the relay's clock: a validly-signed but *future*-dated
    // presence would otherwise read as "online" indefinitely (it never falls below the moving
    // floor), so a non-conforming/hostile relay could inflate the figure. Anything beyond the
    // shared skew (`binding::FUTURE_SKEW_SECS`, imported above) is dropped.
    let ceiling = now.saturating_add(FUTURE_SKEW_SECS);
    let mut seen: HashMap<PublicKey, u64> = HashMap::new();
    for ev in events {
        // A relay is not trusted to honour the filter it was sent: check the kind locally. Without
        // this, a hostile relay answers the presence query with the author's own validly-signed
        // teaser/listing — it passes the signature check and reads as "online" (W5 review).
        if ev.kind.as_u16() != KIND_PRESENCE {
            continue;
        }
        if is_canary(ev) {
            continue; // F-canary: synthetic presence never counts
        }
        let created = ev.created_at.as_u64();
        if created < floor || created > ceiling {
            continue; // stale → offline; future-dated beyond skew → don't trust the relay's clock
        }
        // W2: kind 11111 is not exclusively ours — a foreign protocol reuses it with validly-signed
        // events that carry none of our binding tags (probe-confirmed 2026-08-04: the owner's chip
        // read "3 users" = 2 real machines + 1 such foreign npub). `verify_event` is signature-only
        // and admits them; `verify_binding` additionally requires a well-formed Hoardbook presence
        // binding (our schema tag + explicit expiry + author pin). For a global tally there is no
        // external "expected" author — the beacon is self-authored — so `expected = ev.pubkey`. This
        // is not circular: verify_binding still meaningfully re-checks the signature, pins the kind,
        // pins author==pubkey (a relay can't substitute a *different* valid event for this one), and
        // demands the schema/expiry tags that a foreign kind-11111 event does not carry.
        if verify_binding(ev, &ev.pubkey, now).is_err() {
            continue; // a forged/tampered/unbound presence cannot inflate the count
        }
        // A `created_at` inside the future-skew tolerance is admitted, but never *recorded* as the
        // future: clamping to `now` keeps a +5min stamp from rendering as age zero (and outliving a
        // truthful one under the max below) for the skew's whole duration.
        let stamp = created.min(now);
        // Newest wins: a peer's replaceable beacon may arrive from several relays at once.
        seen.entry(ev.pubkey).and_modify(|t| *t = (*t).max(stamp)).or_insert(stamp);
    }
    seen
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
    // An arbitrary fixture window, NOT the app's. `count_distinct_online` takes `window_secs` as a
    // parameter, so hb-core holds no copy of the production value (which is 480 — Decision #12's
    // 600 was superseded by the owner on 2026-08-26).
    const WINDOW: u64 = 600;

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
    fn fresh_presence_keeps_who_and_when_and_matches_the_count() {
        // W5.2: the map is the count's own pass, kept — same admission rules, same length. It is
        // what gives an offline contact a real age instead of "seen just now" forever.
        let online = Identity::generate();
        let stale = Identity::generate();
        let forged = Identity::generate();
        let canary = Identity::generate();
        let mut bad = presence_at(&forged, NOW - 30);
        bad.content.push('x'); // breaks the signature
        let evs = vec![
            presence_at(&online, NOW - 30),
            presence_at(&stale, NOW - WINDOW - 1),
            bad,
            canary_presence(&canary, NOW - 10),
        ];
        let map = fresh_presence(&evs, NOW, WINDOW);
        assert_eq!(map.len(), 1, "only the fresh, verified, non-canary author survives");
        assert_eq!(map.get(&online.public_key()), Some(&(NOW - 30)), "and we know WHEN");
        assert!(!map.contains_key(&stale.public_key()));
        assert_eq!(map.len(), count_distinct_online(&evs, NOW, WINDOW), "count == map length");
    }

    #[test]
    fn a_relay_cannot_answer_the_presence_query_with_another_kind() {
        // W5 review (HIGH): the relay is not trusted to honour its own filter. Bob's validly-signed
        // TEASER returned to a presence request must not read as "Bob is online" — it passes the
        // signature check, so only the local kind check stops it.
        let bob = Identity::generate();
        let teaser = build_teaser(
            &bob,
            &Teaser {
                display_name: "bob".into(),
                bio: String::new(),
                tags: vec![],
                content_types: vec![],
                picture: None,
            },
            true,
        )
        .unwrap();
        assert!(verify_event(&teaser).is_ok(), "the substituted event is genuinely signed");
        assert!(
            fresh_presence(std::slice::from_ref(&teaser), NOW, WINDOW).is_empty(),
            "wrong kind → not online"
        );
        assert_eq!(count_distinct_online(&[teaser], NOW, WINDOW), 0, "and it cannot inflate the chip");
    }

    #[test]
    fn a_future_dated_beacon_within_skew_is_not_recorded_as_the_future() {
        // Admitted (inside the ±300s tolerance) but clamped to `now`, so it renders as age ~0
        // rather than staying "fresher than real" for the skew's whole duration — and so it cannot
        // outrank a truthful later beacon under the newest-wins merge.
        let id = Identity::generate();
        let ahead = presence_at(&id, NOW + 200);
        let map = fresh_presence(std::slice::from_ref(&ahead), NOW, WINDOW);
        assert_eq!(map.get(&id.public_key()), Some(&NOW), "clamped to now, never above it");

        let truthful = presence_at(&id, NOW - 10);
        let both = fresh_presence(&[ahead, truthful], NOW, WINDOW);
        assert_eq!(*both.get(&id.public_key()).unwrap(), NOW, "clamped value, not NOW + 200");
    }

    #[test]
    fn fresh_presence_keeps_the_newest_beacon_per_author() {
        // The same peer's replaceable beacon arriving from several relays: the newest ts wins, so
        // the rendered age is the freshest thing we actually saw.
        let id = Identity::generate();
        let evs = vec![presence_at(&id, NOW - 300), presence_at(&id, NOW - 30), presence_at(&id, NOW - 120)];
        let map = fresh_presence(&evs, NOW, WINDOW);
        assert_eq!(map.get(&id.public_key()), Some(&(NOW - 30)));
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

    /// W2 — the probe-confirmed bug: a validly-signed kind-11111 event with **no Hoardbook binding**
    /// (no `hb-v` schema tag, no `hb-expires`) is the exact shape of the foreign kind-11111 traffic
    /// that was inflating the owner's chip — a different protocol reusing kind 11111. It passes
    /// `verify_event` (the signature is real), is fresh, non-canary, and the right kind, yet it must
    /// not count. A real Hoardbook beacon from a distinct npub is included alongside so the test
    /// cannot pass by rejecting everything (count is exactly 1, not 0 and not 2).
    #[test]
    fn online_rejects_validly_signed_but_unbound_kind_11111() {
        // Foreign-protocol kind-11111 traffic: validly signed, right kind, fresh — but it carries
        // none of our binding tags, so verify_binding must reject it on the missing schema version.
        let foreign = Identity::generate();
        let unbound = foreign
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .custom_created_at(Timestamp::from(NOW)),
            )
            .unwrap();
        // Sanity: this is genuinely a validly-signed presence event — the pre-change admission gate
        // (verify_event) accepts it. That is precisely why the chip inflated.
        assert!(verify_event(&unbound).is_ok(), "the foreign event is genuinely validly signed");
        assert_eq!(unbound.kind.as_u16(), KIND_PRESENCE, "and it is the right kind");

        // A real Hoardbook beacon — must still count, so the assertion below cannot pass vacuously.
        let real = Identity::generate();
        let valid = presence_at(&real, NOW);

        let evs = vec![unbound, valid];
        assert_eq!(
            count_distinct_online(&evs, NOW, WINDOW),
            1,
            "validly-signed-but-unbound kind-11111 must not inflate the online count"
        );
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
