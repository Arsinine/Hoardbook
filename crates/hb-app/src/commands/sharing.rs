use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    SharedEndpoint,
    error::{CmdResult, cmd_err},
    store::DataStore,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShareSettings {
    pub enabled: bool,
    /// Absolute filesystem path to the collection root directory.
    pub root_path: Option<String>,
    /// Relative paths (within root) that are downloadable. Empty = all files allowed.
    pub allowed_paths: Vec<String>,
    /// Speed cap in KB/s. None = unlimited.
    pub speed_cap_kbps: Option<u32>,
    /// Max simultaneous downloads allowed. None = unlimited.
    pub download_limit: Option<u32>,
    /// If true, only peers you follow can download.
    pub require_follow: bool,
}

#[tauri::command]
pub async fn get_share_settings(
    slug: String,
    store: State<'_, DataStore>,
) -> CmdResult<ShareSettings> {
    Ok(store.load_share_settings(&slug).map_err(cmd_err)?.unwrap_or_default())
}

#[tauri::command]
pub async fn save_share_settings(
    slug: String,
    settings: ShareSettings,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    store.save_share_settings(&slug, &settings).map_err(cmd_err)
}

/// Download a file from a peer's shared collection via direct iroh P2P connection.
#[tauri::command]
pub async fn request_download(
    peer_hb_id: String,
    peer_node_addr: Option<String>,
    slug: String,
    path: String,
    save_path: String,
    endpoint: State<'_, SharedEndpoint>,
    identity: State<'_, crate::SharedIdentity>,
) -> CmdResult<u64> {
    let addr_json = peer_node_addr.ok_or_else(|| {
        "Peer has no P2P address — they need to be online and running a recent Hoardbook version.".to_string()
    })?;

    // Include our own hb_id so the server can enforce require_follow.
    let my_hb_id = {
        let guard = identity.read().await;
        guard.as_ref().map(|kp| kp.hb_id())
    };

    // Clone the endpoint Arc so we don't hold the read lock during the transfer.
    let ep = {
        let guard = endpoint.read().await;
        guard.as_ref()
            .ok_or_else(|| "P2P transport not initialised. Generate or import a keypair first.".to_string())?
            .clone()
    };

    crate::transfer::download_file(&ep, &addr_json, &slug, &path, &save_path, my_hb_id)
        .await
        .map_err(|e| e.to_string())
}
