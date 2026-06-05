use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    relay::RelayClient,
    store::DataStore,
    SharedRelay,
};

fn default_true() -> bool { true }
fn default_dht_port() -> u16 { 6882 }

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
    /// Whether to announce tags/content-types on the mainline DHT.
    #[serde(default)]
    pub dht_announce_enabled: bool,
    /// Tags to announce on the DHT (only announced when dht_announce_enabled).
    #[serde(default)]
    pub dht_announce_tags: Vec<String>,
    /// Content types to announce on the DHT.
    #[serde(default)]
    pub dht_announce_content_types: Vec<String>,
    /// TCP port for the DHT identity server (announced as BEP 5 peer port).
    #[serde(default = "default_dht_port")]
    pub dht_identity_port: u16,
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
    relay.set_relay_urls(settings.relay_urls.clone());
    Ok(())
}
