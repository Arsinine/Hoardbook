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
use hb_core::{ShareCode, Identity};
use hb_net::{browse_share_code, search_teasers, SearchHit};
use serde::Serialize;

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

    // Fall back to the cached contact if the relay yielded no teaser.
    if profile.is_none() {
        if let Some(mut stale) = store.load_contact(&CachedPeer::pubkey_hash(&npub)).map_err(cmd_err)? {
            stale.online = online;
            stale.fingerprint = fingerprint;
            return Ok(stale);
        }
    }

    Ok(CachedPeer {
        npub,
        source: crate::store::ContactSource::Manual,
        browse_key_hex: share_code.browse_key().map(hex::encode),
        petname: profile.as_ref().map(|p| p.display_name.clone()),
        profile,
        collections: vec![],
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
    if peer.profile.is_none() && peer.online {
        return Err("This peer has not published a profile yet".into());
    }
    Ok(peer)
}

#[tauri::command]
pub async fn follow(
    code: String,
    group_name: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    let peer = resolve_peer(&share_code, &me, &store, &relay).await?;
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
            },
        };
        let card = hit_to_card(hit);
        assert!(card.fingerprint.is_some(), "the §7 fingerprint is derived from the npub");
        assert_eq!(card.bio.as_deref(), Some("90s anime"));
        let json = serde_json::to_string(&card).unwrap();
        assert!(!json.contains("browse_key") && !json.contains("browseKey"), "no browse-key on a hit");
        assert!(!json.contains("listing"), "no listing on a hit (DISC3)");
    }

    #[test]
    fn hit_card_blank_bio_is_none() {
        let id = Identity::generate();
        let hit = SearchHit {
            npub: id.npub(),
            teaser: Teaser { display_name: "x".into(), bio: String::new(), tags: vec![], content_types: vec![] },
        };
        assert_eq!(hit_to_card(hit).bio, None, "a blank bio renders as None, not an empty string");
    }
}
