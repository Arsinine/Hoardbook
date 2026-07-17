//! Contacts: paste a share code, follow, refresh — rewired onto the M3 `hb-net` browse API.
//!
//! A "contact" is now keyed on the peer's **npub** (+ the account browse-key captured from a full
//! `hbk` code, which unlocks their listings + presence address). Resolving a peer is a **relay
//! read**: fetch their public teaser (`browse_share_code`) and their presence binding (for online
//! status). Full collection browsing is the dedicated M3 browse route (now the default) — the
//! inline `collections` on a contact is no longer populated here.

use chrono::Utc;
use nostr::prelude::ToBech32;
use tauri::State;

use hb_core::event::Teaser;
use hb_core::fingerprint::Fingerprint;
use hb_core::types::Collection;
use hb_core::{ShareCode, Identity};
use hb_net::{
    browse_peer_listings, browse_share_code, fetch_full_listing_if_current,
    listing_snapshot_fingerprint, search_teasers, RelayClient, RenderedListing, SearchHit,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{CmdResult, cmd_err},
    net::{self, SharedRelay},
    store::{CachedPeer, DataStore},
    identity_state::SharedIdentity,
};

/// Discovery result cap — mirrors the teaser/discovery cap; a flood of teasers can't make the result
/// set unbounded.
const SEARCH_CAP: usize = 100;

/// A §6 Discovery teaser card (M12 W3). Carries **only** the opt-in public teaser — name/bio/tags/
/// content-types + the §7 fingerprint (the impersonation distinguisher for a stranger). It carries
/// **no listing and no browse-key** (DISC3): a search hit surfaces the advertisement, never the
/// hoard. The stash stays 🔒 browse-key-locked.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PeerSearchHit {
    pub npub: String,
    pub display_name: String,
    pub bio: Option<String>,
    pub tags: Vec<String>,
    pub content_types: Vec<String>,
    /// Optional data-URI avatar from the teaser (validated/sanitized at parse — never a remote URL).
    pub picture: Option<String>,
    /// The §7 word+color fingerprint, derived from the npub alone (no listing access).
    pub fingerprint: Option<Fingerprint>,
}

/// Normalize + validate search filters (M12 W3, Decision I). Trims, lowercases tags, drops empties,
/// and enforces **≥1 filter** at the trust boundary (defense-in-depth — also enforced inside
/// `teaser_search_filter`; Gemini). Returns `(tags, content_types)` or an error string.
fn normalize_search_filters(
    tags: Vec<String>,
    content_types: Vec<String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let tags: Vec<String> =
        tags.into_iter().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
    let content_types: Vec<String> =
        content_types.into_iter().map(|c| c.trim().to_lowercase()).filter(|c| !c.is_empty()).collect();
    if tags.is_empty() && content_types.is_empty() {
        return Err("Enter at least one tag or content type to search.".into());
    }
    Ok((tags, content_types))
}

/// Drop discovery hits that are the searcher's own npub (devtest #4) or an already-added contact
/// (devtest #6) — Discover should only ever surface strangers, never yourself or someone already on
/// the roster. Pure and unit-testable without a relay.
fn filter_hits(hits: Vec<SearchHit>, me_npub: &str, contact_npubs: &[String]) -> Vec<SearchHit> {
    hits.into_iter().filter(|h| h.npub != me_npub && !contact_npubs.contains(&h.npub)).collect()
}

/// Map a verified discovery `SearchHit` → a teaser card, deriving the §7 fingerprint from the npub.
/// **No listing / browse-key is carried** (DISC3) — the card type structurally cannot hold one.
fn hit_to_card(hit: SearchHit) -> PeerSearchHit {
    let fingerprint =
        hb_core::identity::parse_npub(&hit.npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk));
    PeerSearchHit {
        npub: hit.npub,
        display_name: hit.teaser.display_name,
        bio: if hit.teaser.bio.is_empty() { None } else { Some(hit.teaser.bio) },
        tags: hit.teaser.tags,
        content_types: hit.teaser.content_types,
        picture: hit.teaser.picture,
        fingerprint,
    }
}

