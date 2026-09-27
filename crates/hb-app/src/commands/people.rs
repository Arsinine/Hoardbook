//! QURATOR-347 slice B — `similar_people`: contacts ranked by overlap, never prominence.
//!
//! Candidates are the **stored contacts** (the ticket's "candidates = contacts for now");
//! non-contact strangers found by teaser discovery are a LATER slice — nothing here reads or
//! feeds discovery. My inputs are computed ONCE per call (they are shared by every candidate
//! and by `get_contacts`' similarity summaries); each candidate's inputs come from its
//! [`CachedPeer`] plus the in-memory title index (its readable listings). A Locked stranger
//! has no indexed titles — that is fine: they score on tags alone, exactly as the ticket
//! allows, and keep their Add button (owner ruling 2026-09-27).
//!
//! Cold start: when I have no titles AND no interests/collection tags, every score is 0, so
//! the wrapper carries `cold_start: true` for the UI's "pick your Interests, add a
//! collection" state. Zero scores are dropped even when NOT cold-start (a contact with no
//! overlap is not "similar").
//!
//! No network, no crypto — §5 does not fire.

use std::collections::HashSet;

use serde::Serialize;
use tauri::State;

use hb_core::fingerprint::Fingerprint;
use hb_core::title_norm::normalize_title;
use hb_core::types::{DirectoryItem, Visibility};

use crate::commands::browse::{ReadState, read_state_for};
use crate::error::{CmdResult, cmd_err};
use crate::identity_state::SharedIdentity;
use crate::similarity::{SimilarityInputs, SimilarityReason, norm_tags};
use crate::store::{CachedPeer, DataStore};

/// Default cap when no `limit` is passed. A `limit` may lower it, never raise it.
pub(crate) const MAX_PEERS: usize = 50;

/// MY similarity inputs: my published PUBLIC titles + my interests + my collection tags.
/// Two passes over the drafts (this title walk and `compute_collection_tags`) — deliberate:
/// the tags must come from the ONE production emitter so a private collection's tags can never
/// leak into a public comparison, and the walk needs the listings the tag fn never touches.
pub(crate) fn my_similarity_inputs(store: &DataStore) -> SimilarityInputs {
    let mut titles = HashSet::new();
    for slug in store.list_collection_slugs().unwrap_or_default() {
        if store.is_published(&slug) {
            if let Ok(Some(col)) = store.load_collection_draft(&slug) {
                if col.visibility == Visibility::Public {
                    collect_titles(&col.listing, &mut titles);
                }
            }
        }
    }
    let interest_tags = store
        .load_profile_draft()
        .ok()
        .flatten()
        .map(|p| norm_tags(&p.tags))
        .unwrap_or_default();
    let collection_tags = norm_tags(&crate::commands::profile::compute_collection_tags(store));
    SimilarityInputs { titles, interest_tags, collection_tags }
}

/// Walk a listing tree collecting every item's normalised title key (folders and files — a
/// folder title is a title). Empty normalisations are skipped.
fn collect_titles(items: &[DirectoryItem], out: &mut HashSet<String>) {
    for item in items {
        let norm = normalize_title(&item.name);
        if !norm.is_empty() {
            out.insert(norm);
        }
        collect_titles(&item.children, out);
    }
}

/// A PEER's similarity inputs: their titles come from the in-memory title index (readable
/// listings only — a Locked stranger has none and scores on tags), interests from the cached
/// profile `tags`, collection tags from their PUBLIC cached collections.
pub(crate) fn peer_similarity_inputs(peer: &CachedPeer) -> SimilarityInputs {
    SimilarityInputs {
        titles: crate::title_index::titles_of(&peer.npub),
        interest_tags: peer
            .profile
            .as_ref()
            .map(|p| norm_tags(&p.tags))
            .unwrap_or_default(),
        collection_tags: peer
            .collections
            .iter()
            .filter(|pc| pc.collection.visibility == Visibility::Public)
            .flat_map(|pc| norm_tags(&pc.collection.tags))
            .collect(),
    }
}

