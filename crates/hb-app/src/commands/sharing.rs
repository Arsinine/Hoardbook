//! Per-collection persisted root path. The whole download path (`request_download` /
//! `cancel_download`) and the download-config UI ("Share settings" dialog) were removed in v0.9.6 and
//! **have not come back**: M18 added a transport plane, but it is structurally limited to manifests
//! (INV-4′ — Hoardbook moves no *collection files*), so there is still nothing to configure a
//! download for. Those symbol names are swept by CI precisely so they cannot return. What remains is a
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
