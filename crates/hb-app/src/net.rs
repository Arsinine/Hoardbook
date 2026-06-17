//! Connect-per-command Nostr relay access (M4 decision: connect-per-command, optimise later).
//!
//! Each command that touches the network opens a fresh [`RelayClient`] from the configured relay
//! set, uses it, and drops it. This sidesteps the startup-ordering problem (the identity may be
//! absent at launch) and `RelayClient::disconnect`'s self-consuming signature. A persistent shared
//! client is a later optimisation.

use std::time::Duration;

use anyhow::{anyhow, Result};
use hb_core::Identity;
use hb_net::RelayClient;

use crate::store::DataStore;

/// Handshake/fetch timeout for a per-command relay connection.
pub const RELAY_TIMEOUT: Duration = Duration::from_secs(10);

/// The configured relay set (seed + write). Empty until the user adds a relay in Settings.
pub fn relay_urls(store: &DataStore) -> Vec<String> {
    store.load_settings().ok().flatten().map(|s| s.relay_urls).unwrap_or_default()
}

/// Connect a fresh [`RelayClient`] for one command. Errors (actionably) if no relay is configured
/// or none completed the handshake.
pub async fn connect(identity: &Identity, store: &DataStore) -> Result<RelayClient> {
    let relays = relay_urls(store);
    if relays.is_empty() {
        return Err(anyhow!("No relays configured. Add a relay in Settings first."));
    }
    RelayClient::connect(identity, &relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| anyhow!("Could not connect to any relay: {e}"))
}
