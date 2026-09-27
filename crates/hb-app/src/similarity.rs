//! QURATOR-347 slice B — the ONE pure similarity scorer.
//!
//! Ranks a peer against me by **overlap, never prominence** (owner, 2026-09-26: curated lists
//! are *"prone to just recommending the most prominent or most active. Should be most
//! semantically similar in terms of interests."*). Three signals feed it, all normalised with
//! existing helpers and compared as SETS:
//!
//! - **titles** — normalised with the ONE `hb_core::title_norm::normalize_title` (QURATOR-344),
//!   so the scorer, the title index and the holder counts can never disagree about identity.
//! - **interests** — the profile's `tags` (the wire field stays `tags`; the rename is UI copy).
//! - **collection tags** — a peer's public collections' `Collection.tags`; mine via
//!   `compute_collection_tags` (published public only — private tags never leak into a score).
//!
//! **Metric — Jaccard, chosen and justified:** `|A∩B| / |A∪B|`. The alternative, the overlap
//! coefficient `|A∩B| / min(|A|,|B|)`, is prominence-shaped: a 10,000-title library containing
//! my five would score 1.0 exactly like a true peer twin. Jaccard divides by the union, so
//! "contains my whole shelf plus a mountain more" ranks LOW — that is the wanted anti-prominence
//! bias, and it is symmetric.
//!
//! **Weights:** Interests/tags carry the whole score while I have no titles (cold start);
//! the moment my first published title exists, titles take over at
//! [`WEIGHT_TITLES`] 0.8 / [`WEIGHT_TAGS`] 0.2. Both consts carry the ticket's
//! "tune after seeding" note — they are constants, not config, until there is data to tune on.
//!
//! No network, no crypto — §5 does not fire.

use std::collections::HashSet;

use serde::Serialize;

/// Titles' weight once I have at least one published title (ticket: start 0.8; tune after seeding).
pub const WEIGHT_TITLES: f32 = 0.8;
/// Tags' weight (interests + collection tags combined) once I have titles; alone while I don't.
pub const WEIGHT_TAGS: f32 = 0.2;
/// Display cap on `shared_interests` (ticket: "capped, e.g. 5").
pub const SHARED_INTERESTS_CAP: usize = 5;

/// The plain inputs one side (me or a peer) contributes to a comparison. All members are
/// NORMALISED (titles via `normalize_title`, tags via [`norm_tag`]) — constructing this from
/// raw strings without normalising makes the comparison wrong, so the builders
/// (`commands::people`) and `title_index`'s author metadata are the only production callers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimilarityInputs {
    /// Normalised title keys of every published-public title the side holds.
    pub titles: HashSet<String>,
    /// Normalised interest (profile `tags`) tags.
    pub interest_tags: HashSet<String>,
    /// Normalised collection tags (public collections only).
    pub collection_tags: HashSet<String>,
}

impl SimilarityInputs {
    /// True when the side has nothing at all — the cold-start side: no published titles, no
    /// interests, no collection tags. A cold-start ME cannot be scored against anyone; the
    /// `similar_people` UI state is driven from this (`PeopleResult.cold_start`).
    pub fn is_empty(&self) -> bool {
        self.titles.is_empty() && self.interest_tags.is_empty() && self.collection_tags.is_empty()
    }
}

/// The named explanation returned with every score — the ticket's "NAMED cold-start reason".
/// Serialized snake_case for the TS side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityReason {
    /// Shared normalised titles exist — the titles' weight did the ranking work.
    TitlesInCommon,
    /// No shared titles (either side title-less); tags carried the score.
    InterestsOnly,
    /// I have no titles AND no interests and no collection tags at all — the score is 0 for
    /// everyone and the UI should say "pick your Interests, add a collection" (spec v5).
    ColdStartNoInterests,
}

/// One scored comparison. `shared_interests` shows the NORMALISED (lowercased, trimmed)
/// spelling — the comparison key, not the user's original casing; the UI copy should not claim
/// the other person's exact spelling.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Similarity {
    /// 0.0 ..= 1.0. 0.0 when nothing overlaps.
    pub score: f32,
    /// How many normalised titles we hold in common (the "you both have N of …" explanation).
    pub shared_titles: usize,
    /// The shared interest tags (interests only, not collection tags), capped at
    /// [`SHARED_INTERESTS_CAP`], sorted for determinism.
    pub shared_interests: Vec<String>,
    pub reason: SimilarityReason,
}

/// Normalise one tag for comparison: trimmed, lowercased. hb-core has no tag normaliser
/// (verified: no `tag_util`; tags are stored verbatim, sorted+deduped raw by
/// `compute_collection_tags`), so THIS is the one tag normaliser for scoring — if a tag
/// normaliser is ever added to hb-core, fold this into it rather than growing a second.
pub(crate) fn norm_tag(tag: &str) -> String {
    tag.trim().to_lowercase()
}

