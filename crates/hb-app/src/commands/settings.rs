use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    relay::RelayClient,
    store::DataStore,
    SharedIdentity, SharedRelay,
};

fn default_true() -> bool { true }

/// Probe a relay URL. Returns Ok(()) if reachable and valid.
#[tauri::command]
pub async fn check_relay(url: String) -> CmdResult<()> {
    RelayClient::check_url(&url).await.map_err(cmd_err)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub relay_urls: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_dms: bool,
    /// When true, this user appears in the relay's public directory.
    #[serde(default)]
    pub recommended: bool,
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
    identity: State<'_, SharedIdentity>,
) -> CmdResult<()> {
    let old_recommended = store
        .load_settings()
        .ok()
        .flatten()
        .map(|s| s.recommended)
        .unwrap_or(false);

    store.save_settings(&settings).map_err(cmd_err)?;
    relay.set_relay_urls(settings.relay_urls.clone());

    // Send an immediate heartbeat so recommended status takes effect without waiting 5 minutes.
    if settings.recommended || old_recommended != settings.recommended {
        let guard = identity.read().await;
        if let Some(ref kp) = *guard {
            let _ = relay.send_heartbeat(kp, None, settings.recommended).await;
        }
    }

    Ok(())
}
