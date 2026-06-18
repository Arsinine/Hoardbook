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

/// Curated default seed relays a fresh install rides until the user customises their set. These are
/// public Nostr relays — there is **no Hoardbook-run SPOF** (spec §Relay Model) — chosen from the
/// set the launch survey (`RELAY_DEPLOY.md` §2) verified accept the Hoardbook kinds + brand-new
/// `npub`s + retention with no PoW. The user can remove/replace any of them in Settings; clearing
/// them all simply falls back here again, so the app is never left with zero relays.
pub const DEFAULT_RELAYS: &[&str] = &["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"];

/// The effective relay set (seed + write). A **fresh install** (no settings file yet) rides
/// [`DEFAULT_RELAYS`] so it works out of the box; a **configured** set is honoured **verbatim**,
/// including a deliberately-empty list — a privacy user who cleared their relays to go dark stays
/// dark, and `connect()` surfaces the actionable no-relays error rather than silently reconnecting
/// them to third-party defaults. (Distinguishing unset from explicitly-empty is the chorus finding.)
pub fn relay_urls(store: &DataStore) -> Vec<String> {
    match store.load_settings().ok().flatten() {
        None => DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
        Some(settings) => settings.relay_urls,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Settings;

    #[test]
    fn relay_urls_falls_back_to_defaults_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // No settings file at all (fresh install) → the public defaults, so the app can reach relays.
        assert_eq!(relay_urls(&store), DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn relay_urls_honours_a_deliberately_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // Settings saved with an empty list = the user chose zero relays → stays empty (NOT defaults);
        // connect() then surfaces the actionable no-relays error (chorus: don't override intent).
        store.save_settings(&Settings { relay_urls: vec![], ..Default::default() }).unwrap();
        assert!(relay_urls(&store).is_empty());
    }

    #[test]
    fn relay_urls_uses_configured_set_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_settings(&Settings { relay_urls: vec!["wss://my.relay".into()], ..Default::default() })
            .unwrap();
        assert_eq!(relay_urls(&store), vec!["wss://my.relay".to_string()]);
    }
}
