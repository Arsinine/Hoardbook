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
    //
    // QURATOR-44: the query keyword for fuzzy ranking is the joined tag terms (the tag input IS the
    // keyword the user typed); when only content-types are selected (no tags), the query is empty and
    // rank_hits falls back to a deterministic seeded shuffle. The seed is derived from the filter
    // terms so it is stable for the lifetime of one query — coherent pagination across pages.
    let query = tags.join(" ");
    let seed = hash_seed(tags, content_types);
    hits = rank_hits(hits, &query, seed);
    let capped = hits.len() > cap;
    hits.truncate(cap);
    (hits, capped)
}

/// The discovery ranking (QURATOR-44, owner ruling 2026-08-10). **This function is the single swap
/// point** for discovery order.
///
/// Two modes, selected by whether a typed `query` keyword is present:
///
/// 1. **Keyword search** (`query` non-empty) — fuzzy rank by trigram overlap between the query and
///    exactly the fields the teaser publishes about the person: `display_name` + `bio` + `tags` +
///    `content_types`. Fuzzy, NOT semantic: semantic search would need either a large local
///    embedding model in the installer or a remote API call that leaks the user's search terms —
///    both unacceptable here. Trigram overlap is local, small, dependency-free, and catches typos
///    and partial-word matches (e.g. "ani" matches "anime"). Recency (`created_at`, newest first) is
///    the stable tiebreak.
///
/// 2. **Type-toggle browse** (`query` empty, content-types only) — deterministic shuffle keyed on
///    `seed`. A fresh RNG per render would make pagination incoherent (page 2 reshuffles page 1), so
///    the seed MUST be stable for the lifetime of a query; the caller derives it from the query terms.
///
/// **Rank inputs are EXACTLY the teaser's published fields** — `display_name`, `bio`, `tags`,
/// `content_types`. Collection names, descriptions, and file/folder names are hoard contents sealed
/// under a browse-key and fenced by `scan_selective` (M8 F1); a searchable index IS a disclosure, so
/// they are deliberately absent (owner ruling 2026-08-10).
pub fn rank_hits(hits: Vec<SearchHit>, query: &str, seed: u64) -> Vec<SearchHit> {
    let query = query.trim();
    if query.is_empty() {
        // Type-toggle browse: deterministic shuffle so pagination is coherent across pages.
        shuffle_stable(hits, seed)
    } else {
        let qtri = trigrams(query);
        let mut scored: Vec<(i64, SearchHit)> = hits
            .into_iter()
            .map(|h| {
                let hay = h.teaser.display_name.to_lowercase()
                    + " "
                    + &h.teaser.bio.to_lowercase()
                    + " "
                    + &h.teaser.tags.join(" ").to_lowercase()
                    + " "
                    + &h.teaser.content_types.join(" ").to_lowercase();
                let score = overlap_score(&qtri, &trigrams(&hay));
                (score, h)
            })
            .collect();
        // Descending fuzzy score, then descending recency. sort_by is stable, so equal-score,
        // equal-recency hits preserve the ingest presort's newest-first-by-npub order.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.created_at.cmp(&a.1.created_at)));
        scored.into_iter().map(|(_, h)| h).collect()
    }
}

