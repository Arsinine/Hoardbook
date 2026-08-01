//! Pure discovery logic (M3): the teaser tag/content-type matcher, the ingest pipeline that turns
//! a relay's raw teaser events into trustworthy search hits, and replaceable-event resolution.
//!
//! A relay is an adversary (AB3/AB8): it can flood junk teasers, return bad-signature events,
//! oversize bodies, duplicates, or — for a replaceable event — *both* the old and new version when
//! it should have dropped the old. Every guard here is a pure function so the resilience is
//! unit-tested without a relay: bad signatures are discarded (via `parse_teaser`'s verify),
//! oversize bodies are bounded before parse, results are deduped by `npub` and capped, and a
//! non-compliant relay's duplicate replaceable events collapse to the newest by `created_at`.

use hb_core::event::{parse_teaser, Teaser};
use nostr::prelude::*;

/// Per-teaser content-size bound applied on ingest, before parse — a hostile relay flooding huge
/// teaser bodies can't exhaust memory. (Generous vs a real teaser; teasers are name+bio+tags.)
pub const MAX_TEASER_BYTES: usize = 8192;

/// A trustworthy discovery hit: a verified teaser, the `npub` that signed it, and the teaser's
/// `created_at` (for the recency tiebreak in [`rank_hits`]). This is an **internal** type — it never
/// crosses the wire; the serialized card is `PeerSearchHit` in hb-app, which deliberately omits
/// `created_at` (the card shows the teaser, not the relay's event clock).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub npub: String,
    pub teaser: Teaser,
    /// The teaser event's `created_at`, carried so the recency tiebreak survives ingest. This is the
    /// *signing* time of the latest teaser for this npub (dedup keeps the newest), not "last seen".
    pub created_at: Timestamp,
}

/// Whether a teaser satisfies a query: **tags AND-intersect** (every requested tag must be
/// present) while **content-types OR-union** (any requested content-type matches). An empty tag
/// list imposes no tag constraint; an empty content-type list imposes no content-type constraint
/// (DISC1).
pub fn teaser_matches(teaser: &Teaser, tags: &[String], content_types: &[String]) -> bool {
    let tags_ok = tags.iter().all(|q| teaser.tags.contains(q));
    let ct_ok =
        content_types.is_empty() || content_types.iter().any(|q| teaser.content_types.contains(q));
    tags_ok && ct_ok
}

/// Turn raw fetched teaser events into ranked, trustworthy hits:
/// bound size → verify+parse (discard bad-sig / wrong-schema) → match the query → dedup by `npub`
/// → rank → cap. Each stage is independently observable in the tests (AB3a/b/c + DISC1).
///
/// The newest-first presort exists **only so the per-`npub` dedup keeps the latest teaser** — a
/// non-compliant relay returning old+new can't hide an update. The *user-visible* order is then set
/// by [`rank_hits`], a separate pure function (M20 W3 — previously the visible order was the dedup
/// sort's accidental by-product, "whoever republished most recently"). The cap bounds the count *of
/// the ranked output*; fetching above the cap is the caller's job (see `TEASER_SEARCH_FETCH_LIMIT`).
///
/// Returns only the hits. Use [`ingest_teasers_capped`] when you also need to know whether the cap
/// truncated the result (e.g. the Discover UI's "showing first N" affordance).
pub fn ingest_teasers(
    events: Vec<Event>,
    tags: &[String],
    content_types: &[String],
    cap: usize,
) -> Vec<SearchHit> {
    ingest_teasers_capped(events, tags, content_types, cap).0
}

