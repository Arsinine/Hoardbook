//! Per-collection persisted root path. The whole download path (`request_download` /
//! `cancel_download`) and the download-config UI ("Share settings" dialog) were removed when file
//! transfer moved to the Mascara companion (Hoardbook INV-4 — moves no files). What remains is a
//! single read: the collection's on-disk root, used to pre-fill the re-scan dialog. The root is
//! *written* by the scan/prepare path (`commands::collection`), never by the UI, so there is no
//! `save_share_settings` command.

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
