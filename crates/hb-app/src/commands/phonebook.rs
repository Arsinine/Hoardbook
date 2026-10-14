//! QURATOR-351 slice 4 — `discover_people`: the phonebook. The book = my contacts ∪ strangers
//! seen in public teasers, every entry carrying size, collection names, likeness and the
//! Looking-for line (DRAFT "Data gaps (build)" items 1, 2 and 4).
//!
//! NO-DISCLOSURE POSTURE (CLAUDE.md §6, "Holder discovery is ASK-ONLY"): this command adds no
//! new relay event, publishes no new public signal, and caches nothing from strangers. The
//! stranger half is a READ-ONLY teaser scan through the same `search_teasers_capped` path
//! `search_peers` already uses — the same public documents, held in memory only for the
//! lifetime of the response. What a stranger publishes in their teaser (name, tags, collection
//! names, Looking-for line, total size) is public by the owner's 2026-09-28 ruling (QURATOR-342,
//! "The collection name is the teaser"); searching over it discloses nothing new (see the
//! rewritten ruling comment on `hb_net::discover::substring_in_teaser`). Title matching reaches
//! only the in-memory title index — cached listings, i.e. contacts — a limit the DRAFT already
//! states; it is surfaced per-entry as `MatchKind::Title`.
//!
//! No network crypto, no new wire shape — §5 does not fire.

use std::collections::HashSet;

use serde::Serialize;
use tauri::State;

use hb_core::event::TeaserCollection;
use hb_core::fingerprint::Fingerprint;
use hb_net::browse::search_teasers_capped;
use hb_net::discover::SearchHit;

use crate::commands::browse::SEARCH_CAP;
use crate::error::{CmdResult, cmd_err};
use crate::identity_state::SharedIdentity;
use crate::net::{self, RELAY_TIMEOUT, SharedRelay};
use crate::similarity::{SimilarityInputs, norm_tags};
use crate::store::{CachedPeer, DataStore};

/// One phonebook row: a contact or a stranger, with everything the public teaser (or the cached
/// contact) carries.
#[derive(Debug, Clone, Serialize)]
pub struct PhonebookEntry {
    pub npub: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    /// The §7 word+color fingerprint — same type/shape `PeerSearchHit.fingerprint` uses.
    pub fingerprint: Option<Fingerprint>,
    pub is_contact: bool,
    pub total_bytes: u64,
    /// Names + sizes from the teaser (`hb_core::event::TeaserCollection`).
    pub collections: Vec<TeaserCollection>,
    /// v5 (QURATOR-344) — the contact hint rides the public teaser (strangers) and the cached
    /// Profile (contacts). §6 additive-optional rule: absence means "no Contact section" and
    /// gates nothing, so NO discriminant bump — both wire shapes pinned in the tests below.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub looking_for: Option<String>,
    /// Merged Interests + collection tags, as the teaser carries them.
    pub tags: Vec<String>,
    /// 0–100 likeness, `None` when unscorable (cold-start me, or nothing to compare) — the UI
    /// shows NO % for `None`, never 0%.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likeness: Option<u8>,
    /// Which fields a query matched; empty when there is no query.
    pub matched: Vec<MatchKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Name,
    Collection,
    Tag,
    LookingFor,
    Title,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhonebookResult {
    pub entries: Vec<PhonebookEntry>,
    pub capped: bool,
    pub cold_start: bool,
}

/// The phonebook: every cached contact plus every stranger surfaced by a relay teaser scan,
/// deduped by npub (a contact wins over its own teaser hit), ordered size-desc.
#[tauri::command]
pub async fn discover_people(
    query: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<PhonebookResult> {
    let me = crate::commands::browse::identity_clone(&identity)
        .await
        .map_err(cmd_err)?;
    let me_npub = me.npub();
    // Single string → whitespace-split terms, lowercased (the `normalize_search_filters`
    // posture). 1 term = wide teaser match; ≥2 = strict AND-on-tags (unchanged contract).
    let terms: Vec<String> = query
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    // The scan ALWAYS runs: zero terms = no tag constraint (the `search_peers` filter posture),
    // and the unqueried book is exactly "contacts ∪ every teaser seen", size-sorted (r5 state a).
    // Setup copied from `search_peers`: same client, same cap, same timeout. The one deliberate
    // difference: a relay failure (none configured, all down) DEGRADES to the contacts-only
    // book instead of failing the command — the contacts half never needed a relay, and an
    // offline phonebook that still lists your own contacts beats an error. The UI learns
    // nothing was scanned from `capped == false` plus an empty stranger set; no error is
    // surfaced for the relay half (orchestrator ruling 2026-10-13, inline fix).
    let (strangers, capped) = match net::client(&me, &store, &relay).await {
        Ok(client) => {
            match search_teasers_capped(&client, &terms, &[], SEARCH_CAP, RELAY_TIMEOUT).await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("discover_people: teaser scan failed, contacts-only book: {e}");
                    (Vec::new(), false)
                }
            }
        }
        Err(e) => {
            tracing::warn!("discover_people: no relay client, contacts-only book: {e}");
            (Vec::new(), false)
        }
    };
    let mine = crate::commands::people::my_similarity_inputs(&store);
    // Title matching runs the existing title index exactly the way `search_titles` does. It
    // reaches only cached listings — contacts — which the DRAFT already states; holders of a
    // matching title get `MatchKind::Title`.
    let title_npubs: HashSet<String> = match query.as_deref() {
        Some(q) if !q.trim().is_empty() => crate::title_index::search_titles(q, None, &mine)
            .hits
            .into_iter()
            .flat_map(|h| h.holders.into_iter().map(|holder| holder.npub))
            .collect(),
        _ => HashSet::new(),
    };
    let contacts = store.list_contacts().map_err(cmd_err)?;
    Ok(assemble(&terms, &me_npub, &mine, &title_npubs, &contacts, &strangers, capped))
}