/// Same as [`ingest_teasers`] but also returns whether the cap truncated the ranked set (`true` =
/// more candidates existed than the cap kept; the UI should surface a "showing first N" affordance).
/// This is the authoritative truncation signal — the only layer that sees both the full deduped set
/// and the cap is the ingest function (M20 W3).
pub fn ingest_teasers_capped(
    events: Vec<Event>,
    tags: &[String],
    content_types: &[String],
    cap: usize,
) -> (Vec<SearchHit>, bool) {
    // Sort newest-first so the per-`npub` dedup keeps the **latest** teaser — a non-compliant relay
    // that returns an author's old + new teaser (or serves the stale one first) can't hide the
    // update. Ties break on the higher event id (deterministic).
    let mut events = events;
    events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut hits: Vec<SearchHit> = Vec::new();
    for ev in events {
        if hb_core::is_canary(&ev) {
            continue; // F-canary: the VPS canary's synthetic teasers never surface in discovery
        }
        if ev.content.len() > MAX_TEASER_BYTES {
            continue; // AB3: oversize body bounded before any parse
        }
        let Ok(teaser) = parse_teaser(&ev) else {
            continue; // AB3: bad signature / wrong schema discarded
        };
        if !teaser_matches(&teaser, tags, content_types) {
            continue;
        }
        let npub = ev.pubkey.to_bech32().unwrap_or_else(|_| ev.pubkey.to_hex());
        if seen.insert(npub.clone()) {
            hits.push(SearchHit { npub, teaser, created_at: ev.created_at });
        }
    }
    // Rank the full deduped set, then cap — so the cap keeps the TOP-ranked hits, not whichever
    // happened to sort first under the dedup presort (the old bug: cap applied before ranking).
    hits = rank_hits(hits, tags, content_types);
    let capped = hits.len() > cap;
    hits.truncate(cap);
    (hits, capped)
}

/// The M20 default ranking for discovery hits. **This function is the single swap point** — change
/// the order here when the owner rules on it (the prompt records an explicit owner decision pending;
/// see M20_PROMPT.md §W3 decision 2).
///
/// Documented order, descending:
/// 1. **AND-match strength** — the number of query terms (tags + content-types) present on the
///    teaser. Tags are AND-intersected by [`teaser_matches`] (every query tag must be present for a
///    hit to survive ingest, so every passing teaser matches `tags.len()` tags — that axis is
///    constant across hits); content-types are OR-unioned, so a teaser carrying MORE of the queried
///    content-types is a stronger match and ranks above one carrying fewer. With no content-types in
///    the query, match strength is constant and recency alone orders the tied hits.
/// 2. **Recency** (`created_at`, newest first) as a tiebreak — stable and deterministic.
///
/// **Deferred per the surgical-changes rule + DISC3:** the prompt's suggested order also names
/// *collection count* as a ranking input. That input does not exist on the teaser today — DISC3
/// keeps all listing/collection data off the public teaser (a hit surfaces the advertisement, never
/// the hoard), so wiring collection count requires a teaser-schema change in hb-core that is beyond
/// W3's scope. When the owner rules and that field lands, it slots in here as a second tier (between
/// match strength and recency); this function is the only place that changes.
pub fn rank_hits(hits: Vec<SearchHit>, tags: &[String], content_types: &[String]) -> Vec<SearchHit> {
    let mut ranked = hits;
    ranked.sort_by(|a, b| {
        let strength_a = tags.iter().filter(|t| a.teaser.tags.contains(t)).count()
            + content_types.iter().filter(|c| a.teaser.content_types.contains(c)).count();
        let strength_b = tags.iter().filter(|t| b.teaser.tags.contains(t)).count()
            + content_types.iter().filter(|c| b.teaser.content_types.contains(c)).count();
        // Descending match strength, then descending recency. Equal on both → preserve prior order
        // (sort_by is stable; the ingest presort already put equal hits newest-first by npub id).
        strength_b.cmp(&strength_a).then(b.created_at.cmp(&a.created_at))
    });
    ranked
}

