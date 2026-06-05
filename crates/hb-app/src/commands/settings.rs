use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    relay::RelayClient,
    store::{DataStore, Settings},
    SharedRelay,
};

/// Probe a relay URL. Returns Ok(()) if reachable and valid.
#[tauri::command]
pub async fn check_relay(url: String) -> CmdResult<()> {
    RelayClient::check_url(&url).await.map_err(cmd_err)
}

#[tauri::command]
pub async fn get_settings(store: State<'_, DataStore>) -> CmdResult<Settings> {
    Ok(store.load_settings().map_err(cmd_err)?.unwrap_or_default())
}

#[tauri::command]
pub async fn save_settings(
    settings: Settings,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    store.save_settings(&settings).map_err(cmd_err)?;
    relay.set_relay_urls(settings.relay_urls.clone()).await;
    Ok(())
}