/// The pure assembly: plain slices in, one result out. No relay, no locks, no store — the
/// command wraps this; the tests drive it directly.
pub(crate) fn assemble(
    terms: &[String],
    me_npub: &str,
    mine: &SimilarityInputs,
    title_npubs: &HashSet<String>,
    contacts: &[CachedPeer],
    strangers: &[SearchHit],
    capped: bool,
) -> PhonebookResult {
    let mut entries: Vec<PhonebookEntry> = Vec::with_capacity(contacts.len() + strangers.len());
    let mut seen: HashSet<&str> = HashSet::new();
    for peer in contacts {
        if peer.npub == me_npub {
            continue; // Exclude me.
        }
        seen.insert(peer.npub.as_str());
        entries.push(contact_entry(peer, mine, terms, title_npubs));
    }
    for hit in strangers {
        // A contact wins over its own teaser hit: the cached copy is fresher-richer (bio,
        // browsed collections), so the stranger row is dropped, not the contact's.
        if hit.npub == me_npub || seen.contains(hit.npub.as_str()) {
            continue;
        }
        entries.push(stranger_entry(hit, mine, terms, title_npubs));
    }
    // Deterministic — never HashMap order: size DESC, display name ASC case-insensitively,
    // npub ASC. The UI re-sorts client-side for "likeness"; the backend ships size order.
    entries.sort_by(|a, b| {
        b.total_bytes
            .cmp(&a.total_bytes)
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
            .then_with(|| a.npub.cmp(&b.npub))
    });
    PhonebookResult { entries, capped, cold_start: mine.is_empty() }
}