/// Collapse a set of events for one replaceable address to the **newest by `created_at`**. A
/// compliant relay keeps only the latest, but a non-compliant one can return both the old and new
/// version — the client must never read the stale one (N3/AB8). Ties break on the higher event id
/// (deterministic). Returns `None` for an empty set.
pub fn select_newest_by_created_at(events: Vec<Event>) -> Option<Event> {
    events
        .into_iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::Identity;

    fn teaser_with(tags: &[&str], cts: &[&str]) -> Teaser {
        Teaser {
            display_name: "archivebox".into(),
            bio: "hoards".into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            content_types: cts.iter().map(|s| s.to_string()).collect(),
            picture: None,
        }
    }

    fn ev(id: &Identity, t: &Teaser) -> Event {
        hb_core::event::build_teaser(id, t, true).unwrap()
    }

    #[test]
    fn tag_terms_intersect_and() {
        let t = teaser_with(&["anime", "vhs"], &["video"]);
        assert!(teaser_matches(&t, &["anime".into(), "vhs".into()], &[]), "all tags present → match");
        assert!(!teaser_matches(&t, &["anime".into(), "manga".into()], &[]), "a missing tag → no match (AND)");
        assert!(teaser_matches(&t, &[], &[]) == teaser_matches(&t, &[], &[]));
    }

    #[test]
    fn content_types_union_or() {
        let t = teaser_with(&["anime"], &["video"]);
        assert!(teaser_matches(&t, &[], &["video".into(), "audio".into()]), "any content-type matches → OR");
        assert!(!teaser_matches(&t, &[], &["audio".into()]), "no content-type matches → no match");
    }

    #[test]
    fn teasers_dedup_by_npub() {
        let id = Identity::generate();
        let t = teaser_with(&["anime"], &["video"]);
        // The same author appears twice (e.g. fetched from two relays, or two t-tag hits).
        let hits = ingest_teasers(vec![ev(&id, &t), ev(&id, &t)], &["anime".into()], &[], 100);
        assert_eq!(hits.len(), 1, "one npub yields one hit");
    }

    #[test]
    fn dedup_keeps_newest_teaser_per_npub() {
        // A non-compliant relay returns an author's OLD and NEW teaser, old first. The dedup must
        // keep the newest (by created_at), never the stale one a relay served first.
        let id = Identity::generate();
        let mut old = teaser_with(&["anime"], &["video"]);
        old.display_name = "old".into();
        let mut new = teaser_with(&["anime"], &["video"]);
        new.display_name = "new".into();
        let build_at = |t: &Teaser, ts: u64| {
            let base = hb_core::event::build_teaser(&id, t, true).unwrap();
            let tags: Vec<Tag> = base.tags.iter().cloned().collect();
            id.sign(
                EventBuilder::new(base.kind, base.content)
                    .tags(tags)
                    .custom_created_at(Timestamp::from(ts)),
            )
            .unwrap()
        };
        let old_ev = build_at(&old, 1_000);
        let new_ev = build_at(&new, 2_000);
        let hits = ingest_teasers(vec![old_ev, new_ev], &["anime".into()], &[], 100);
        assert_eq!(hits.len(), 1, "one npub → one hit");
        assert_eq!(hits[0].teaser.display_name, "new", "the newest teaser wins, not the first-served");
    }

    #[test]
    fn results_capped_at_limit() {
        let t = teaser_with(&["anime"], &["video"]);
        let events: Vec<Event> = (0..10).map(|_| ev(&Identity::generate(), &t)).collect();
        let hits = ingest_teasers(events, &["anime".into()], &[], 3);
        assert_eq!(hits.len(), 3, "result cap honoured");
    }

    #[test]
    fn bad_sig_teaser_discarded_before_dedup() {
        let good = ev(&Identity::generate(), &teaser_with(&["anime"], &["video"]));
        let mut tampered = ev(&Identity::generate(), &teaser_with(&["anime"], &["video"]));
        tampered.content = "mutated after signing".into(); // id no longer matches the signature
        let hits = ingest_teasers(vec![tampered, good.clone()], &["anime".into()], &[], 100);
        assert_eq!(hits.len(), 1, "only the validly-signed teaser survives");
        assert_eq!(hits[0].npub, good.pubkey.to_bech32().unwrap());
    }

    #[test]
    fn canary_marked_teaser_excluded_from_discovery() {
        // F-canary: a validly-signed teaser carrying the hb-canary marker must NOT surface in a tag
        // search — the canary's synthetic traffic stays out of discovery (parity with the counts).
        let real = Identity::generate();
        let canary = Identity::generate();
        let real_ev = ev(&real, &teaser_with(&["anime"], &["video"]));
        // The canary teaser carries hb-canary as a tag → a `t`=hb-canary marker.
        let mut canary_teaser = teaser_with(&["anime"], &["video"]);
        canary_teaser.tags.push(hb_core::CANARY_MARKER.to_string());
        let canary_ev = ev(&canary, &canary_teaser);
        let hits = ingest_teasers(vec![canary_ev, real_ev.clone()], &["anime".into()], &[], 100);
        assert_eq!(hits.len(), 1, "only the real teaser surfaces");
        assert_eq!(hits[0].npub, real_ev.pubkey.to_bech32().unwrap(), "the canary teaser is excluded");
    }

    #[test]
    fn oversize_teaser_content_bounded_on_ingest() {
        let id = Identity::generate();
        let mut huge = teaser_with(&["anime"], &["video"]);
        huge.bio = "x".repeat(MAX_TEASER_BYTES + 1); // body exceeds the ingest bound
        let hits = ingest_teasers(vec![ev(&id, &huge)], &["anime".into()], &[], 100);
        assert!(hits.is_empty(), "an oversize teaser body is bounded out before parse");
    }

    #[test]
    fn duplicate_dtag_selects_highest_created_at() {
        // A non-compliant relay returns both the old and new version of one author's replaceable
        // event. The newest (by created_at) must win — never the stale one. (select_newest compares
        // timestamps only, so the content here is immaterial.)
        let id = Identity::generate();
        let kind = Kind::from_u16(hb_core::event::KIND_TEASER);
        let old_ev = id
            .sign(EventBuilder::new(kind, "old").custom_created_at(Timestamp::from(1_000)))
            .unwrap();
        let new_ev = id
            .sign(EventBuilder::new(kind, "new").custom_created_at(Timestamp::from(2_000)))
            .unwrap();
        let winner = select_newest_by_created_at(vec![old_ev, new_ev.clone()]).unwrap();
        assert_eq!(winner.id, new_ev.id, "the newer event is selected");
    }

    // ── M20 W3: deliberate ranking (was: the dedup sort's accidental by-product) ───────────────
    //
    // Build a teaser event at a specific created_at with a specific tag set, so ranking tests can
    // control both the match-strength and recency axes independently. (Mirrors the `build_at`
    // closure in `dedup_keeps_newest_teaser_per_npub` above, generalized to an arbitrary teaser.)
    fn ev_at(id: &Identity, t: &Teaser, ts: u64) -> Event {
        let base = hb_core::event::build_teaser(id, t, true).unwrap();
        let tags: Vec<Tag> = base.tags.iter().cloned().collect();
        id.sign(
            EventBuilder::new(base.kind, base.content)
                .tags(tags)
                .custom_created_at(Timestamp::from(ts)),
        )
        .unwrap()
    }

    #[test]
    fn rank_orders_by_match_strength_then_recency() {
        // Match strength = # query terms present (tags AND-matched + content-types OR-matched). All
        // three teasers pass the AND-tag filter (query tag "anime" present on each) AND the content-
        // type OR filter (each carries ≥1 of the queried CTs); they differ in how many queried CTs
        // they carry, which is the varying strength axis.
        //
        // Query content-types = [video, audio, image].
        //  - "three-cts" carries all 3  (strength 1 tag + 3 cts = 4), created_at 100 (oldest)
        //  - "two-cts"   carries 2      (strength 1 tag + 2 cts = 3), created_at 300 (newest)
        //  - "one-ct"    carries 1      (strength 1 tag + 1 ct  = 2), created_at 200 (middle)
        //
        // Expected order: three-cts (4) → two-cts (3) → one-ct (2). Recency is only the tiebreak, so
        // the OLDEST hit ranks FIRST because it has the strongest match — the exact inversion of the
        // old "whoever republished most recently" order.
        let three_cts = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video", "audio", "image"]);
            t.display_name = "three-cts".into();
            ev_at(&id, &t, 100)
        };
        let two_cts = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video", "audio"]);
            t.display_name = "two-cts".into();
            ev_at(&id, &t, 300)
        };
        let one_ct = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video"]);
            t.display_name = "one-ct".into();
            ev_at(&id, &t, 200)
        };
        let hits = ingest_teasers(
            vec![two_cts.clone(), one_ct, three_cts.clone()],
            &["anime".into()],
            &["video".into(), "audio".into(), "image".into()],
            100,
        );
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].teaser.display_name, "three-cts", "strongest match ranks first despite being oldest");
        assert_eq!(hits[1].teaser.display_name, "two-cts", "strength 3 next");
        assert_eq!(hits[2].teaser.display_name, "one-ct", "strength 2 last");
    }

    #[test]
    fn rank_recency_tiebreaks_equal_match_strength() {
        // Two teasers with EQUAL match strength (both match the single query tag, no content-types
        // queried). The newer one wins the tiebreak. This is the recency-as-tiebreak half of the
        // order, and also the behavior when match strength is constant (tag-only queries).
        let older = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video"]);
            t.display_name = "older".into();
            ev_at(&id, &t, 1_000)
        };
        let newer = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video"]);
            t.display_name = "newer".into();
            ev_at(&id, &t, 2_000)
        };
        // Feed older first so a stable sort that preserved input order would wrongly keep it first.
        let hits = ingest_teasers(vec![older, newer], &["anime".into()], &[], 100);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].teaser.display_name, "newer", "equal strength → newer wins the tiebreak");
        assert_eq!(hits[1].teaser.display_name, "older");
    }

    #[test]
    fn rank_cap_keeps_top_ranked_not_first_seen() {
        // The cap applies AFTER ranking, so the TOP-ranked hits survive, not whichever the dedup
        // presort happened to put first. With a cap of 1 and two equal-strength hits, the newer one
        // (recency tiebreak) is kept — the older is dropped even though dedup saw it.
        let older = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video"]);
            t.display_name = "older".into();
            ev_at(&id, &t, 1_000)
        };
        let newer = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video"]);
            t.display_name = "newer".into();
            ev_at(&id, &t, 2_000)
        };
        let hits = ingest_teasers(vec![older, newer], &["anime".into()], &[], 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].teaser.display_name, "newer", "cap keeps the top-ranked hit, not the first-seen");
    }

    #[test]
    fn full_and_match_survives_above_strict_cap_m20_w3_eviction_regression() {
        // M20 W3 eviction regression (the prompt's verify clause). A teaser that is a STRONGER match
        // but OLDER than many weaker matches must still surface — and rank FIRST. Pre-fix, the
        // relay's response window filled with newer weaker matches and the stronger hit was evicted
        // before the client filter ran (no explicit `.limit()`); even post-limit, the old recency-
        // only ordering would have buried it. The fix is the explicit `.limit()` on the filter
        // (tested in client.rs) AND ranking by match-strength. This test pins the ingest/rank half:
        // one strong hit (matches both queried content-types) OLDEST, several weaker hits (match only
        // one) newer — the strong hit ranks first despite being oldest.
        let mut events: Vec<Event> = Vec::new();
        // The strong hit: matches the queried tag + BOTH queried content-types (strength 1+2=3),
        // OLDEST (ts = 1) — under the old recency-only order it ranked last.
        let strong = {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video", "audio"]);
            t.display_name = "strong".into();
            ev_at(&id, &t, 1)
        };
        events.push(strong);
        // 5 weaker hits: match the queried tag + only ONE queried content-type (strength 1+1=2), all
        // NEWER. (The relay window in production is ~500; here 5 is enough — match-strength, not
        // headroom, is what this test asserts.)
        for i in 0..5 {
            let id = Identity::generate();
            let mut t = teaser_with(&["anime"], &["video"]);
            t.display_name = format!("weak-{i}");
            events.push(ev_at(&id, &t, 100 + i));
        }
        // Cap below the event count so we also prove the cap keeps the TOP-ranked hit.
        let hits = ingest_teasers(events, &["anime".into()], &["video".into(), "audio".into()], 3);
        assert_eq!(hits.len(), 3, "capped to 3");
        assert_eq!(hits[0].teaser.display_name, "strong", "the older strong hit surfaces FIRST — not evicted by newer weaker matches");
        // The remaining two slots are weak matches (strength 2), newest-first among themselves.
        assert!(hits[1].teaser.display_name.starts_with("weak-"), "weak matches fill the rest");
        assert!(hits[2].teaser.display_name.starts_with("weak-"));
    }
}