/// The combined (interests ∪ collection) normalised tag set of a peer — the tag metadata the
/// title index stores per author for the `search_titles` holder sort. Same inputs as
/// [`peer_similarity_inputs`], combined, so the two can never disagree about a peer's tags.
pub(crate) fn peer_tag_inputs(peer: &CachedPeer) -> HashSet<String> {
    let inputs = peer_similarity_inputs(peer);
    inputs
        .interest_tags
        .union(&inputs.collection_tags)
        .cloned()
        .collect()
}

/// One ranked contact.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarPerson {
    pub npub: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Fingerprint>,
    /// 0.0..=1.0 — see [`crate::similarity`].
    pub score: f32,
    pub shared_titles: usize,
    /// Capped shared interest tags (normalised spellings).
    pub shared_interests: Vec<String>,
    pub reason: SimilarityReason,
    /// The v5 size-rule read state — computed by the ONE [`read_state_for`], never re-derived.
    pub read_state: ReadState,
}

/// The wrapper: `cold_start` tells the UI "pick your Interests, add a collection" instead of
/// rendering an empty-looking list (decided: wrapper rather than a bare `Vec` — the state is a
/// DATA property of my inputs, not of the result length).
#[derive(Debug, Clone, Serialize)]
pub struct PeopleResult {
    pub people: Vec<SimilarPerson>,
    pub cold_start: bool,
}