/// Lowercase trigram set for `s` (substrings of length 3). Strings shorter than 3 chars contribute
/// themselves as a single token so short queries still match. Empty input → empty set.
fn trigrams(s: &str) -> Vec<String> {
    let s = s.trim().to_lowercase();
    if s.len() < 3 {
        return if s.is_empty() { vec![] } else { vec![s] };
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<String> = Vec::with_capacity(chars.len().saturating_sub(2));
    let mut i = 0;
    while i + 3 <= chars.len() {
        out.push(chars[i..i + 3].iter().collect());
        i += 1;
    }
    out
}

/// Overlap score: the total count of query-trigram occurrences in the haystack (with multiplicity).
/// A teaser that mentions the keyword in BOTH its bio AND its tags outscores one that carries it
/// only as a tag, because the trigram appears more times. Catches partial-word and typo matches
/// (the fuzzy requirement) without an embedding model.
fn overlap_score(query: &[String], hay: &[String]) -> i64 {
    let mut score: i64 = 0;
    for qt in query {
        score += hay.iter().filter(|ht| ht == &qt).count() as i64;
    }
    score
}

/// A deterministic shuffle keyed on `seed`. Uses a simple xorshift PRNG (no new dependency) so the
/// same seed always produces the same order — which is what makes pagination coherent (page 2 never
/// reshuffles a page-1 item into view, or skips one). The Fisher-Yates walk is stable for equal keys.
fn shuffle_stable(mut hits: Vec<SearchHit>, seed: u64) -> Vec<SearchHit> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15); // splitmix64 offset — nonzero for seed 0
    let n = hits.len();
    for i in (1..n).rev() {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        hits.swap(i, j);
    }
    hits
}

