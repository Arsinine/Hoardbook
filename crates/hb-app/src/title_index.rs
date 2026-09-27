//! QURATOR-347 slice A — the in-memory title index.
//!
//! One process-global index of every listing row we can read, keyed by the ONE normaliser
//! (`hb_core::title_norm::normalize_title`, QURATOR-344) so the index, holder counts and the
//! future similarity sort can never disagree about whether two names are the same title.
//! SQLite was considered and dropped (owner): at seeding scale memory is enough.
//!
//! Lifecycle, by ruling:
//! - **Rebuilt at launch** from the on-disk listing cache (`rebuild_from_store`, called once
//!   from `lib.rs` setup after `restore_identity`).
//! - **Updated on every successful decrypt** — actually: on every successful `save_contact`,
//!   which is THE write chokepoint a decrypt-and-cache rides (`refresh_contact_inner` →
//!   `save_contact`), so background harvest rides the existing enumeration scheduler's own
//!   saves. There is NO second loop — a second loop would double the background rate against
//!   the "queued and dragged out over a long period, never a burst" ruling (QURATOR-332).
//! - **Nothing is published or broadcast**; a server indexes nothing. Purely local, in memory.
//!
//! Rows are owned per `(author_npub, slug)`: an upsert REPLACES that collection's rows rather
//! than appending, so a refreshed collection never double-counts holders. Only PUBLIC-visibility
//! collections are indexed (the safe default for counts shown elsewhere; a private listing we
//! can read stays out of the index).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use hb_core::title_norm::normalize_title;
use hb_core::types::{DirectoryItem, ItemType, Visibility};
use serde::Serialize;

use crate::store::{CachedPeer, DataStore};

/// Hard caps, documented for the UI copy: at most 50 titles and 20 holders per title per query.
/// A `limit` argument may lower these, never raise them. The honest limit the UI must state:
/// **holder counts cover only people you can read** — the index is fed from contacts' cached
/// listings, nothing more.
pub(crate) const MAX_TITLES: usize = 50;
pub(crate) const MAX_HOLDERS: usize = 20;

/// One indexed row. `name_norm` is the key; the rest is display data.
/// The ticket's row schema pins (author, slug, fingerprint, path, name_norm, name, item_type,
/// size, format) — `fingerprint`/`item_type`/`size`/`format` have no reader until P3b
/// (similarity) and P4 (render) consume them; allowed here rather than dropping mandated
/// schema. Same pattern as `remove_author` below.
#[derive(Debug, Clone)]
#[allow(dead_code)] // the four P3b/P4-consumed fields above; everything else is read
struct Row {
    author_npub: String,
    slug: String,
    /// The collection's full-tree snapshot fingerprint when the listing carried one (M16 W4);
    /// `None` for a listing without the marker — rows are still keyed by (author, slug).
    fingerprint: Option<String>,
    /// Collection-relative path: `name` for a top-level item, `parent/name` for a child.
    path: String,
    name_norm: String,
    name: String,
    item_type: String,
    size: Option<String>,
    format: Option<String>,
}

/// Per-author display data the holder rows need (fed from the contact's profile by
/// [`contact_saved`]).
#[derive(Debug, Clone, Default)]
struct AuthorMeta {
    display_name: Option<String>,
    /// v5 (QURATOR-345) — the peer's teaser-computed total. Placeholder holder-sort signal
    /// until P3b replaces it with similarity.
    total_bytes: u64,
}

#[derive(Debug, Default)]
struct Index {
    /// name_norm → rows sharing that key (authors may repeat; a holder is a DISTINCT author).
    by_title: HashMap<String, Vec<Arc<Row>>>,
    /// (author npub, slug) → that collection's rows. THE ownership map: an upsert drops these
    /// from `by_title` before inserting the replacement, which is what makes replace-not-append.
    owned: HashMap<(String, String), Vec<Arc<Row>>>,
    authors: HashMap<String, AuthorMeta>,
    /// My own npub, when known — rows authored by it are dropped (a contact file that somehow
    /// names me must never put my own drafts into a "who else has this" count).
    self_npub: Option<String>,
}

