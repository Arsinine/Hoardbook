use hb_core::HbId;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::{CachedPeer, DataStore},
    SharedIdentity, SharedRelay,
};

#[tauri::command]
pub async fn paste_key(
    hb_id: HbId,
    relay: State<'_, SharedRelay>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<CachedPeer> {
    let guard = identity.read().await;
    if let Some(ref kp) = *guard {
        if kp.hb_id() == *hb_id {
            return Err("You cannot look up your own ID".into());
        }
    }
    drop(guard);
    let peer = relay.fetch_peer(&hb_id).await.map_err(cmd_err)?;
    if peer.profile.is_none() {
        return Err("This peer has not published a profile yet".into());
    }
    Ok(peer)
}

#[tauri::command]
pub async fn follow(
    hb_id: HbId,
    relay: State<'_, SharedRelay>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let peer = relay.fetch_peer(&hb_id).await.map_err(cmd_err)?;
    let hash = CachedPeer::pubkey_hash(&hb_id);
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

#[tauri::command]
pub async fn get_contacts(store: State<'_, DataStore>) -> CmdResult<Vec<CachedPeer>> {
    store.list_contacts().map_err(cmd_err)
}

#[tauri::command]
pub async fn unfollow_contact(
    hb_id: HbId,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let hash = CachedPeer::pubkey_hash(&hb_id);
    store.delete_contact(&hash).map_err(cmd_err)
}

#[tauri::command]
pub async fn refresh_contact(
    hb_id: HbId,
    relay: State<'_, SharedRelay>,
    store: State<'_, DataStore>,
) -> CmdResult<CachedPeer> {
    let peer = relay.fetch_peer(&hb_id).await.map_err(cmd_err)?;
    let hash = CachedPeer::pubkey_hash(&hb_id);
    // Preserve local_tags across refresh.
    let existing = store.load_contact(&hash).map_err(cmd_err)?.unwrap_or_else(|| peer.clone());
    let mut updated = peer;
    updated.local_tags = existing.local_tags;
    store.save_contact(&hash, &updated).map_err(cmd_err)?;
    Ok(updated)
}

/// Set user-defined local tags on a contact. Tags are stored locally and never shared.
#[tauri::command]
pub async fn set_contact_tags(
    hb_id: HbId,
    tags: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let hash = CachedPeer::pubkey_hash(&hb_id);
    let mut peer = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {hb_id} not found"))?;
    peer.local_tags = tags;
    store.save_contact(&hash, &peer).map_err(cmd_err)
}