/// Map a public teaser into the local `Profile` shape the contacts UI renders.
fn teaser_to_profile(t: Teaser) -> hb_core::types::Profile {
    hb_core::types::Profile {
        display_name: t.display_name,
        bio: if t.bio.is_empty() { None } else { Some(t.bio) },
        tags: t.tags,
        content_types: t.content_types,
        picture: t.picture,
        since: None,
        est_size: None,
        languages: vec![],
        contact_hint: None,
        email: None,
        location: None,
        social_links: vec![],
        willing_to: vec![],
        updated: Utc::now(),
    }
}

/// A peer's collection as browsed with a full share code (M13 HANDOVER gap #5) — the `Collection`
/// plus the K-of-N part counts `hb-net::browse_peer_listings` returned for it. Mirrors the
/// `CollectionEntry` pattern (REGRESSION #90): the part-availability info is a **local browse-time
/// signal**, never folded into the hb-core wire `Collection` type itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCollection {
    #[serde(flatten)]
    pub collection: Collection,
    /// Total parts the peer's index claims for this collection. `None` for a pre-M13 cached entry
    /// (never fabricate a K-of-N badge for stale cache data — see `browse-view.ts::collectionAvailability`).
    #[serde(default)]
    pub parts_total: Option<usize>,
    /// Parts actually present. `None` alongside `parts_total` for a pre-M13 cached entry.
    #[serde(default)]
    pub parts_present: Option<usize>,
    /// devtest #7 — true when the author published only a truncated paywall teaser of this collection
    /// (too large to publish whole). `total_items` is the full item count; the browser shows the kept
    /// entries followed by a "N more hidden" fade. `None` for a listing without the marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_items: Option<usize>,
}

/// Map a `RenderedListing` (meta + entries, from `hb_net::browse_peer_listings`) back into a
/// `PeerCollection` — the inverse of `collection_to_listing_json`'s `listing` → `entries` remap,
/// mirroring `private_listing_to_collection`'s shape trick. Pure — unit-tested without a relay.
/// Unparseable meta (a family that doesn't decode as a `Collection`) → `None`, never a hard error.
pub(crate) fn rendered_to_peer_collection(r: &RenderedListing) -> Option<PeerCollection> {
    let mut map = r.meta.clone();
    // devtest #7: pull the paywall-teaser markers out of the meta before it's decoded as a Collection
    // (which has no such fields) — they become PeerCollection browse-time signals, like the K-of-N counts.
    let truncated = map.remove("truncated").and_then(|v| v.as_bool());
    let total_items = map.remove("total_items").and_then(|v| v.as_u64()).map(|n| n as usize);
    map.insert("listing".into(), serde_json::Value::Array(r.entries.clone()));
    let collection: Collection = serde_json::from_value(serde_json::Value::Object(map)).ok()?;
    Some(PeerCollection {
        collection,
        parts_total: Some(r.parts_total),
        parts_present: Some(r.parts_present),
        truncated,
        total_items,
    })
}

/// Ordered, de-duplicated big-relay candidates for browse-side full-manifest resolution (M16 W3),
/// per the owner ruling **(a) then (b)**: (a) the browser's OWN configured big relay first, then
/// (b) the peer's big relay advertised in the (browse-key-encrypted) teaser meta. Blank entries are
/// dropped and a peer relay identical to our own is not retried (the shared-community case, where the
/// hoarder's advertised relay equals ours). Pure — unit-tested without a relay.
fn big_relay_fetch_order<'a>(own_big: &'a str, peer_big: &'a str) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for candidate in [own_big.trim(), peer_big.trim()] {
        if !candidate.is_empty() && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// For a browsed collection that came back as a truncated paywall teaser, try to upgrade it to the
