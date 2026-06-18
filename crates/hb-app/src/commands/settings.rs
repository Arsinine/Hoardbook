use std::time::Duration;

use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::{DataStore, Settings},
};

/// Probe a Nostr relay URL: connect with an ephemeral identity and confirm the handshake.
#[tauri::command]
pub async fn check_relay(url: String) -> CmdResult<()> {
    let ephemeral = hb_core::Identity::generate();
    let client = hb_net::RelayClient::connect(&ephemeral, &[url], Duration::from_secs(8))
        .await
        .map_err(cmd_err)?;
    client.disconnect().await;
    Ok(())
}

#[tauri::command]
pub async fn get_settings(store: State<'_, DataStore>) -> CmdResult<Settings> {
    Ok(store.load_settings().map_err(cmd_err)?.unwrap_or_default())
}

#[tauri::command]
pub async fn save_settings(
    settings: Settings,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    // Connect-per-command (M4): the relay set is read from storage on each command, so persisting
    // here is all that's needed — no shared relay client to update.
    store.save_settings(&settings).map_err(cmd_err)
}

/// Record that the one-time pre-first-download IP-exposure notice has been acknowledged. The UI
/// calls this once, before the first file download (browsing leaks nothing). Idempotent.
#[tauri::command]
pub async fn acknowledge_privacy_notice(store: State<'_, DataStore>) -> CmdResult<()> {
    let mut settings = store.load_settings().map_err(cmd_err)?.unwrap_or_default();
    settings.privacy_notice_acknowledged = true;
    store.save_settings(&settings).map_err(cmd_err)
}