fn contact_entry(
    peer: &CachedPeer,
    mine: &SimilarityInputs,
    terms: &[String],
    title_npubs: &HashSet<String>,
) -> PhonebookEntry {
    let profile = peer.profile.as_ref();
    let display_name = profile
        .map(|p| p.display_name.clone())
        .or_else(|| peer.petname.clone())
        .unwrap_or_else(|| peer.npub.clone());
    // Names + sizes: the teaser-carried list when the cached profile has it; otherwise
    // reconstructed from the browsed public collections (a pre-QURATOR-342 cached contact).
    let collections: Vec<TeaserCollection> = match profile {
        Some(p) if !p.teaser_collections.is_empty() => p.teaser_collections.clone(),
        _ => peer
            .collections
            .iter()
            .map(|pc| TeaserCollection {
                name: pc.collection.path_alias.clone(),
                bytes: pc
                    .collection
                    .est_size
                    .as_deref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0),
            })
            .collect(),
    };
    // Merged Interests + collection tags; first spelling wins, deduped case-insensitively.
    let mut tags: Vec<String> = profile.map(|p| p.tags.clone()).unwrap_or_default();
    tags.extend(peer.collections.iter().flat_map(|pc| pc.collection.tags.iter().cloned()));
    dedup_tags(&mut tags);
    let inputs = crate::commands::people::peer_similarity_inputs(peer);
    let collection_names: Vec<String> = collections.iter().map(|c| c.name.clone()).collect();
    // Computed BEFORE the struct literal: the literal moves `collections` and `tags`.
    let matched = matched_for(
        terms,
        &display_name,
        &tags,
        &collection_names,
        profile.and_then(|p| p.looking_for.as_deref()),
        &peer.npub,
        title_npubs,
    );
    PhonebookEntry {
        npub: peer.npub.clone(),
        display_name,
        picture: profile.and_then(|p| p.picture.clone()),
        // Same derivation `hit_to_card` uses; the stored one wins when present.
        fingerprint: peer.fingerprint.clone().or_else(|| derive_fingerprint(&peer.npub)),
        is_contact: true,
        total_bytes: profile.map(|p| p.total_bytes).unwrap_or(0),
        collections,
        contact_hint: profile.and_then(|p| p.contact_hint.clone()),
        looking_for: profile.and_then(|p| p.looking_for.clone()),
        tags,
        likeness: likeness_of(mine, &inputs),
        matched,
    }
}

fn stranger_entry(
    hit: &SearchHit,
    mine: &SimilarityInputs,
    terms: &[String],
    title_npubs: &HashSet<String>,
) -> PhonebookEntry {
    let t = &hit.teaser;
    // A stranger's likeness runs on the teaser alone: no titles (their listings are never
    // cached — the DRAFT's stated limit) and the merged teaser tag list standing in for both
    // tag sets (the teaser does not split interests from collection tags).
    let inputs = SimilarityInputs {
        titles: HashSet::new(),
        interest_tags: norm_tags(&t.tags),
        collection_tags: HashSet::new(),
    };
    PhonebookEntry {
        npub: hit.npub.clone(),
        display_name: t.display_name.clone(),
        picture: t.picture.clone(),
        fingerprint: derive_fingerprint(&hit.npub),
        is_contact: false,
        total_bytes: t.total_bytes,
        collections: t.collections.clone(),
        contact_hint: t.contact_hint.clone(),
        looking_for: t.looking_for.clone(),
        tags: t.tags.clone(),
        likeness: likeness_of(mine, &inputs),
        matched: matched_for(
            terms,
            &t.display_name,
            &t.tags,
            &t.collections.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            t.looking_for.as_deref(),
            &hit.npub,
            title_npubs,
        ),
    }
}

/// Which query terms hit which fields for one entry — case-insensitive substring (terms arrive
/// lowercased from the command). Empty when there is no query. `Title` comes from the
/// precomputed title-index holder set: cached listings only, i.e. contacts.
fn matched_for(
    terms: &[String],
    display_name: &str,
    tags: &[String],
    collection_names: &[String],
    looking_for: Option<&str>,
    npub: &str,
    title_npubs: &HashSet<String>,
) -> Vec<MatchKind> {
    if terms.is_empty() {
        return Vec::new();
    }
    let hit_in = |hay: &[String]| {
        terms.iter().any(|t| hay.iter().any(|h| h.to_lowercase().contains(t)))
    };
    let mut matched = Vec::new();
    if hit_in(&[display_name.to_string()]) {
        matched.push(MatchKind::Name);
    }
    if hit_in(collection_names) {
        matched.push(MatchKind::Collection);
    }
    if hit_in(tags) {
        matched.push(MatchKind::Tag);
    }
    if looking_for.is_some_and(|l| hit_in(&[l.to_string()])) {
        matched.push(MatchKind::LookingFor);
    }
    if title_npubs.contains(npub) {
        matched.push(MatchKind::Title);
    }
    matched
}

