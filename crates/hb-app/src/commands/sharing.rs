use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::{
    SharedDownloadRegistry, SharedEndpoint,
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
/// Returns the download ID so the frontend can track progress or cancel.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn request_download(
    peer_hb_id: String,
    peer_node_addr: Option<String>,
    slug: String,
    path: String,
    save_path: String,
    expected_sha256: Option<String>,
    app: AppHandle,
    endpoint: State<'_, SharedEndpoint>,
    registry: State<'_, SharedDownloadRegistry>,
) -> CmdResult<u64> {
    let addr_json = peer_node_addr.ok_or_else(|| {
        "Peer has no P2P address — they need to be online and running a recent Hoardbook version.".to_string()
    })?;

    let ep = {
        let guard = endpoint.read().await;
        guard.as_ref()
            .ok_or_else(|| "P2P transport not initialised. Generate or import a keypair first.".to_string())?
            .clone()
    };

    let id = registry.next_id();
    let reg = (*registry).clone();

    // Spawn the transfer so the Tauri command returns the ID immediately
    // (the frontend subscribes to progress events instead of awaiting).
    tauri::async_runtime::spawn(async move {
        if let Err(e) = crate::transfer::download_file(
            &ep, &addr_json, &peer_hb_id, &slug, &path, &save_path, expected_sha256, id, reg, app,
        ).await {
            tracing::warn!("download {id} failed: {e}");
        }
    });

    Ok(id)
}

/// Cancel an active download by ID. The download loop will detect the signal,
/// close the connection, and delete the partial file.
#[tauri::command]
pub async fn cancel_download(
    download_id: u64,
    registry: State<'_, SharedDownloadRegistry>,
) -> CmdResult<bool> {
    Ok(registry.cancel(download_id).await)
}