static INDEX: LazyLock<RwLock<Index>> = LazyLock::new(|| RwLock::new(Index::default()));

/// Stamp the self-npub guard (called once at setup, after `restore_identity`) and purge any
/// rows already attributed to it.
pub(crate) fn set_self_npub(npub: Option<String>) {
    let mut idx = INDEX.write().expect("title index lock poisoned");
    if let Some(me) = npub.as_deref() {
        let slugs: Vec<(String, String)> = idx
            .owned
            .keys()
            .filter(|(a, _)| a == me)
            .cloned()
            .collect();
        for key in slugs {
            drop_rows_locked(&mut idx, &key.0, &key.1);
        }
    }
    idx.self_npub = npub;
}

/// The hook `DataStore::save_contact` calls after a successful persist. This is the ONE
/// consumer path into the index from production: it iterates the contact's collections and
/// re-indexes each. The chokepoint's call form appears exactly once in production — inside
/// this function — pinned by the one-site source-scan test at the bottom of this file.
pub(crate) fn contact_saved(peer: &CachedPeer) {
    let display = peer
        .profile
        .as_ref()
        .map(|p| p.display_name.clone())
        .or_else(|| peer.petname.clone());
    let total_bytes = peer.profile.as_ref().map(|p| p.total_bytes).unwrap_or(0);
    note_author(&peer.npub, display, total_bytes);
    for pc in &peer.collections {
        match pc.collection.visibility {
            Visibility::Public => upsert_listing(
                &peer.npub,
                &pc.collection.slug,
                &pc.collection.listing,
                pc.snapshot_fingerprint.as_deref(),
            ),
            // A private listing we can read stays out of the index — the safe default for
            // counts shown to the user until the disclosure question is ruled on.
            Visibility::Private => remove_collection(&peer.npub, &pc.collection.slug),
        }
    }
}

/// Record a contact's display metadata (display name, v5 total_bytes for the placeholder
/// holder sort). Called only from [`contact_saved`].
fn note_author(npub: &str, display_name: Option<String>, total_bytes: u64) {
    let mut idx = INDEX.write().expect("title index lock poisoned");
    idx.authors.insert(
        npub.to_string(),
        AuthorMeta { display_name, total_bytes },
    );
}

/// Replace (never append) the rows for one (author, slug) collection. THE chokepoint every
/// consumer goes through; production's single call site is [`contact_saved`], driven by
/// `save_contact` — which the enumeration scheduler's refresh path already calls, so harvest
/// rides the existing 30 s-spaced loop and no new scheduler exists.
pub(crate) fn upsert_listing(
    author_npub: &str,
    slug: &str,
    items: &[DirectoryItem],
    fingerprint: Option<&str>,
) {
    let mut rows = Vec::new();
    walk_items(items, "", author_npub, slug, fingerprint, &mut rows);

    let mut idx = INDEX.write().expect("title index lock poisoned");
    // The self guard: a contact file naming my own npub contributes nothing.
    if idx.self_npub.as_deref() == Some(author_npub) {
        drop_rows_locked(&mut idx, author_npub, slug);
        return;
    }
    drop_rows_locked(&mut idx, author_npub, slug);
    for row in rows {
        idx.by_title.entry(row.name_norm.clone()).or_default().push(Arc::clone(&row));
        idx.owned
            .entry((author_npub.to_string(), slug.to_string()))
            .or_default()
            .push(row);
    }
}

/// Remove one collection's rows (private listings skip the index through here).
fn remove_collection(author_npub: &str, slug: &str) {
    let mut idx = INDEX.write().expect("title index lock poisoned");
    drop_rows_locked(&mut idx, author_npub, slug);
}

/// Pull one (author, slug) collection's rows out of both maps. Caller holds the write lock.
fn drop_rows_locked(idx: &mut Index, author_npub: &str, slug: &str) {
    let key = (author_npub.to_string(), slug.to_string());
    if let Some(old) = idx.owned.remove(&key) {
        // Visit only the title keys this collection actually held — never a scan of the whole
        // index, which would make every save O(index) and the launch rebuild quadratic.
        for row in &old {
            if let Some(rows) = idx.by_title.get_mut(&row.name_norm) {
                rows.retain(|r| !Arc::ptr_eq(r, row));
                if rows.is_empty() {
                    idx.by_title.remove(&row.name_norm);
                }
            }
        }
    }
}

