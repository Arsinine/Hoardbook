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
use hb_core::{ShareCode, Identity};
use hb_net::browse_share_code;

use crate::{
    error::{CmdResult, cmd_err},
    net,
    store::{CachedPeer, DataStore},
    identity_state::SharedIdentity,
};

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
) -> Result<CachedPeer, String> {
    let peer = share_code.pubkey();
    let npub = peer.to_bech32().map_err(cmd_err)?;
    let seed = net::relay_urls(store);

    let client = net::connect(me, store).await.map_err(cmd_err)?;
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
    client.disconnect().await;

    let profile = browse.ok().and_then(|b| b.teaser).map(teaser_to_profile);

    // Fall back to the cached contact if the relay yielded no teaser.
    if profile.is_none() {
        if let Some(mut stale) = store.load_contact(&CachedPeer::pubkey_hash(&npub)).map_err(cmd_err)? {
            stale.online = online;
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
) -> CmdResult<CachedPeer> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    if me.public_key() == share_code.pubkey() {
        return Err("You cannot look up your own code".into());
    }
    let peer = resolve_peer(&share_code, &me, &store).await?;
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
) -> CmdResult<()> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    let peer = resolve_peer(&share_code, &me, &store).await?;
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
) -> CmdResult<CachedPeer> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let existing = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    let share_code = contact_share_code(&existing)?;
    let me = identity_clone(&identity).await?;
    let mut updated = resolve_peer(&share_code, &me, &store).await?;
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
