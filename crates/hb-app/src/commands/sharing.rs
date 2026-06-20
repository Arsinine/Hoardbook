//! File-sharing settings. The download path (`request_download` / `cancel_download`) was removed
//! when file transfer moved to the Mascara companion (Hoardbook INV-4 — moves no files); only the
//! collection owner's per-collection share settings live here now.

use tauri::State;

use crate::{
    store::{DataStore, ShareSettings},
    error::{CmdResult, cmd_err},
};

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

// The download path (`peer_browse_key` / `request_download` / `cancel_download`) moved to the
// Mascara companion with file transfer (Hoardbook INV-4). Hoardbook keeps only the share settings.