/// Contacts ranked by similarity. `limit` may lower the [`MAX_PEERS`] cap.
#[tauri::command]
pub async fn similar_people(
    limit: Option<usize>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<PeopleResult> {
    let self_npub = identity.read().await.as_ref().map(|id| id.npub());
    similar_people_inner(&store, self_npub.as_deref(), limit)
}

/// The body of [`similar_people`], store-level and testable without a Tauri app.
fn similar_people_inner(
    store: &DataStore,
    self_npub: Option<&str>,
    limit: Option<usize>,
) -> CmdResult<PeopleResult> {
    let mine = my_similarity_inputs(store);
    let cold_start = mine.is_empty();
    let my_total = crate::commands::profile::total_published_public_bytes(store);
    // The auto-ask trace read failing ⇒ empty (same posture as `contact_summary_for`: it is a
    // de-dup memory, never correctness).
    let asked = store.load_auto_asks().unwrap_or_default();
    let cap = limit.unwrap_or(MAX_PEERS).clamp(1, MAX_PEERS);

    let mut scored: Vec<SimilarPerson> = Vec::new();
    for peer in store.list_contacts().map_err(cmd_err)? {
        // Exclude me, exclude zero scores; every surviving row is ranked, not prominent.
        if self_npub == Some(peer.npub.as_str()) {
            continue;
        }
        let s = crate::similarity::similarity(&mine, &peer_similarity_inputs(&peer));
        if s.score <= 0.0 {
            continue;
        }
        scored.push(SimilarPerson {
            display_name: peer
                .profile
                .as_ref()
                .map(|p| p.display_name.clone())
                .or_else(|| peer.petname.clone()),
            fingerprint: peer.fingerprint.clone(),
            npub: peer.npub.clone(),
            score: s.score,
            shared_titles: s.shared_titles,
            shared_interests: s.shared_interests,
            reason: s.reason,
            read_state: read_state_for(&peer, my_total, asked.contains(&peer.npub)),
        });
    }
    // Deterministic: score descending, then npub ascending — never HashMap order.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.npub.cmp(&b.npub))
    });
    scored.truncate(cap);
    Ok(PeopleResult { people: scored, cold_start })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::browse::PeerCollection;
    use crate::store::{ContactSource, ListingsStatus};
    use hb_core::types::{Collection, ItemType, Profile};

    fn item(name: &str) -> DirectoryItem {
        DirectoryItem {
            name: name.to_string(),
            item_type: ItemType::Folder,
            size: None,
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        }
    }

    fn col(slug: &str, visibility: Visibility, tags: &[&str], items: Vec<DirectoryItem>) -> Collection {
        Collection {
            slug: slug.to_string(),
            path_alias: slug.to_string(),
            description: None,
            item_count: items.len() as u64,
            est_size: None,
            content_types: vec![],
            tags: tags.iter().map(|t| t.to_string()).collect(),
            languages: vec![],
            visibility,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: items,
        }
    }

    fn profile(npub: &str, tags: &[&str]) -> Profile {
        Profile {
            display_name: format!("disp-{npub}"),
            bio: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            since: None,
            est_size: None,
            languages: vec![],
            contact_hint: None,
            email: None,
            location: None,
            social_links: vec![],
            willing_to: vec![],
            content_types: vec![],
            picture: None,
            hide_in_rosters: false,
            total_bytes: 100,
            updated: chrono::Utc::now(),
        }
    }

    fn peer_with(npub: &str, slug: &str, visibility: Visibility, tags: &[&str], items: Vec<DirectoryItem>) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source: ContactSource::Manual,
            browse_key_hex: None,
            petname: None,
            profile: Some(profile(npub, tags)),
            collections: vec![PeerCollection {
                collection: col(slug, visibility, tags, items),
                parts_total: None,
                parts_present: None,
                truncated: None,
                total_items: None,
                oversized: None,
                snapshot_fingerprint: Some("fp-1".to_string()),
                manifest_imported_at: None,
                teaser_event_id: None,
            }],
            listings_state: ListingsStatus::Fetched,
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        }
    }

    /// Seed: my profile (interests) + one published PUBLIC collection of mine; three contacts
    /// (A shares title+interest, B interest only, C nothing). Saves ride `save_contact`, which
    /// feeds the title index — the same production chokepoint.
    fn seed_store(dir: &std::path::Path) -> DataStore {
        let store = DataStore::new(dir.to_path_buf());
        store.save_profile_draft(&profile("me", &["sci-fi"])).unwrap();
        store
            .save_collection_draft(&col(
                "mine-films",
                Visibility::Public,
                &["film"],
                vec![item("Overlap Fixture Specimen")],
            ))
            .unwrap();
        store.save_published("mine-films", "{}").unwrap();
        for (npub, slug, vis, tags, items) in [
            ("hb_pp_sim_a", "a-films", Visibility::Public, &["sci-fi"][..], vec![item("Overlap Fixture Specimen")]),
            ("hb_pp_sim_b", "b-books", Visibility::Public, &["sci-fi"][..], vec![item("Unrelated Alpha")]),
            ("hb_pp_sim_c", "c-cook", Visibility::Public, &["cooking"][..], vec![item("Unrelated Beta")]),
        ] {
            store
                .save_contact(
                    &CachedPeer::pubkey_hash(npub),
                    &peer_with(npub, slug, vis, tags, items),
                )
                .unwrap();
        }
        store
    }

    /// P-10 mutation: in `similar_people_inner` (crates/hb-app/src/commands/people.rs), change
    /// the sort comparator `.then(a.npub.cmp(&b.npub))` line's score comparison
    /// `b.score.partial_cmp(&a.score)` to `a.score.partial_cmp(&b.score)` — this test reds
    /// (the lower-scoring contact ranks first).
    #[test]
    fn similar_people_orders_by_score_then_npub_and_drops_zero_scores() {
        let _g = crate::title_index::test_gate();
        let dir = tempfile::tempdir().unwrap();
        let store = seed_store(dir.path());
        let res = similar_people_inner(&store, Some("me"), None).unwrap();
        assert!(!res.cold_start);
        let names: Vec<&str> = res.people.iter().map(|p| p.npub.as_str()).collect();
        assert_eq!(names, vec!["hb_pp_sim_a", "hb_pp_sim_b"], "A (title+tag) over B (tag); C dropped: {names:?}");
        assert!(res.people[0].score > res.people[1].score);
        assert_eq!(res.people[0].reason, SimilarityReason::TitlesInCommon);
        assert_eq!(res.people[1].reason, SimilarityReason::InterestsOnly);
        assert_eq!(res.people[0].shared_interests, vec!["sci-fi".to_string()]);
    }

    /// P-10 mutation: in `similar_people_inner` (crates/hb-app/src/commands/people.rs), delete
    /// the `if self_npub == Some(peer.npub.as_str()) { continue; }` block — this test reds
    /// (the self row survives and appears in the results).
    #[test]
    fn self_row_is_excluded_even_when_saved_as_a_contact() {
        let _g = crate::title_index::test_gate();
        let dir = tempfile::tempdir().unwrap();
        let store = seed_store(dir.path());
        // A contact file that (pathologically) names me — the title index has the same guard.
        store
            .save_contact(
                &CachedPeer::pubkey_hash("me"),
                &peer_with("me", "self-films", Visibility::Public, &["sci-fi"][..], vec![item("Overlap Fixture Specimen")]),
            )
            .unwrap();
        let res = similar_people_inner(&store, Some("me"), None).unwrap();
        assert!(
            res.people.iter().all(|p| p.npub != "me"),
            "self must never be recommended: {:?}",
            res.people.iter().map(|p| p.npub.clone()).collect::<Vec<_>>()
        );
    }

    /// Cold start: no published collection and no interests ⇒ empty list + `cold_start: true`,
    /// even though contacts exist.
    ///
    /// P-10 mutation: in `similar_people_inner` (crates/hb-app/src/commands/people.rs), change
    /// `let cold_start = mine.is_empty();` to `let cold_start = false;` — this test reds (the
    /// wrapper reports cold_start: false for an empty-me store).
    #[test]
    fn cold_start_returns_empty_list_and_the_flag() {
        let _g = crate::title_index::test_gate();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_contact(
                &CachedPeer::pubkey_hash("hb_pp_sim_a"),
                &peer_with("hb_pp_sim_a", "a-films", Visibility::Public, &["sci-fi"][..], vec![item("Overlap Fixture Specimen")]),
            )
            .unwrap();
        let res = similar_people_inner(&store, Some("me"), None).unwrap();
        assert!(res.cold_start);
        assert!(res.people.is_empty(), "a cold-start me scores 0 against everyone");
    }

    /// The limit may lower the cap, never raise it past the candidate pool; ordering ties
    /// break on npub.
    ///
    /// P-10 mutation: in `similar_people_inner` (crates/hb-app/src/commands/people.rs), change
    /// `limit.unwrap_or(MAX_PEERS).clamp(1, MAX_PEERS)` to `usize::MAX` — this test reds (the
    /// capped result grows past the requested limit of 1).
    #[test]
    fn limit_caps_the_result() {
        let _g = crate::title_index::test_gate();
        let dir = tempfile::tempdir().unwrap();
        let store = seed_store(dir.path());
        let res = similar_people_inner(&store, Some("me"), Some(1)).unwrap();
        assert_eq!(res.people.len(), 1, "limit=1 keeps only the top-ranked contact");
        assert_eq!(res.people[0].npub, "hb_pp_sim_a");
    }

    /// read_state comes from the ONE `read_state_for` (v5 size rule), not a re-derivation:
    /// the bigger hoard (A, total 100 vs my 50 published) stays Locked with THEIR total.
    ///
    /// P-10 mutation: in `similar_people_inner` (crates/hb-app/src/commands/people.rs), change
    /// `read_state_for(&peer, my_total, asked.contains(&peer.npub))` to
    /// `read_state_for(&peer, u64::MAX, false)` — this test reds (A flips from Locked to
    /// Asked: the row stopped using the ONE v5 read-state computation's real inputs).
    #[test]
    fn read_state_rides_the_one_computation() {
        let _g = crate::title_index::test_gate();
        let dir = tempfile::tempdir().unwrap();
        let store = seed_store(dir.path());
        let res = similar_people_inner(&store, Some("me"), None).unwrap();
        let a = res.people.iter().find(|p| p.npub == "hb_pp_sim_a").unwrap();
        assert_eq!(a.read_state, ReadState::Locked { need_bytes: 100 }, "their 100 > my published total");
    }
}