/// Depth-first walk of a listing tree into flat rows. Normalises the item NAME (a file or
/// folder name), never the full path — the path is carried as display data.
fn walk_items(
    items: &[DirectoryItem],
    parent: &str,
    author_npub: &str,
    slug: &str,
    fingerprint: Option<&str>,
    out: &mut Vec<Arc<Row>>,
) {
    for item in items {
        let path = if parent.is_empty() {
            item.name.clone()
        } else {
            format!("{parent}/{}", item.name)
        };
        out.push(Arc::new(Row {
            author_npub: author_npub.to_string(),
            slug: slug.to_string(),
            fingerprint: fingerprint.map(str::to_string),
            path: path.clone(),
            name_norm: normalize_title(&item.name),
            name: item.name.clone(),
            item_type: match item.item_type {
                ItemType::Folder => "Folder".to_string(),
                ItemType::File => "File".to_string(),
            },
            size: item.size.clone(),
            format: item.format.clone(),
        }));
        walk_items(&item.children, &path, author_npub, slug, fingerprint, out);
    }
}

/// Rebuild the whole index from the on-disk listing cache — called ONCE at launch, when the
/// index is empty. `list_contacts` is where every decrypted listing already lives
/// (`CachedPeer.collections`), so this is "rebuilt at launch" with no new persistence.
/// Returns the number of distinct title keys indexed.
pub(crate) fn rebuild_from_store(store: &DataStore) -> usize {
    for peer in store.list_contacts().unwrap_or_default() {
        contact_saved(&peer);
    }
    INDEX.read().expect("title index lock poisoned").by_title.len()
}

/// Drop every row of one author — the unfollow affordance, called from `DataStore::delete_contact`
/// (the delete twin of the `save_contact` hook), so an unfollowed contact stops counting as a
/// holder immediately.
pub(crate) fn remove_author(npub: &str) {
    let mut idx = INDEX.write().expect("title index lock poisoned");
    let slugs: Vec<(String, String)> = idx
        .owned
        .keys()
        .filter(|(a, _)| a == npub)
        .cloned()
        .collect();
    for (a, s) in slugs {
        drop_rows_locked(&mut idx, &a, &s);
    }
    idx.authors.remove(npub);
}