/// FNV-1a hash of the filter terms → a u64 seed. Stable for the same query, different across
/// different queries — so each search's shuffle is its own, but pagination within one search is
/// coherent (the same query re-derives the same seed and reproduces the same order).
fn hash_seed(tags: &[String], content_types: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for t in tags {
        for b in t.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h ^= 0x2f; // '/' separator between the two axes
    for c in content_types {
        for b in c.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
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

    // ── QURATOR-44: fuzzy keyword ranking + seeded shuffle for type-toggle browse ─────────────
    //
    // Build a teaser event at a specific created_at with a specific tag set, so ranking tests can
    // control both the fuzzy-overlap and recency axes independently. (Mirrors the `build_at`
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
    fn rank_fuzzy_keyword_orders_by_overlap_then_recency() {
        // QURATOR-44: the query keyword drives fuzzy trigram overlap (with multiplicity) against each
        // teaser's published text (name + bio + tags + content_types). All three teasers pass the
        // AND-tag filter (tag "vhs" present on each). They differ in how MANY TIMES "vhs" trigrams
        // appear across name+bio+tags: the trigram "vhs" appears once per occurrence of the word.
        //  - "vhs-vhs"  → name "vhs-vhs" has "vhs" twice in the name → more occurrences → higher score
        //  - "vhs-once" → name has "vhs" once → middling score
        //  - "bluray"   → name has no "vhs"; only the tag carries it → lowest score
        // "bluray" is NEWEST so recency alone would rank it first; the fuzzy overlap inverts that.
        let strong = {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video"]);
            t.display_name = "vhs-vhs".into(); // "vhs" appears twice in the name
            t.bio = "vhs tapes".into();        // and again in bio
            ev_at(&id, &t, 100) // OLDEST
        };
        let mid = {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video"]);
            t.display_name = "vhs-once".into();
            ev_at(&id, &t, 200)
        };
        let weak = {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video"]);
            t.display_name = "bluray".into(); // only the tag carries "vhs"
            ev_at(&id, &t, 300) // NEWEST
        };
        let hits = ingest_teasers(vec![weak, mid, strong.clone()], &["vhs".into()], &[], 100);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].teaser.display_name, "vhs-vhs", "highest trigram multiplicity ranks first despite being oldest");
        assert_eq!(hits[2].teaser.display_name, "bluray", "lowest multiplicity last despite being newest");
    }

    #[test]
    fn rank_recency_tiebreaks_equal_fuzzy_score() {
        // Two teasers with EQUAL fuzzy overlap (both carry the "anime" tag, query "anime"). The
        // newer one wins the tiebreak. This is the recency-as-tiebreak half of the order, and the
        // behavior when fuzzy score is constant.
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
        assert_eq!(hits[0].teaser.display_name, "newer", "equal score → newer wins the tiebreak");
        assert_eq!(hits[1].teaser.display_name, "older");
    }

    #[test]
    fn rank_cap_keeps_top_ranked_not_first_seen() {
        // The cap applies AFTER ranking, so the TOP-ranked hits survive, not whichever the dedup
        // presort happened to put first. With a cap of 1 and two equal-score hits, the newer one
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
        // M20 W3 eviction regression. A teaser that is a STRONGER fuzzy match but OLDER than many
        // weaker matches must still surface — and rank FIRST. The strong hit's BIO repeats the query
        // keyword ("vhs") giving it strictly more trigram occurrences than the weak hits (whose bios
        // do not mention it), OLDEST; the 5 weaker hits all pass the tag filter but have lower
        // multiplicity, all NEWER. The strong hit ranks first despite being oldest.
        let mut events: Vec<Event> = Vec::new();
        let strong = {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video", "audio"]);
            t.display_name = "strong".into();
            t.bio = "vhs tapes vhs archive vhs".into(); // "vhs" repeated in bio
            ev_at(&id, &t, 1)
        };
        events.push(strong);
        for i in 0..5 {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video"]);
            t.display_name = format!("weak-{i}");
            t.bio = "just a hoarder".into(); // no "vhs" bio mention; only the tag carries it
            events.push(ev_at(&id, &t, 100 + i));
        }
        let hits = ingest_teasers(events, &["vhs".into()], &["video".into(), "audio".into()], 3);
        assert_eq!(hits.len(), 3, "capped to 3");
        assert_eq!(hits[0].teaser.display_name, "strong", "the older strong hit surfaces FIRST — not evicted by newer weaker matches");
        assert!(hits[1].teaser.display_name.starts_with("weak-"), "weak matches fill the rest");
        assert!(hits[2].teaser.display_name.starts_with("weak-"));
    }

    #[test]
    fn rank_fuzzy_matches_bio_not_just_tags() {
        // QURATOR-44 mutation guard: the fuzzy rank inputs are display_name + bio + tags +
        // content_types — NOT tags alone. Both teasers pass the AND-tag filter (query tag "vhs"
        // present on each as a tag) and share the SAME neutral display_name, so name+tag overlap is
        // equal. They differ ONLY in BIO: one bio mentions "vhs" (adding trigram occurrences), the
        // other does not. The bio-mention teaser must rank above the other despite being OLDER. This
        // is the test that REDS when the ranker ignores bio (mutation probe 1).
        let bio_vhs = {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video"]);
            t.display_name = "hoarder".into(); // neutral — no "vhs" in the name
            t.bio = "vhs tapes archive".into(); // "vhs" appears in the bio → extra trigram occurrences
            ev_at(&id, &t, 100) // OLDEST
        };
        let bio_none = {
            let id = Identity::generate();
            let mut t = teaser_with(&["vhs"], &["video"]);
            t.display_name = "hoarder".into(); // same neutral name
            t.bio = "bluray only".into(); // no "vhs" in bio; only the tag carries it
            ev_at(&id, &t, 300) // NEWEST — recency alone would rank it first
        };
        let hits = ingest_teasers(vec![bio_none, bio_vhs.clone()], &["vhs".into()], &[], 100);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].teaser.bio, "vhs tapes archive", "bio mention of 'vhs' ranks it above a newer hit without it");
    }

    // ── QURATOR-44: type-toggle browse uses a deterministic seeded shuffle ────────────────────

    /// Build N teaser events that all pass the content-type OR filter (each carries the "video"
    /// content-type), for testing the type-toggle shuffle path. Each gets a distinct name + npub.
    fn events_for_shuffle(n: usize) -> Vec<Event> {
        (0..n)
            .map(|i| {
                let id = Identity::generate();
                let mut t = teaser_with(&[], &["video"]);
                t.display_name = format!("hit-{i}");
                ev_at(&id, &t, 100 + i as u64)
            })
            .collect()
    }

    #[test]
    fn type_toggle_shuffle_is_deterministic_for_same_query() {
        // Same filter terms → same seed → same order. Two identical searches must produce identical
        // rankings, or pagination is incoherent.
        let events = events_for_shuffle(20);
        let a = ingest_teasers(events.clone(), &[], &["video".into()], 100);
        let b = ingest_teasers(events, &[], &["video".into()], 100);
        assert_eq!(a.len(), b.len());
        let names_a: Vec<&str> = a.iter().map(|h| h.teaser.display_name.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|h| h.teaser.display_name.as_str()).collect();
        assert_eq!(names_a, names_b, "same query → identical shuffle order");
    }

    #[test]
    fn type_toggle_pagination_is_coherent_no_dupes_no_skips() {
        // QURATOR-44 pagination coherence: the seed is stable per query, so slicing the ranked set
        // into pages of 10 never repeats an item across pages and never skips one. Paginating twice
        // produces the same partition. This is the test the ruling demands.
        let events = events_for_shuffle(25);
        let ranked = ingest_teasers(events, &[], &["video".into()], 100);
        assert_eq!(ranked.len(), 25, "all 25 survive the content-type filter");

        // Page 1 (0..10) and page 2 (10..20) from the SAME ranked vector.
        let page1: Vec<&str> = ranked.iter().take(10).map(|h| h.teaser.display_name.as_str()).collect();
        let page2: Vec<&str> = ranked.iter().skip(10).take(10).map(|h| h.teaser.display_name.as_str()).collect();
        let page3: Vec<&str> = ranked.iter().skip(20).take(10).map(|h| h.teaser.display_name.as_str()).collect();

        // No item appears on two pages.
        let all: Vec<&str> = page1.iter().chain(page2.iter()).chain(page3.iter()).copied().collect();
        let mut seen = std::collections::HashSet::new();
        for name in &all {
            assert!(seen.insert(*name), "{name:?} appeared on more than one page — pagination is incoherent");
        }
        // No item is skipped: the union of pages covers all 25 ranked hits, in order.
        let ranked_names: Vec<&str> = ranked.iter().map(|h| h.teaser.display_name.as_str()).collect();
        assert_eq!(all, ranked_names, "pages must partition the ranked set without reordering or skipping");
        // Re-running the same search reproduces the same partition (seed stability).
        let events2 = events_for_shuffle(25);
        let ranked2 = ingest_teasers(events2, &[], &["video".into()], 100);
        let names2: Vec<&str> = ranked2.iter().map(|h| h.teaser.display_name.as_str()).collect();
        assert_eq!(ranked_names, names2, "re-running the search reproduces the same order");
    }

    #[test]
    fn type_toggle_shuffle_differs_from_natural_order() {
        // The shuffle must actually shuffle — if the output equals the ingest presort order for a
        // non-trivial input, the shuffler is a no-op. With 20 items in ascending created_at order,
        // at least one item must move from its original position. Compare ranked names against the
        // original `hit-0`..`hit-19` sequence.
        let events = events_for_shuffle(20);
        let ranked = ingest_teasers(events, &[], &["video".into()], 100);
        let names: Vec<String> = ranked.iter().map(|h| h.teaser.display_name.clone()).collect();
        let original: Vec<String> = (0..20).map(|i| format!("hit-{i}")).collect();
        let moved = names.iter().zip(original.iter()).filter(|(a, b)| a != b).count();
        assert!(moved > 0, "shuffle must differ from natural order; got identical order for 20 items");
    }

    #[test]
    fn rank_hits_pure_function_no_network() {
        // rank_hits is a pure function — table-driven testable with no relay. Construct hits in
        // memory, rank by a keyword, assert order. (No network, no Identity, no Event.)
        let mk = |name: &str, bio: &str, ts: u64| SearchHit {
            npub: format!("npub-{name}"),
            teaser: Teaser {
                display_name: name.into(),
                bio: bio.into(),
                tags: vec!["anime".into()],
                content_types: vec!["video".into()],
                picture: None,
            },
            created_at: Timestamp::from(ts),
        };
        let hits = vec![
            mk("z-last", "nothing relevant", 9_000),
            mk("a-first", "anime collector", 1_000),
        ];
        let ranked = rank_hits(hits, "anime collector", 0);
        assert_eq!(ranked[0].teaser.display_name, "a-first", "fuzzy overlap beats recency");
        assert_eq!(ranked[1].teaser.display_name, "z-last");
    }
}