/// 0–100 likeness. `None` when unscorable: a cold-start me, a peer with nothing at all to
/// compare, or a zero score — the UI shows NO percentage for `None`, never 0%.
fn likeness_of(mine: &SimilarityInputs, peer: &SimilarityInputs) -> Option<u8> {
    if mine.is_empty() || peer.is_empty() {
        return None;
    }
    let score = crate::similarity::similarity(mine, peer).score;
    (score > 0.0).then(|| (score * 100.0).round() as u8)
}

/// The §7 word+color fingerprint from the npub alone — the same one-line derivation
/// `hit_to_card` performs (duplicated rather than exported so browse.rs keeps its private
/// helpers private; two identical 1-line bodies, no drift surface beyond the type).
fn derive_fingerprint(npub: &str) -> Option<Fingerprint> {
    hb_core::identity::parse_npub(npub)
        .ok()
        .map(|pk| hb_core::fingerprint::fingerprint(&pk))
}

fn dedup_tags(tags: &mut Vec<String>) {
    let mut seen: HashSet<String> = HashSet::new();
    tags.retain(|t| seen.insert(t.to_lowercase()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ContactSource, ListingsStatus};
    use hb_core::event::Teaser;
    use hb_core::types::Profile;

    fn contact(npub: &str, name: &str, total: u64, tags: &[&str], looking_for: Option<&str>) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source: ContactSource::Manual,
            browse_key_hex: None,
            petname: None,
            profile: Some(Profile {
                display_name: name.to_string(),
                bio: None,
                tags: tags.iter().map(|t| t.to_string()).collect(),
                content_types: vec![],
                picture: None,
                hide_in_rosters: false,
                total_bytes: total,
                teaser_collections: vec![],
                since: None,
                est_size: None,
                languages: vec![],
                contact_hint: None,
                looking_for: looking_for.map(|s| s.to_string()),
                email: None,
                location: None,
                social_links: vec![],
                willing_to: vec![],
                updated: chrono::Utc::now(),
            }),
            collections: vec![],
            listings_state: ListingsStatus::Fetched,
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        }
    }

    fn stranger(npub: &str, name: &str, total: u64, tags: &[&str], looking_for: Option<&str>) -> SearchHit {
        SearchHit {
            npub: npub.to_string(),
            teaser: Teaser {
                display_name: name.to_string(),
                bio: String::new(),
                tags: tags.iter().map(|t| t.to_string()).collect(),
                content_types: vec![],
                picture: None,
                hide_in_rosters: false,
                total_bytes: total,
                contact_hint: None,
                looking_for: looking_for.map(|s| s.to_string()),
                collections: vec![],
            },
            created_at: nostr::prelude::Timestamp::now(),
        }
    }

    /// P-10 mutation: in `assemble`'s stranger loop, delete the guard that skips a stranger
    /// whose npub is already in `seen` (keep only the self-check) — the same person then
    /// appears twice and the first assert reds while the loop still compiles.
    #[test]
    fn discover_people_dedups_contact_over_stranger_teaser() {
        let mine = SimilarityInputs::default();
        let contacts = vec![contact("hb_pb_dedup_a", "Dedup Peer", 500, &["sci-fi"], None)];
        let strangers = vec![stranger("hb_pb_dedup_a", "Dedup Peer", 500, &["sci-fi"], None)];
        let res = assemble(&[], "me", &mine, &HashSet::new(), &contacts, &strangers, false);
        assert_eq!(res.entries.len(), 1, "one npub, one row: {res:?}");
        assert!(res.entries[0].is_contact, "the contact wins over its own teaser hit");
        assert!(res.cold_start, "an empty me is cold start");
    }

    /// §6 wire shape for the optional field: the `contact_hint` key is PRESENT with the value
    /// when the teaser/profile carries one and ABSENT (never null) when it does not — both
    /// shapes built through the real builders (`assemble` → `contact_entry`/`stranger_entry`).
    /// P-10 mutation: in `stranger_entry` replace `contact_hint: t.contact_hint.clone(),`
    /// with `contact_hint: None,` → the stranger's present-key assert reds; in `contact_entry`
    /// replace `contact_hint: profile.and_then(|p| p.contact_hint.clone()),` with
    /// `contact_hint: None,` → the contact's present-key assert reds. Both seen red 2026-10-14
    /// (orchestrator; mutate by LINE — the anchor text also appears in this comment).
    #[test]
    fn discover_people_contact_hint_wire_shape() {
        let mine = SimilarityInputs::default();
        let mut hinted_contact = contact("hb_pb_hint_c", "Hinted Contact", 500, &["sci-fi"], None);
        hinted_contact.profile.as_mut().unwrap().contact_hint = Some("DM my alt acct".to_string());
        let mut hinted_stranger =
            stranger("hb_pb_hint_s", "Hinted Stranger", 500, &["sci-fi"], None);
        hinted_stranger.teaser.contact_hint = Some("signal first, then email".to_string());
        let plain = stranger("hb_pb_hint_p", "Plain Stranger", 500, &["sci-fi"], None);
        let res = assemble(
            &[],
            "me",
            &mine,
            &HashSet::new(),
            &[hinted_contact],
            &[hinted_stranger, plain],
            false,
        );
        let by_npub = |npub: &str| {
            res.entries
                .iter()
                .find(|e| e.npub == npub)
                .unwrap_or_else(|| panic!("no entry for {npub}"))
        };
        // Present (stranger, teaser-carried): key present with the value.
        let hinted = by_npub("hb_pb_hint_s");
        assert_eq!(
            hinted.contact_hint.as_deref(),
            Some("signal first, then email")
        );
        let j = serde_json::to_string(hinted).unwrap();
        assert!(
            j.contains(r#""contact_hint":"signal first, then email""#),
            "hinted stranger wire shape: {j}"
        );
        // Present (contact, cached-Profile-carried): same key, same rule.
        let hinted_c = by_npub("hb_pb_hint_c");
        assert_eq!(hinted_c.contact_hint.as_deref(), Some("DM my alt acct"));
        let jc = serde_json::to_string(hinted_c).unwrap();
        assert!(
            jc.contains(r#""contact_hint":"DM my alt acct""#),
            "hinted contact wire shape: {jc}"
        );
        // Absent: the key is OMITTED, never null (skip_serializing_if).
        let plain_e = by_npub("hb_pb_hint_p");
        assert_eq!(plain_e.contact_hint, None);
        let jp = serde_json::to_string(plain_e).unwrap();
        assert!(
            !jp.contains("contact_hint"),
            "plain wire must omit the key entirely: {jp}"
        );
    }

    /// P-10 mutation: in `assemble`'s sort, reverse the FIRST key — the total-bytes
    /// comparison currently descending (larger first) — to ascending. The size assert reds;
    /// the name/npub tiebreak asserts keep holding, attributing the failure to that key.
    #[test]
    fn discover_people_orders_size_desc_then_name_asc_then_npub() {
        let mine = SimilarityInputs::default();
        // Insertion order deliberately differs from the expected order, and two rows tie on
        // BOTH size and name so the npub tiebreak is exercised (a 2-entry fixture is vacuous).
        let contacts = vec![
            contact("hb_pb_ord_b", "Zeta", 200, &[], None),
            contact("hb_pb_ord_a", "Beta", 500, &[], None),
            contact("hb_pb_ord_c", "alpha", 500, &[], None),
        ];
        let strangers = vec![stranger("hb_pb_ord_d", "alpha", 500, &[], None)];
        let res = assemble(&[], "me", &mine, &HashSet::new(), &contacts, &strangers, false);
        let npubs: Vec<&str> = res.entries.iter().map(|e| e.npub.as_str()).collect();
        assert_eq!(
            npubs,
            vec!["hb_pb_ord_c", "hb_pb_ord_d", "hb_pb_ord_a", "hb_pb_ord_b"],
            "500-byte rows first (name asc: alpha==alpha → npub asc: c before d), then Beta, then 200-byte Zeta: {npubs:?}"
        );
        assert_eq!(res.entries[3].total_bytes, 200);
        assert!(!res.entries[1].is_contact, "the stranger keeps its own row");
    }

    /// P-10 mutation (orchestrator-verified 2026-10-13): the `None` outcome has TWO guards in
    /// `likeness_of` — the `mine.is_empty() || peer.is_empty()` early return AND the
    /// `(score > 0.0)` filter — and either alone keeps this test GREEN (a cold-start me has
    /// empty sets, so Jaccard is 0 and the filter catches it). Mutating BOTH at once (drop
    /// `mine.is_empty() ||` from the early return and replace the `.then(..)` filter with an
    /// unconditional `Some(..)`) reds the first assert. One guard alone is not a proof here.
    #[test]
    fn discover_people_likeness_none_for_cold_start_and_tagless_stranger_some_for_overlap() {
        let contacts: Vec<CachedPeer> = vec![];
        let strangers = vec![
            stranger("hb_pb_like_a", "Has Tags", 100, &["sci-fi"], None),
            stranger("hb_pb_like_b", "No Tags", 100, &[], None),
        ];
        let cold_me = SimilarityInputs::default();
        let res = assemble(&[], "me", &cold_me, &HashSet::new(), &contacts, &strangers, false);
        assert!(res.cold_start);
        assert!(
            res.entries.iter().all(|e| e.likeness.is_none()),
            "a cold-start me scores nobody — no percentages at all"
        );

        let me = SimilarityInputs {
            titles: HashSet::new(),
            interest_tags: ["sci-fi".to_string()].into_iter().collect(),
            collection_tags: HashSet::new(),
        };
        let res = assemble(&[], "me", &me, &HashSet::new(), &contacts, &strangers, false);
        assert!(!res.cold_start);
        let a = res.entries.iter().find(|e| e.npub == "hb_pb_like_a").unwrap();
        let b = res.entries.iter().find(|e| e.npub == "hb_pb_like_b").unwrap();
        assert!(matches!(a.likeness, Some(n) if n > 0), "overlapping stranger scores: {a:?}");
        assert!(b.likeness.is_none(), "a stranger with no tags has nothing to compare — None, never 0%");
    }

    /// P-10 mutation: in `matched_for`, force the Looking-for test to a constant false
    /// (replace the substring check over the looking_for line with `false`) — the LookingFor
    /// assert reds; the empty-query assert stays green, attributing the failure to that arm.
    #[test]
    fn discover_people_matched_carries_looking_for_and_is_empty_without_query() {
        let mine = SimilarityInputs::default();
        let strangers = vec![stranger(
            "hb_pb_mf_a",
            "Vinyl Curator",
            100,
            &["records"],
            Some("after cheap sci-fi paperbacks"),
        )];
        let res = assemble(
            &["paperbacks".to_string()],
            "me",
            &mine,
            &HashSet::new(),
            &[],
            &strangers,
            false,
        );
        assert_eq!(
            res.entries[0].matched,
            vec![MatchKind::LookingFor],
            "'paperbacks' appears ONLY in the Looking-for line: {res:?}"
        );
        let res = assemble(&[], "me", &mine, &HashSet::new(), &[], &strangers, false);
        assert!(res.entries[0].matched.is_empty(), "no query → no matched kinds");
    }
}
