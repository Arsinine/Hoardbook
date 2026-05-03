use serde::Serialize;
use tauri_plugin_updater::UpdaterExt;

use crate::error::{CmdResult, cmd_err};

#[derive(Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub body: Option<String>,
}

/// Check whether a newer release is available.
/// Returns None if already on the latest version, or an error if the updater
/// is not configured (pubkey not set in tauri.conf.json).
#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> CmdResult<Option<UpdateInfo>> {
    let updater = app
        .updater_builder()
        .build()
        .map_err(cmd_err)?;
    let update = updater.check().await.map_err(cmd_err)?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version,
        body: u.body,
    }))
}

/// Download and install the latest release, then restart.
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> CmdResult<()> {
    let updater = app
        .updater_builder()
        .build()
        .map_err(cmd_err)?;
    let update = updater.check().await.map_err(cmd_err)?;
    if let Some(u) = update {
        u.download_and_install(|_, _| {}, || {})
            .await
            .map_err(cmd_err)?;
        app.restart();
    }
    Ok(())
}