/// Normalise a tag list into the comparison set.
pub(crate) fn norm_tags(tags: &[String]) -> HashSet<String> {
    tags.iter()
        .map(|t| norm_tag(t))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Jaccard `|A∩B| / |A∪B|`, 0.0 when the union is empty.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let inter = a.intersection(b).count();
    if inter == 0 {
        return 0.0;
    }
    let union = a.union(b).count();
    inter as f32 / union as f32
}

/// Score one peer against me. Pure: no locks, no store, no I/O — callers pre-build the inputs.
pub fn similarity(mine: &SimilarityInputs, peer: &SimilarityInputs) -> Similarity {
    let shared_titles = mine.titles.intersection(&peer.titles).count();
    let title_score = jaccard(&mine.titles, &peer.titles);
    let my_tags = mine
        .interest_tags
        .union(&mine.collection_tags)
        .cloned()
        .collect::<HashSet<_>>();
    let peer_tags = peer
        .interest_tags
        .union(&peer.collection_tags)
        .cloned()
        .collect::<HashSet<_>>();
    let tag_score = jaccard(&my_tags, &peer_tags);

    // The cold-start switch: with zero titles of my own, tags carry 100%; with any title,
    // the ticket's 0.8/0.2 split applies (a title-less peer still scores on 0.2 × tags, so
    // tag-similar peers keep ranking above strangers as the index fills).
    let score = if mine.titles.is_empty() {
        tag_score
    } else {
        WEIGHT_TITLES * title_score + WEIGHT_TAGS * tag_score
    }
    .clamp(0.0, 1.0);

    // Display: shared INTEREST tags only (collection tags are scoring fuel, not explanation),
    // normalised spellings, sorted so the cap never depends on HashSet order.
    let mut shared_interests: Vec<String> = mine
        .interest_tags
        .intersection(&peer.interest_tags)
        .cloned()
        .collect();
    shared_interests.sort();
    shared_interests.truncate(SHARED_INTERESTS_CAP);

    let reason = if mine.is_empty() {
        SimilarityReason::ColdStartNoInterests
    } else if shared_titles > 0 {
        SimilarityReason::TitlesInCommon
    } else {
        SimilarityReason::InterestsOnly
    };

    Similarity { score, shared_titles, shared_interests, reason }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::title_norm::normalize_title;

    fn inputs(titles: &[&str], interests: &[&str], col_tags: &[&str]) -> SimilarityInputs {
        SimilarityInputs {
            titles: titles.iter().map(|t| t.to_string()).collect(),
            interest_tags: interests.iter().map(|t| t.to_string()).collect(),
            collection_tags: col_tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// P-10 mutation: in the consts of crates/hb-app/src/similarity.rs, change
    /// `pub const WEIGHT_TAGS: f32 = 0.2;` to `= 0.0` — this test reds (identical sets then
    /// score 0.8, not 1.0; the disjoint half stays 0.0, which is what discriminates this
    /// mutation from a blanket-score mutation).
    #[test]
    fn identical_sets_score_one_disjoint_score_zero() {
        let a = inputs(&["dune 1984"], &["sci-fi"], &["film"]);
        let b = inputs(&["dune 1984"], &["sci-fi"], &["film"]);
        let s = similarity(&a, &b);
        assert_eq!(s.reason, SimilarityReason::TitlesInCommon);
        assert!((s.score - 1.0).abs() < 1e-6, "identical: {}", s.score);

        let disjoint = inputs(&["other"], &["cooking"], &[]);
        let s = similarity(&a, &disjoint);
        assert_eq!(s.score, 0.0, "nothing shared: {}", s.score);
        assert_eq!(s.reason, SimilarityReason::InterestsOnly);
    }

    /// The cold-start switch: no titles of mine ⇒ score is exactly the tag score; adding my
    /// first title ⇒ the 0.8/0.2 formula takes over exactly.
    ///
    /// P-10 mutation: in `similarity` (crates/hb-app/src/similarity.rs), change
    /// `if mine.titles.is_empty() {` to `if false {` — this test reds (the no-titles branch
    /// asserts the exact tag-only score and gets the 0.8/0.2 formula instead).
    #[test]
    fn weights_switch_at_my_first_title() {
        // No titles: tag Jaccard carries everything — here 1 shared of 1∪2 union.
        let me = inputs(&[], &["sci-fi"], &[]);
        let peer = inputs(&[], &["sci-fi", "anime"], &[]);
        let s = similarity(&me, &peer);
        assert!((s.score - 1.0 / 2.0).abs() < 1e-6, "tag-only score: {}", s.score);
        assert_eq!(s.reason, SimilarityReason::InterestsOnly);

        // My first title arrives (shared): 0.8·(1/2) + 0.2·(1/2).
        let me = inputs(&["dune 1984"], &["sci-fi"], &[]);
        let peer = inputs(&["dune 1984", "neuromancer"], &["sci-fi", "anime"], &[]);
        let s = similarity(&me, &peer);
        let expected = WEIGHT_TITLES * (1.0 / 2.0) + WEIGHT_TAGS * (1.0 / 2.0);
        assert!((s.score - expected).abs() < 1e-6, "weighted score: {}", s.score);
        assert_eq!(s.reason, SimilarityReason::TitlesInCommon);
    }

    /// A title-less me ranks a tag-similar, title-heavy peer on tags alone — "Interests/tags
    /// carry the score when you have no collections yet".
    ///
    /// P-10 mutation: in `similarity` (crates/hb-app/src/similarity.rs), change
    /// `if mine.is_empty() {` to `if false {` — this test reds (the empty-me reason is no
    /// longer named ColdStartNoInterests).
    #[test]
    fn cold_start_me_is_named_and_tags_carry_the_score() {
        let me = inputs(&[], &[], &[]);
        let peer = inputs(&["a lot of titles"], &["sci-fi"], &[]);
        let s = similarity(&me, &peer);
        assert_eq!(s.score, 0.0, "empty me cannot score: {}", s.score);
        assert_eq!(s.reason, SimilarityReason::ColdStartNoInterests);
    }

    /// P-10 mutation: in `norm_tag` (crates/hb-app/src/similarity.rs), delete the `.trim()`
    /// call (leave `tag.to_lowercase()`) — this test reds (" sci-fi " with whitespace no
    /// longer equals the trimmed key).
    #[test]
    fn tags_are_compared_case_insensitively_and_trimmed() {
        // Built through the REAL production normaliser (`norm_tags`), never raw sets — the
        // harness drives the production body, never a re-implementation (§9).
        let me = SimilarityInputs {
            interest_tags: norm_tags(&[" Sci-Fi ".to_string()]),
            ..Default::default()
        };
        let peer = SimilarityInputs {
            interest_tags: norm_tags(&["sci-fi".to_string()]),
            ..Default::default()
        };
        let s = similarity(&me, &peer);
        assert_eq!(s.score, 1.0, "case+trim fold to one key: {}", s.score);
        assert_eq!(s.shared_interests, vec!["sci-fi".to_string()]);
    }

    /// Titles are keys from the ONE normaliser: "Dune (1984)" and "Dune (2021)" are different
    /// works and never shared, while spelling variants of the same work merge.
    ///
    /// P-10 mutation: in `similarity` (crates/hb-app/src/similarity.rs), change
    /// `mine.titles.intersection(&peer.titles).count()` to
    /// `mine.titles.union(&peer.titles).count()` — this test reds (shared_titles counts the
    /// union).
    #[test]
    fn titles_key_through_normalize_title_years_never_merge() {
        let me = inputs(
            &[&normalize_title("Dune (1984)"), &normalize_title("dune.1984")],
            &[],
            &[],
        );
        let peer = inputs(&[&normalize_title("Dune (2021)")], &["sci-fi"], &[]);
        let s = similarity(&me, &peer);
        assert_eq!(s.shared_titles, 0, "different years are different works");
        assert_eq!(s.reason, SimilarityReason::InterestsOnly);

        let twin = inputs(&[&normalize_title("DUNE 1984")], &[], &[]);
        let s = similarity(&me, &twin);
        assert_eq!(s.shared_titles, 1, "spelling variants are one key");
    }

    /// P-10 mutation: in the consts of crates/hb-app/src/similarity.rs, change
    /// `pub const SHARED_INTERESTS_CAP: usize = 5;` to `= 10` — this test reds (the cap
    /// assertion fails at 6).
    #[test]
    fn shared_interests_are_capped_and_sorted() {
        let me = inputs(&[], &["a", "b", "c", "d", "e", "f", "g"], &[]);
        let peer = inputs(&[], &["a", "b", "c", "d", "e", "f", "g"], &[]);
        let s = similarity(&me, &peer);
        assert_eq!(s.shared_interests.len(), SHARED_INTERESTS_CAP);
        assert_eq!(s.shared_interests, vec!["a", "b", "c", "d", "e"]);
    }

    /// Collection tags join interests in the tag score but never in the displayed
    /// shared_interests (the explanation is the INTEREST overlap).
    ///
    /// P-10 mutation: in `similarity` (crates/hb-app/src/similarity.rs), change
    /// `.intersection(&peer.interest_tags)` (the shared_interests collector) to
    /// `.intersection(&peer_tags)` — this test reds (the peer's collection-only tag shows
    /// up in the display list).
    #[test]
    fn collection_tags_score_but_never_display_as_interests() {
        let me = inputs(&[], &["sci-fi"], &["film"]);
        // Overlaps ONLY through collection tags: nothing may display as a shared interest.
        let peer = inputs(&[], &[], &["sci-fi"]);
        let s = similarity(&me, &peer);
        // Tag sets: mine {sci-fi, film} vs peer {sci-fi} — Jaccard 1/2, all from collection tags.
        assert!((s.score - 1.0 / 2.0).abs() < 1e-6, "combined tag sets score: {}", s.score);
        assert!(s.shared_interests.is_empty(), "collection tag must not display as an interest");
    }
}