/// One holder of a title: a distinct author with a representative (slug, path).
#[derive(Debug, Clone, Serialize)]
pub struct TitleHolder {
    pub npub: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub slug: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleHit {
    /// A representative display name for the key (the first row stored under it).
    pub title: String,
    pub name_norm: String,
    /// DISTINCT authors holding this title — the true count, computed BEFORE the holder cap.
    pub holder_count: usize,
    /// Capped at [`MAX_HOLDERS`]; `holder_count` is the honest total.
    pub holders: Vec<TitleHolder>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleSearchResult {
    pub hits: Vec<TitleHit>,
    /// True when more matching titles existed than the cap returned.
    pub truncated: bool,
}

/// Search the index. The query goes through the SAME `normalize_title` as the rows; matching
/// is the exact key first, then substring matches on keys. Sorted: exact hit first, then
/// holder_count descending, then key ascending (deterministic — never HashMap order).
pub(crate) fn search_titles(query: &str, limit: Option<usize>) -> TitleSearchResult {
    let norm = normalize_title(query);
    if norm.is_empty() {
        return TitleSearchResult { hits: vec![], truncated: false };
    }
    let cap = limit.unwrap_or(MAX_TITLES).clamp(1, MAX_TITLES);
    let idx = INDEX.read().expect("title index lock poisoned");

    // (key, holder_count) for the exact key first, then every substring key.
    let mut candidates: Vec<(String, usize)> = Vec::new();
    if let Some(rows) = idx.by_title.get(&norm) {
        candidates.push((norm.clone(), distinct_authors(rows)));
    }
    let mut subs: Vec<(String, usize)> = idx
        .by_title
        .iter()
        .filter(|(k, _)| k.contains(&norm) && *k != &norm)
        .map(|(k, rows)| (k.clone(), distinct_authors(rows)))
        .collect();
    subs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    candidates.extend(subs);

    let truncated = candidates.len() > cap;
    let hits = candidates
        .into_iter()
        .take(cap)
        .map(|(key, count)| build_hit(&idx, key, count))
        .collect();
    TitleSearchResult { hits, truncated }
}

fn distinct_authors(rows: &[Arc<Row>]) -> usize {
    let mut seen = std::collections::HashSet::new();
    rows.iter().for_each(|r| {
        seen.insert(r.author_npub.as_str());
    });
    seen.len()
}

fn build_hit(idx: &Index, key: String, count: usize) -> TitleHit {
    let rows = &idx.by_title[&key];
    let title = rows[0].name.clone();
    // Holder = DISTINCT author; the representative (slug, path) is the first row seen for
    // that author under this key.
    let mut seen = std::collections::HashSet::new();
    let mut holders: Vec<TitleHolder> = rows
        .iter()
        .filter_map(|r| {
            if seen.insert(r.author_npub.as_str()) {
                Some(TitleHolder {
                    npub: r.author_npub.clone(),
                    display_name: idx
                        .authors
                        .get(&r.author_npub)
                        .and_then(|m| m.display_name.clone()),
                    slug: r.slug.clone(),
                    path: r.path.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    // P3b: replace with similarity. Placeholder holder order: the peer's v5 total_bytes
    // descending (bigger hoard first), then npub ascending for determinism.
    holders.sort_by(|a, b| {
        let tb = |np: &str| idx.authors.get(np).map(|m| m.total_bytes).unwrap_or(0);
        tb(&b.npub).cmp(&tb(&a.npub)).then(a.npub.cmp(&b.npub))
    });
    holders.truncate(MAX_HOLDERS);
    TitleHit { title, name_norm: key, holder_count: count, holders }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::browse::PeerCollection;
    use crate::store::{ContactSource, ListingsStatus};
    use hb_core::types::{Collection, Profile, Visibility};

    /// Unique keys per test would still collide with `clear_for_tests` and `set_self_npub`,
    /// which touch GLOBAL state mid-run while sibling tests hold rows in the index — the
    /// 2026-09-27 flake this gate fixes (the rebuild test wiped a parallel test's rows).
    /// Every index-touching test holds the gate for its whole body.
    static TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn gate() -> std::sync::MutexGuard<'static, ()> {
        TEST_GATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn item(name: &str, kind: ItemType) -> DirectoryItem {
        DirectoryItem {
            name: name.to_string(),
            item_type: kind,
            size: None,
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        }
    }

    fn peer_with(npub: &str, slug: &str, visibility: Visibility, items: Vec<DirectoryItem>) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source: ContactSource::Manual,
            browse_key_hex: None,
            petname: None,
            profile: Some(Profile {
                display_name: format!("disp-{npub}"),
                bio: None,
                tags: vec![],
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
                total_bytes: 500,
                updated: chrono::Utc::now(),
            }),
            collections: vec![PeerCollection {
                collection: Collection {
                    slug: slug.to_string(),
                    path_alias: slug.to_string(),
                    description: None,
                    item_count: items.len() as u64,
                    est_size: None,
                    content_types: vec![],
                    tags: vec![],
                    languages: vec![],
                    visibility,
                    sorted: false,
                    last_updated: chrono::Utc::now(),
                    listing: items,
                },
                parts_total: None,
                parts_present: None,
                truncated: None,
                total_items: None,
                oversized: None,
                snapshot_fingerprint: Some("fp-123".to_string()),
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

    fn hit_count(query: &str) -> usize {
        search_titles(query, None).hits.len()
    }

    /// P-10 mutation: in `contact_saved` (crates/hb-app/src/title_index.rs), change the
    /// `Visibility::Public` arm to call `remove_collection(...)` instead of
    /// `upsert_listing(...)` — this test reds (nothing is ever indexed).
    #[test]
    fn public_collections_are_indexed_private_are_not() {
        let _g = gate();
        contact_saved(&peer_with(
            "hb_ti_pub_a",
            "ti-pub-films",
            Visibility::Public,
            vec![item("The Public Corpus", ItemType::Folder)],
        ));
        contact_saved(&peer_with(
            "hb_ti_priv_a",
            "ti-priv-films",
            Visibility::Private,
            vec![item("The Private Corpus", ItemType::Folder)],
        ));
        assert_eq!(hit_count("public corpus"), 1);
        assert_eq!(hit_count("private corpus"), 0);
    }

    /// P-10 mutation: in `upsert_listing` (crates/hb-app/src/title_index.rs), change
    /// `drop_rows_locked(&mut idx, author_npub, slug);` (the unconditional one before the
    /// insert loop) to a no-op — this test reds (the second upsert's rows append to the
    /// first's and the paths/holder rows double up).
    #[test]
    fn upsert_replaces_not_appends() {
        let _g = gate();
        contact_saved(&peer_with(
            "hb_ti_rep_a",
            "ti-rep-films",
            Visibility::Public,
            vec![item("Replacement Alpha", ItemType::Folder)],
        ));
        // Same (author, slug), a renamed tree — the old row must be GONE.
        contact_saved(&peer_with(
            "hb_ti_rep_a",
            "ti-rep-films",
            Visibility::Public,
            vec![item("Replacement Beta", ItemType::Folder)],
        ));
        let res = search_titles("replacement", None);
        assert_eq!(res.hits.len(), 1, "one key survives the replace: {res:?}");
        assert_eq!(res.hits[0].name_norm, "replacement beta");
        assert_eq!(res.hits[0].holder_count, 1);
    }

    /// P-10 mutation: in `distinct_authors` (crates/hb-app/src/title_index.rs), replace the
    /// `seen.insert(...)` body with `seen.insert("all");` (collapse to one bucket) — this test
    /// reds (holder_count collapses to 1).
    #[test]
    fn two_authors_same_title_give_holder_count_two() {
        let _g = gate();
        contact_saved(&peer_with(
            "hb_ti_cnt_a",
            "ti-cnt-films-a",
            Visibility::Public,
            vec![item("Shared Shelf Specimen", ItemType::Folder)],
        ));
        contact_saved(&peer_with(
            "hb_ti_cnt_b",
            "ti-cnt-films-b",
            Visibility::Public,
            vec![item("shared shelf specimen", ItemType::Folder)],
        ));
        let res = search_titles("shared shelf specimen", None);
        assert_eq!(res.hits.len(), 1, "normalised key merges the authors: {res:?}");
        assert_eq!(res.hits[0].holder_count, 2);
        assert_eq!(res.hits[0].holders.len(), 2);
    }

    /// P-10 mutation (direction 1): in `walk_items` (crates/hb-app/src/title_index.rs), change
    /// `name_norm: normalize_title(&item.name)` to `name_norm: item.name.to_lowercase()` —
    /// this test reds (the "MERGE" halves stop merging: "(1984)" survives as literal text in
    /// the lowercase key while the normaliser would produce the same key from both spellings).
    /// Direction 2: post-process the key with `replace("1984", "")`-style year stripping —
    /// this test reds (the two years collapse into one key).
    #[test]
    fn title_norm_keys_merge_spellings_but_never_years() {
        let _g = gate();
        // MERGE direction: same title, different spelling — ONE key.
        contact_saved(&peer_with(
            "hb_ti_norm_a",
            "ti-norm-films-a",
            Visibility::Public,
            vec![item("Dune (1984)", ItemType::Folder)],
        ));
        contact_saved(&peer_with(
            "hb_ti_norm_b",
            "ti-norm-films-b",
            Visibility::Public,
            vec![item("dune.1984", ItemType::Folder)],
        ));
        let merged = search_titles("dune 1984", None);
        assert_eq!(merged.hits.len(), 1, "both spellings are one normalised key: {merged:?}");
        assert_eq!(merged.hits[0].holder_count, 2);

        // DISTINCT direction: the year is part of the identity of the work.
        contact_saved(&peer_with(
            "hb_ti_norm_c",
            "ti-norm-films-c",
            Visibility::Public,
            vec![item("Dune (2021)", ItemType::Folder)],
        ));
        let all = search_titles("dune", None);
        let norms: Vec<&str> = all.hits.iter().map(|h| h.name_norm.as_str()).collect();
        assert!(norms.contains(&"dune 1984") && norms.contains(&"dune 2021"), "years must not merge: {norms:?}");
        let y1984 = all.hits.iter().find(|h| h.name_norm == "dune 1984").unwrap();
        let y2021 = all.hits.iter().find(|h| h.name_norm == "dune 2021").unwrap();
        assert_eq!(y1984.holder_count, 2);
        assert_eq!(y2021.holder_count, 1);
    }

    /// P-10 mutation: in `upsert_listing` (crates/hb-app/src/title_index.rs), delete the
    /// self-guard block (`if idx.self_npub.as_deref() == Some(author_npub) { ... }`) — this
    /// test reds (self-authored rows stay in the index).
    #[test]
    fn self_npub_rows_are_dropped() {
        let _g = gate();
        set_self_npub(Some("hb_ti_self_me".to_string()));
        contact_saved(&peer_with(
            "hb_ti_self_me",
            "ti-self-films",
            Visibility::Public,
            vec![item("My Own Shelf Secretum", ItemType::Folder)],
        ));
        set_self_npub(None);
        assert_eq!(hit_count("my own shelf secretum"), 0);
    }

    /// P-10 mutation: in `rebuild_from_store` (crates/hb-app/src/title_index.rs), delete the
    /// `for peer in store.list_contacts()...` loop body (make it return early) — this test
    /// reds (the cache's titles never reach the index).
    #[test]
    fn rebuild_from_store_reads_the_disk_cache() {
        let _g = gate();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_contact(
                &CachedPeer::pubkey_hash("hb_ti_rb_a"),
                &peer_with("hb_ti_rb_a", "ti-rb-films", Visibility::Public, vec![item("Rebuild From Disk Evidence", ItemType::Folder)]),
            )
            .unwrap();
        // Prove the rebuild reads DISK, not our in-memory state: empty the index first.
        clear_for_tests();
        let titles = rebuild_from_store(&store);
        assert!(titles >= 1, "rebuild must index the cached listing");
        let res = search_titles("rebuild from disk evidence", None);
        assert_eq!(res.hits.len(), 1, "rebuild restores rows from the on-disk cache: {res:?}");
        assert_eq!(res.hits[0].holders[0].slug, "ti-rb-films");
        // ...and the fingerprint rides the row via PeerCollection.snapshot_fingerprint.
        assert!(res.hits[0].holders[0].path == "Rebuild From Disk Evidence");
    }

    /// Unfollowing a contact removes them as a holder at once — `delete_contact` is the delete
    /// twin of the `save_contact` hook.
    ///
    /// P-10 mutation: in `DataStore::delete_contact` (store.rs), delete the line
    /// `crate::title_index::remove_author(&npub);` — this test reds (the unfollowed author
    /// still counts).
    #[test]
    fn unfollowing_a_contact_drops_them_as_a_holder() {
        let _g = gate();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        for who in ["hb_ti_unf_a", "hb_ti_unf_b"] {
            store
                .save_contact(
                    &CachedPeer::pubkey_hash(who),
                    &peer_with(who, "ti-unf-films", Visibility::Public, vec![item("Unfollow Evidence Title", ItemType::Folder)]),
                )
                .unwrap();
        }
        assert_eq!(search_titles("unfollow evidence title", None).hits[0].holder_count, 2);
        store.delete_contact(&CachedPeer::pubkey_hash("hb_ti_unf_a")).unwrap();
        let res = search_titles("unfollow evidence title", None);
        assert_eq!(res.hits[0].holder_count, 1, "the unfollowed author no longer counts: {res:?}");
        assert_eq!(res.hits[0].holders[0].npub, "hb_ti_unf_b");
    }

    /// P-10 mutation: in `search_titles` (crates/hb-app/src/title_index.rs), change
    /// `let cap = limit.unwrap_or(MAX_TITLES).clamp(1, MAX_TITLES);` to
    /// `let cap = usize::MAX;` — this test reds (55 titles all returned, truncated false).
    #[test]
    fn caps_and_truncation_are_respected() {
        let _g = gate();
        for i in 0..55 {
            contact_saved(&peer_with(
                "hb_ti_cap_a",
                &format!("ti-cap-films-{i}"),
                Visibility::Public,
                vec![item(&format!("Cap Title Specimen {i:03}"), ItemType::Folder)],
            ));
        }
        // 25 distinct holders of one extra title exercise the holder cap.
        for h in 0..25 {
            contact_saved(&peer_with(
                &format!("hb_ti_cap_h{h:02}"),
                "ti-cap-holders",
                Visibility::Public,
                vec![item("Cap Holder Specimen", ItemType::Folder)],
            ));
        }
        let res = search_titles("cap title specimen", None);
        assert_eq!(res.hits.len(), MAX_TITLES, "title cap: {}", res.hits.len());
        assert!(res.truncated, "55 matches > 50 cap must report truncated");
        let holders = search_titles("cap holder specimen", None);
        assert_eq!(holders.hits.len(), 1);
        assert_eq!(holders.hits[0].holder_count, 25, "count is the true total");
        assert_eq!(holders.hits[0].holders.len(), MAX_HOLDERS, "holders capped");
    }

    /// P-10 mutation: in `contact_saved` (crates/hb-app/src/title_index.rs), change
    /// `note_author(&peer.npub, display, total_bytes);` to `note_author(&peer.npub, display, 0);`
    /// — this test reds (the placeholder sort no longer orders by total_bytes).
    #[test]
    fn holder_order_is_placeholder_total_bytes_then_npub() {
        let _g = gate();
        for (i, npub) in ["hb_ti_ord_a", "hb_ti_ord_b"].iter().enumerate() {
            let mut p = peer_with(npub, "ti-ord-films", Visibility::Public, vec![item("Order Specimen", ItemType::Folder)]);
            p.profile.as_mut().unwrap().total_bytes = if i == 0 { 100 } else { 900 };
            contact_saved(&p);
        }
        let res = search_titles("order specimen", None);
        assert_eq!(res.hits[0].holders[0].npub, "hb_ti_ord_b", "bigger total_bytes first (P3b placeholder)");
        assert_eq!(res.hits[0].holders[0].display_name.as_deref(), Some("disp-hb_ti_ord_b"));
    }

    /// The one-site guard: `upsert_listing(` (call form) appears EXACTLY ONCE in production
    /// code across the whole crate — inside `contact_saved` in this file. Scans every .rs under
    /// src/, strips everything from the first `#[cfg(test)]` (tests are allowed to call it),
    /// and counts the call form, excluding the definition (`fn upsert_listing(`).
    // P-10 mutation: in `contact_saved` (crates/hb-app/src/title_index.rs), delete the
    // `upsert_listing(` call (replace the Visibility::Public arm's call with `remove_collection(...)`)
    // — this test reds (the production call-form count drops to 0).
    #[test]
    fn upsert_listing_call_form_is_onesite() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut total = 0usize;
        let mut sites: Vec<String> = vec![];
        let mut stack = vec![src_dir.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src tree readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let prod = text.split("#[cfg(test)]").next().unwrap_or("");
                    let calls = prod.matches("upsert_listing(").count()
                        - prod.matches("fn upsert_listing(").count();
                    if calls > 0 {
                        sites.push(format!("{}: {calls}", path.display()));
                        total += calls;
                    }
                }
            }
        }
        assert_eq!(total, 1, "upsert_listing( must have exactly ONE production call site: {sites:?}");
        assert!(
            sites.iter().any(|s| s.ends_with("title_index.rs: 1")),
            "the one site is contact_saved in title_index.rs: {sites:?}"
        );
    }

    /// Test isolation: empties the process-global index between tests that need a clean slate.
    #[cfg(test)]
    pub(crate) fn clear_for_tests() {
        let mut idx = INDEX.write().expect("title index lock poisoned");
        *idx = Index::default();
    }
}