/// FULL listing by fetching the big-relay family (M16 W3). Tries the big relays in
/// [`big_relay_fetch_order`] order — the browser's own (a), then the peer's advertised one (b) —
/// gating each on [`fetch_full_listing_if_current`] (fingerprint matches the teaser AND the tree is
/// complete). Returns the full `RenderedListing` on the first success, or `None` when the teaser is
/// not truncated, carries no fingerprint to gate on, or no big relay yields a current full tree — in
/// which case the caller keeps the teaser. Never a hard error: a big-relay hiccup just keeps the
/// teaser (the pre-M16 behaviour).
async fn resolve_full_if_truncated(
    client: &RelayClient,
    peer: &nostr::PublicKey,
    slug: &str,
    browse_key: &[u8; 32],
    teaser: &RenderedListing,
    own_big: &str,
) -> Option<RenderedListing> {
    // Only a truncated teaser has a hidden remainder worth fetching.
    if teaser.meta.get("truncated").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    // Without the teaser's snapshot fingerprint there is nothing to gate staleness on — keep the teaser.
    let fingerprint = listing_snapshot_fingerprint(teaser)?;
    let peer_big = teaser.meta.get("big_relay_url").and_then(|v| v.as_str()).unwrap_or("");
    for candidate in big_relay_fetch_order(own_big, peer_big) {
        let relays = [candidate.to_string()];
        // Targeted connect (mirrors `publish_to`); a relay we can't reach just falls through to the next.
        if client.ensure_relays(&relays, net::RELAY_TIMEOUT).await.is_err() {
            continue;
        }
        if let Ok(Some(full)) = fetch_full_listing_if_current(
            client, peer, slug, browse_key, &relays, fingerprint, net::RELAY_TIMEOUT,
        )
        .await
        {
            return Some(full);
        }
    }
    None
}

