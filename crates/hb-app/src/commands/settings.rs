use std::time::Duration;

use tauri::State;

use hb_net::RelayHealth;

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    presence::{BeaconReport, SharedBeaconState},
    store::{DataStore, Settings},
};

/// Probe a Nostr relay URL: connect with an ephemeral identity and confirm the handshake. (Stays a
/// one-shot ephemeral probe — it must NOT ride the persistent shared client, whose identity + pool
/// are the user's; this answers "is this URL reachable at all".)
#[tauri::command]
pub async fn check_relay(url: String) -> CmdResult<()> {
    net::validate_relay_url(&url)?;
    let ephemeral = hb_core::Identity::generate();
    let client = hb_net::RelayClient::connect(&ephemeral, &[url], Duration::from_secs(8))
        .await
        .map_err(cmd_err)?;
    client.disconnect().await;
    Ok(())
}

/// Live per-relay reachability for the **configured** set on the data path (M12 W1, Decision D), so
/// a "–"/Offline read can say *why*. Reads the persistent shared client's per-relay status; before
/// any network use (or if the relay set can't connect at all) reports the configured relays as
/// `disconnected` rather than erroring.
#[tauri::command]
pub async fn relay_status(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<RelayHealth>> {
    let configured = net::relay_urls(store.inner());
    let disconnected = || {
        configured
            .iter()
            .map(|url| RelayHealth {
                url: url.clone(),
                status: "disconnected".into(),
                connected: false,
                last_error: None,
            })
            .collect::<Vec<_>>()
    };

    let id = {
        let guard = identity.read().await;
        match guard.as_ref() {
            Some(app) => app.identity.clone(),
            None => return Ok(disconnected()),
        }
    };
    match net::client(&id, store.inner(), relay.inner()).await {
        Ok(client) => Ok(client.relay_status().await),
        Err(_) => Ok(disconnected()),
    }
}

/// Per-relay outcome of the most recent presence-beacon publish (devtest #9 same-NAT diagnosis) —
/// the beacon rides the same write path as every outbound publish (DMs/discovery), so a per-relay
/// reject here is evidence for those too, not presence-only.
#[tauri::command]
pub async fn beacon_status(beacon: State<'_, SharedBeaconState>) -> CmdResult<BeaconReport> {
    Ok(beacon.read().await.clone())
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
    for url in &settings.relay_urls {
        net::validate_relay_url(url)?;
    }
    store.save_settings(&settings).map_err(cmd_err)?;
    // M12 W1: a relay-set change is an atomic build-and-swap — drop the shared client so the next
    // network use rebuilds it against the new set (the removed relay is then no longer dialed). A
    // no-op set change just rebuilds harmlessly on next use.
    net::reset(relay.inner()).await;
    Ok(())
}

/// Record that the one-time pre-first-download IP-exposure notice has been acknowledged. The UI
/// calls this once, before the first file download (browsing leaks nothing). Idempotent.
#[tauri::command]
pub async fn acknowledge_privacy_notice(store: State<'_, DataStore>) -> CmdResult<()> {
    let mut settings = store.load_settings().map_err(cmd_err)?.unwrap_or_default();
    settings.privacy_notice_acknowledged = true;
    store.save_settings(&settings).map_err(cmd_err)
}
