use hb_core::HbId;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    node::fetch_profile_via_iroh,
    relay::RelayClient,
    store::{CachedPeer, DataStore},
    SharedEndpoint, SharedIdentity, SharedRelay,
};

/// Resolve a peer to a `CachedPeer` with profile populated, in this priority order:
///   1. Ask the relay for liveness (online + node_addr).
///   2. If the peer is online, fetch profile + collections directly over iroh.
///   3. If iroh fails or the peer is offline, fall back to the local contact cache
///      with `online: false` (stale).
///   4. If there is no cache either, return an error.
async fn resolve_peer(
    hb_id: &str,
    relay: &RelayClient,
    endpoint_state: &tokio::sync::RwLock<Option<iroh::Endpoint>>,
    store: &DataStore,
) -> Result<CachedPeer, String> {
    let mut peer = relay.fetch_peer(hb_id).await.map_err(cmd_err)?;

    if peer.online {
        if let Some(addr) = peer.node_addr.clone() {
            let endpoint_opt = endpoint_state.read().await.clone();
            match endpoint_opt {
                Some(endpoint) => {
                    match fetch_profile_via_iroh(&endpoint, &addr, hb_id).await {
                        Ok((profile, collections)) => {
                            peer.profile = profile;
                            peer.collections = collections;
                        }
                        Err(e) => {
                            tracing::warn!("iroh-direct fetch for {hb_id} failed: {e}");
                        }
                    }
                }
                None => tracing::warn!("iroh endpoint not initialised — skipping direct fetch"),
            }
        }
    }

    if peer.profile.is_some() {
        return Ok(peer);
    }

    let hash = CachedPeer::pubkey_hash(hb_id);
    if let Some(mut stale) = store.load_contact(&hash).map_err(cmd_err)? {
        stale.online = false;
        return Ok(stale);
    }

    if peer.online {
        Err(format!("Could not fetch profile for {hb_id} (peer online but unreachable)"))
    } else {
        Err(format!("Peer {hb_id} is offline and not in your contacts"))
    }
}

#[tauri::command]
pub async fn paste_key(
    hb_id: HbId,
    relay: State<'_, SharedRelay>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, SharedEndpoint>,
    store: State<'_, DataStore>,
) -> CmdResult<CachedPeer> {
    let guard = identity.read().await;
    if let Some(ref kp) = *guard {
        if kp.hb_id() == *hb_id {
            return Err("You cannot look up your own ID".into());
        }
    }
    drop(guard);

    let peer = resolve_peer(&hb_id, &relay, &endpoint, &store).await?;

    // If we couldn't get a profile from iroh and there's no cache, resolve_peer already errored.
    // Here, profile is Some unless the peer is online but has not yet published one.
    if peer.profile.is_none() && peer.online {
        return Err("This peer has not published a profile yet".into());
    }
    Ok(peer)
}

#[tauri::command]
pub async fn follow(
    hb_id: HbId,
    group_name: Option<String>,
    relay: State<'_, SharedRelay>,
    endpoint: State<'_, SharedEndpoint>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let peer = resolve_peer(&hb_id, &relay, &endpoint, &store).await?;
    let hash = CachedPeer::pubkey_hash(&hb_id);
    store.save_contact(&hash, &peer).map_err(cmd_err)?;

    if let Some(gname) = group_name {
        let mut groups = store.load_groups().map_err(cmd_err)?;
        if let Some(group) = groups.iter_mut().find(|g| g.name == gname) {
            if !group.pubkeys.contains(&hb_id.to_string()) {
                group.pubkeys.push(hb_id.to_string());
                group.modified_at = chrono::Utc::now();
            }
            store.save_groups(&groups).map_err(cmd_err)?;
        }
        // Group not found → contact saved as Ungrouped; not an error.
    }

    Ok(())
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
    endpoint: State<'_, SharedEndpoint>,
    store: State<'_, DataStore>,
) -> CmdResult<CachedPeer> {
    let peer = resolve_peer(&hb_id, &relay, &endpoint, &store).await?;
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