/// Resolve a share code to a `CachedPeer`: fetch the public teaser + the presence binding (online
/// status), as a pure relay read. Falls back to the local cache (stale, offline) when the relays
/// yield nothing.
async fn resolve_peer(
    share_code: &ShareCode,
    me: &Identity,
    store: &DataStore,
    relay: &SharedRelay,
) -> Result<CachedPeer, String> {
    let peer = share_code.pubkey();
    let npub = peer.to_bech32().map_err(cmd_err)?;
    let seed = net::relay_urls(store);

    let client = net::client(me, store, relay).await.map_err(cmd_err)?;
    let browse = browse_share_code(&client, share_code, "", &seed, &seed, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err);
    // Online = a fresh, valid presence binding exists for this npub.
    let online = match crate::presence::fetch_peer_presence(&client, &peer, net::RELAY_TIMEOUT).await {
        Ok(Some(ev)) => hb_core::verify_binding(
            &ev,
            &peer,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
        .is_ok(),
        _ => false,
    };

    let profile = browse.ok().and_then(|b| b.teaser).map(teaser_to_profile);
    // The §7 fingerprint is a pure function of the npub — always derivable, even for a peer who has
    // published no teaser (it is the impersonation distinguisher you check before trusting a stranger).
    let fingerprint = Some(hb_core::fingerprint::fingerprint(&peer));

    // A full share code (carrying a browse-key) can browse every listing family the peer has
    // published (M13 HANDOVER gap #5). A locked/failed family is already skipped inside
    // `browse_peer_listings` (BR1) — best-effort here too, mirroring the teaser fetch above:
    // unreadable listings must never fail the whole resolve.
    let collections = match share_code.browse_key() {
        Some(bk) => {
            // The browser's OWN big relay (option a) — tried before the peer's advertised one (b).
            let own_big = store.load_settings().map_err(cmd_err)?.unwrap_or_default().big_relay_url;
            let families = browse_peer_listings(&client, &peer, &bk, net::RELAY_TIMEOUT)
                .await
                .unwrap_or_default();
            let mut out = Vec::with_capacity(families.len());
            for (root, teaser) in &families {
                // M16 W3: a truncated teaser may have its full listing on a big relay — try to upgrade
                // it (a → b); on success browse the full tree, otherwise the teaser as-is (unchanged).
                let full = resolve_full_if_truncated(&client, &peer, root, &bk, teaser, &own_big).await;
                if let Some(pc) = rendered_to_peer_collection(full.as_ref().unwrap_or(teaser)) {
                    out.push(pc);
                }
            }
            out
        }
        None => vec![],
    };

    // Fall back to the cached contact if the relay yielded no teaser.
    if profile.is_none() {
        if let Some(mut stale) = store.load_contact(&CachedPeer::pubkey_hash(&npub)).map_err(cmd_err)? {
            stale.online = online;
            stale.fingerprint = fingerprint;
            // A full share code just handed us a browse-key — merge it even though the teaser fetch
            // flaked (devtest #4: add-by-npub then paste the full code must not lose the key, or the
            // contact stays permanently unbrowseable).
            merge_browse_key(&mut stale, share_code);
            // If the keyed listings fetch above still succeeded, prefer the fresh listings over the
            // stale cache (devtest #3: a hiccup at add-time must not cache "empty" forever).
            if !collections.is_empty() {
                stale.collections = collections;
            }
            return Ok(stale);
        }
    }

    Ok(CachedPeer {
        npub,
        source: crate::store::ContactSource::Manual,
        browse_key_hex: share_code.browse_key().map(hex::encode),
        petname: profile.as_ref().map(|p| p.display_name.clone()),
        profile,
        collections,
        online,
        last_fetched: Utc::now(),
        local_tags: vec![],
        fingerprint,
    })
}

/// Snapshot the loaded identity (cloned) or error if none.
async fn identity_clone(identity: &SharedIdentity) -> Result<Identity, String> {
    identity
        .read()
        .await
        .as_ref()
        .map(|id| id.identity.clone())
        .ok_or_else(|| "No identity loaded. Generate a keypair first.".to_string())
}

/// R2: a peer with no published teaser cannot be added — unconditional reject at the trust
/// boundary (devtest #17/#18), regardless of online status. Pure and unit-testable without a
/// relay. The one deliberate exception is Q7 chat request-accept (`chat.rs
/// dm_request_accept_inner`), which builds its own local peer stub with `profile: None` — that
/// seam does not call this gate.
fn reject_profileless(peer: &CachedPeer) -> Result<(), String> {
    if peer.profile.is_none() {
        return Err(
            "This person hasn't published a profile yet, so there's nothing to add here. Ask them to publish a profile first."
                .into(),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn paste_key(
    code: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<CachedPeer> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    if me.public_key() == share_code.pubkey() {
        return Err("You cannot look up your own code".into());
    }
    let peer = resolve_peer(&share_code, &me, &store, &relay).await?;
    reject_profileless(&peer)?;
    Ok(peer)
}

/// Merge a freshly-resolved share code's browse-key onto a stale cached contact (devtest #4): when
/// the teaser fetch yields nothing we fall back to the cache, but a full share code just handed us a
/// browse-key — dropping it would leave an npub-added contact permanently unbrowseable even after the
/// user pastes the full code. A `FollowOnly`/bare code carries no key and leaves the field untouched.
/// Pure — unit-tested without a relay.
fn merge_browse_key(stale: &mut CachedPeer, share_code: &ShareCode) {
    if let Some(bk) = share_code.browse_key() {
        stale.browse_key_hex = Some(hex::encode(bk));
    }
}

/// Apply an optional follow-time petname edit onto a resolved peer (M13 W5 item 4): a `Some`
/// non-empty petname overrides whatever `resolve_peer` auto-derived from the teaser display_name;
/// `None` or an empty string leaves it untouched. Pure — unit-tested without a relay.
fn apply_follow_petname(peer: &mut CachedPeer, petname: Option<String>) {
    if let Some(p) = petname.filter(|p| !p.is_empty()) {
        peer.petname = Some(p);
    }
}

#[tauri::command]
pub async fn follow(
    code: String,
    group_name: Option<String>,
    // M13 W5 item 4: an optional user-supplied petname, set at follow-time. Trailing `Option` keeps
    // existing callers (which pass fewer invoke args) working — a missing/`null` arg is simply "no
    // petname edit", falling back to the auto-derived one `resolve_peer` already set.
    petname: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    let mut peer = resolve_peer(&share_code, &me, &store, &relay).await?;
    // Defense-in-depth (R2): closes the AddContactDialog Skip path for a profileless peer even if
    // the caller bypassed the paste_key/lookup gate.
    reject_profileless(&peer)?;
    apply_follow_petname(&mut peer, petname);
    let npub = peer.npub.clone();
    store.save_contact(&CachedPeer::pubkey_hash(&npub), &peer).map_err(cmd_err)?;

    if let Some(gname) = group_name {
        let mut groups = store.load_groups().map_err(cmd_err)?;
        if let Some(group) = groups.iter_mut().find(|g| g.name == gname) {
            if !group.pubkeys.contains(&npub) {
                group.pubkeys.push(npub);
                group.modified_at = Utc::now();
            }
            store.save_groups(&groups).map_err(cmd_err)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn get_contacts(store: State<'_, DataStore>) -> CmdResult<Vec<CachedPeer>> {
    store.list_contacts().map_err(cmd_err)
}

#[tauri::command]
pub async fn unfollow_contact(npub: String, store: State<'_, DataStore>) -> CmdResult<()> {
    store.delete_contact(&CachedPeer::pubkey_hash(&npub)).map_err(cmd_err)
}

/// Rebuild a share code from a saved contact (npub + cached browse-key) so a refresh can re-read.
fn contact_share_code(contact: &CachedPeer) -> Result<ShareCode, String> {
    let pubkey = hb_core::identity::parse_npub(&contact.npub).map_err(cmd_err)?;
    match &contact.browse_key_hex {
        Some(hexk) => {
            let bytes: [u8; 32] = hex::decode(hexk)
                .map_err(cmd_err)?
                .try_into()
                .map_err(|_| "stored browse-key is not 32 bytes".to_string())?;
            Ok(ShareCode::Full { pubkey, browse_key: bytes })
        }
        None => Ok(ShareCode::FollowOnly { pubkey }),
    }
}

#[tauri::command]
pub async fn refresh_contact(
    npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<CachedPeer> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let existing = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    let share_code = contact_share_code(&existing)?;
    let me = identity_clone(&identity).await?;
    let mut updated = resolve_peer(&share_code, &me, &store, &relay).await?;
    // Preserve local-only state across refresh.
    updated.local_tags = existing.local_tags;
    updated.petname = existing.petname.or(updated.petname);
    store.save_contact(&hash, &updated).map_err(cmd_err)?;
    Ok(updated)
}

/// Set user-defined local tags on a contact. Tags are stored locally and never shared.
#[tauri::command]
pub async fn set_contact_tags(
    npub: String,
    tags: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let mut peer = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    peer.local_tags = tags;
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// Set a contact's local, user-editable petname (M13 W5 item 4). Mirrors `set_contact_tags` — an
/// impersonation-resistant label bound to the `npub`, stored locally and never shared.
#[tauri::command]
pub async fn set_contact_petname(
    npub: String,
    petname: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let mut peer = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    peer.petname = Some(petname);
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// §6 Discovery (M12 W3): search public teasers by tag (AND) / content-type (OR) across the relays
/// and return teaser cards. **≥1 filter is required** (no unfiltered global peer list — §6). A hit
/// carries only the opt-in public teaser + the §7 fingerprint, **never** a listing or browse-key
/// (DISC3) — the stash stays 🔒 locked.
#[tauri::command]
pub async fn search_peers(
    tags: Vec<String>,
    content_types: Vec<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<PeerSearchHit>> {
    let (tags, content_types) = normalize_search_filters(tags, content_types)?;
    let me = identity_clone(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let hits = search_teasers(&client, &tags, &content_types, SEARCH_CAP, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;
    let contact_npubs: Vec<String> =
        store.list_contacts().map_err(cmd_err)?.into_iter().map(|c| c.npub).collect();
    let hits = filter_hits(hits, &me.npub(), &contact_npubs);
    Ok(hits.into_iter().map(hit_to_card).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_requires_at_least_one_filter() {
        // Decision I / DISC4 at the command's trust boundary: empty ∧ empty is refused (defense in
        // depth — also enforced inside teaser_search_filter), and whitespace-only filters count as empty.
        assert!(normalize_search_filters(vec![], vec![]).is_err());
        assert!(normalize_search_filters(vec!["  ".into()], vec!["".into()]).is_err());
        let (tags, cts) = normalize_search_filters(vec![" Anime ".into()], vec![]).unwrap();
        assert_eq!(tags, vec!["anime".to_string()], "tags are trimmed + lowercased");
        assert!(cts.is_empty());
    }

    #[test]
    fn hit_card_derives_fingerprint_and_carries_no_listing_or_key_disc3() {
        // DISC3: a discovery card is the teaser + a derived fingerprint — never a listing or
        // browse-key. The card type structurally cannot hold one; assert the serialized shape too.
        let id = Identity::generate();
        let hit = SearchHit {
            npub: id.npub(),
            teaser: Teaser {
                display_name: "archivebox".into(),
                bio: "90s anime".into(),
                tags: vec!["anime".into()],
                content_types: vec!["video".into()],
                picture: Some("data:image/webp;base64,AA==".into()),
            },
        };
        let card = hit_to_card(hit);
        assert!(card.fingerprint.is_some(), "the §7 fingerprint is derived from the npub");
        assert_eq!(card.bio.as_deref(), Some("90s anime"));
        assert_eq!(card.picture.as_deref(), Some("data:image/webp;base64,AA=="), "teaser avatar rides the hit card");
        let json = serde_json::to_string(&card).unwrap();
        assert!(!json.contains("browse_key") && !json.contains("browseKey"), "no browse-key on a hit");
        assert!(!json.contains("listing"), "no listing on a hit (DISC3)");
    }

    #[test]
    fn hit_card_blank_bio_is_none() {
        let id = Identity::generate();
        let hit = SearchHit {
            npub: id.npub(),
            teaser: Teaser { display_name: "x".into(), bio: String::new(), tags: vec![], content_types: vec![], picture: None },
        };
        assert_eq!(hit_to_card(hit).bio, None, "a blank bio renders as None, not an empty string");
    }

    fn hit_for(npub: String) -> SearchHit {
        SearchHit {
            npub,
            teaser: Teaser { display_name: "x".into(), bio: String::new(), tags: vec![], content_types: vec![], picture: None },
        }
    }

    #[test]
    fn filter_hits_drops_own_npub_devtest_4() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let hits = vec![hit_for(me.npub()), hit_for(stranger.npub())];
        let kept = filter_hits(hits, &me.npub(), &[]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].npub, stranger.npub());
    }

    #[test]
    fn filter_hits_drops_existing_contacts_devtest_6() {
        let me = Identity::generate();
        let contact = Identity::generate();
        let stranger = Identity::generate();
        let hits = vec![hit_for(contact.npub()), hit_for(stranger.npub())];
        let kept = filter_hits(hits, &me.npub(), &[contact.npub()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].npub, stranger.npub());
    }

    #[test]
    fn filter_hits_keeps_strangers() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let hits = vec![hit_for(stranger.npub())];
        let kept = filter_hits(hits, &me.npub(), &[]);
        assert_eq!(kept.len(), 1);
    }

    fn valid_meta(slug: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut meta = serde_json::Map::new();
        meta.insert("slug".into(), serde_json::json!(slug));
        meta.insert("path_alias".into(), serde_json::json!(slug));
        meta.insert("item_count".into(), serde_json::json!(0));
        meta.insert("content_types".into(), serde_json::json!(["video"]));
        meta.insert("last_updated".into(), serde_json::json!(Utc::now().to_rfc3339()));
        meta
    }

    #[test]
    fn rendered_listing_maps_to_peer_collection_with_parts() {
        // A partial family (K of N): the counts carry straight through onto the PeerCollection.
        let rendered = RenderedListing {
            meta: valid_meta("films"),
            entries: vec![serde_json::json!({"name": "a.mkv", "item_type": "File", "tags": [], "children": []})],
            parts_total: 5,
            parts_present: 3,
            missing: vec![1, 4],
        };
        let peer_col = rendered_to_peer_collection(&rendered).expect("valid meta must convert");
        assert_eq!(peer_col.collection.slug, "films");
        assert_eq!(peer_col.collection.listing.len(), 1, "the rendered entries become the listing");
        assert_eq!(peer_col.parts_total, Some(5));
        assert_eq!(peer_col.parts_present, Some(3));

        // Malformed meta (missing the Collection's required fields) → None, never a panic/hard error.
        let malformed = RenderedListing {
            meta: serde_json::Map::new(),
            entries: vec![],
            parts_total: 1,
            parts_present: 1,
            missing: vec![],
        };
        assert!(rendered_to_peer_collection(&malformed).is_none(), "unparseable meta must convert to None");
    }

    #[test]
    fn rendered_listing_carries_the_paywall_truncation_markers() {
        // devtest #7: a browsed truncated teaser's `truncated`/`total_items` markers ride in the meta
        // and surface on the PeerCollection (they are NOT Collection fields, so they must be pulled
        // out before the meta is decoded as a Collection — otherwise a stricter Collection would reject
        // the unknown keys).
        let mut meta = valid_meta("bigvault");
        meta.insert("truncated".into(), serde_json::json!(true));
        meta.insert("total_items".into(), serde_json::json!(9000));
        let rendered = RenderedListing {
            meta,
            entries: vec![serde_json::json!({"name": "a.mkv", "item_type": "File", "tags": [], "children": []})],
            parts_total: 1,
            parts_present: 1,
            missing: vec![],
        };
        let peer_col = rendered_to_peer_collection(&rendered).expect("markers must not break the decode");
        assert_eq!(peer_col.truncated, Some(true));
        assert_eq!(peer_col.total_items, Some(9000));
        assert_eq!(peer_col.collection.slug, "bigvault");
    }

    // ── M16 W3: browse-side big-relay resolution order (a → b) ────────────────────
    // The candidate ordering is pure; the actual fetch/merge round-trip (truncated teaser → full
    // tree) is proven by hb-it Suite BIG1/BIG2, same split as the publish-path tests.

    #[test]
    fn big_relay_fetch_order_tries_own_then_peer_deduped() {
        // (a) own first, then (b) the peer's advertised relay.
        assert_eq!(
            big_relay_fetch_order("ws://own:7777", "ws://peer:7777"),
            vec!["ws://own:7777", "ws://peer:7777"],
        );
        // No own setting ⇒ just the peer's advertised relay (option b alone).
        assert_eq!(big_relay_fetch_order("", "ws://peer:7777"), vec!["ws://peer:7777"]);
        // Own set, peer advertises none ⇒ just our own (option a alone).
        assert_eq!(big_relay_fetch_order("ws://own:7777", ""), vec!["ws://own:7777"]);
        // Shared-community case: the peer's advertised relay equals ours ⇒ tried once, not twice.
        assert_eq!(big_relay_fetch_order("ws://one:7777", "ws://one:7777"), vec!["ws://one:7777"]);
        // Trimmed; a blank peer entry is dropped.
        assert_eq!(big_relay_fetch_order("  ws://own:7777  ", "   "), vec!["ws://own:7777"]);
        // Both blank ⇒ nothing to try (keep the teaser).
        assert!(big_relay_fetch_order("", "").is_empty());
    }

    #[test]
    fn peer_collection_serializes_with_flattened_collection_fields() {
        // REGRESSION #90 pattern: the parts info must sit ALONGSIDE the flattened Collection fields
        // in the wire JSON, not nested — so a pre-M13 consumer expecting a plain Collection object
        // still finds every Collection field at the top level.
        let rendered = RenderedListing {
            meta: valid_meta("films"),
            entries: vec![],
            parts_total: 2,
            parts_present: 2,
            missing: vec![],
        };
        let peer_col = rendered_to_peer_collection(&rendered).unwrap();
        let json = serde_json::to_value(&peer_col).unwrap();
        assert_eq!(json.get("slug").unwrap(), "films", "Collection fields are flattened to the top level");
        assert_eq!(json.get("parts_total").unwrap(), 2);
        assert_eq!(json.get("parts_present").unwrap(), 2);
    }

    // ── M13 W5 item 4: petname ─────────────────────────────────────────────────────

    fn test_store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn stub_peer(npub: &str, petname: Option<&str>) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source: crate::store::ContactSource::Manual,
            browse_key_hex: None,
            petname: petname.map(|s| s.to_string()),
            profile: None,
            collections: vec![],
            online: false,
            last_fetched: Utc::now(),
            local_tags: vec![],
            fingerprint: None,
        }
    }

    // ── R2: profileless peers cannot be added ─────────────────────────────────────

    #[test]
    fn reject_profileless_errs_when_peer_has_no_profile() {
        let peer = stub_peer("hb1_test", None);
        assert!(reject_profileless(&peer).is_err());
    }

    #[test]
    fn reject_profileless_ok_when_peer_has_profile() {
        let mut peer = stub_peer("hb1_test", None);
        peer.profile = Some(teaser_to_profile(Teaser {
            display_name: "archivebox".into(),
            bio: String::new(),
            tags: vec![],
            content_types: vec![],
            picture: None,
        }));
        assert!(reject_profileless(&peer).is_ok());
    }

    #[test]
    fn follow_sets_edited_petname() {
        // An explicit non-empty petname overrides whatever resolve_peer auto-derived.
        let mut peer = stub_peer("hb1_test", Some("AutoName"));
        apply_follow_petname(&mut peer, Some("MyNickname".into()));
        assert_eq!(peer.petname.as_deref(), Some("MyNickname"));

        // No petname arg (the trailing-Option default for existing callers) leaves the
        // auto-derived one alone.
        let mut peer2 = stub_peer("hb1_test2", Some("AutoName2"));
        apply_follow_petname(&mut peer2, None);
        assert_eq!(peer2.petname.as_deref(), Some("AutoName2"), "no petname arg keeps the auto-derived one");

        // An empty-string petname is treated the same as "no edit", not "clear it".
        apply_follow_petname(&mut peer2, Some(String::new()));
        assert_eq!(peer2.petname.as_deref(), Some("AutoName2"), "an empty-string petname is a no-op");
    }

    // ── devtest #4: a pasted full share code's browse-key survives the stale-teaser fallback ────

    #[test]
    fn merge_browse_key_sets_key_from_full_code() {
        // Add-by-npub leaves the contact keyless; pasting the full code later must attach the key
        // even when the teaser fetch flakes and we fall back to the stale cache.
        let pubkey = Identity::generate().public_key();
        let mut stale = stub_peer("hb1_test", None);
        assert!(stale.browse_key_hex.is_none(), "starts keyless (npub-added)");

        merge_browse_key(&mut stale, &ShareCode::Full { pubkey, browse_key: [7u8; 32] });
        assert_eq!(
            stale.browse_key_hex.as_deref(),
            Some(hex::encode([7u8; 32]).as_str()),
            "the full code's browse-key is merged onto the stale contact"
        );
    }

    #[test]
    fn merge_browse_key_followonly_leaves_key_untouched() {
        // A bare/FollowOnly code carries no key — it must not clobber an already-keyed contact.
        let pubkey = Identity::generate().public_key();
        let mut keyed = stub_peer("hb1_test", None);
        keyed.browse_key_hex = Some(hex::encode([9u8; 32]));

        merge_browse_key(&mut keyed, &ShareCode::FollowOnly { pubkey });
        assert_eq!(
            keyed.browse_key_hex.as_deref(),
            Some(hex::encode([9u8; 32]).as_str()),
            "a keyless code is a no-op, never a downgrade"
        );
    }

    /// Mirrors `set_contact_petname`'s core logic at the store level (load → set → reload) — the
    /// same pattern this file's other `State`-taking commands are exercised with (no live relay
    /// needed; `set_contact_tags` has no direct-call test either, for the same reason).
    #[test]
    fn set_contact_petname_updates_contact() {
        let (_dir, store) = test_store();
        let npub = "hb1_testpeer".to_string();
        let hash = CachedPeer::pubkey_hash(&npub);
        store.save_contact(&hash, &stub_peer(&npub, None)).unwrap();

        let mut peer = store.load_contact(&hash).unwrap().unwrap();
        peer.petname = Some("Nickname".into());
        store.save_contact(&hash, &peer).unwrap();

        let loaded = store.load_contact(&hash).unwrap().unwrap();
        assert_eq!(loaded.petname.as_deref(), Some("Nickname"), "the new petname must persist");
    }
}
