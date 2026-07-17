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

/// INV-5 guard (M16 W3): true iff `big_relay_url` (non-blank) names one of the configured public
/// `relay_urls` — in which case the full big-relay family would be published to a public relay. Relay
/// URLs are compared **canonically** via `nostr::RelayUrl`, the same normalization the relay pool keys
/// on (host case, default ports, trailing slash), so `wss://PUBLIC.example` and `wss://public.example`
/// are recognized as the same relay (Codex round-4). A URL that fails to parse falls back to a
/// case-insensitive, trailing-slash-normalized string compare. Pure — unit-tested.
fn big_relay_overlaps_public(big_relay_url: &str, relay_urls: &[String]) -> bool {
    let big_raw = big_relay_url.trim();
    if big_raw.is_empty() {
        return false;
    }
    let big_canon = nostr::RelayUrl::parse(big_raw).ok();
    relay_urls.iter().any(|u| {
        let u = u.trim();
        match (big_canon.as_ref(), nostr::RelayUrl::parse(u).ok()) {
            (Some(b), Some(p)) => *b == p,
            // Either side unparseable (shouldn't happen post-validation) → normalized string compare.
            _ => u.trim_end_matches('/').eq_ignore_ascii_case(big_raw.trim_end_matches('/')),
        }
    })
}

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
    // M16 W3: the optional big relay is a normal relay URL — validate it when set (empty = feature off).
    let big = settings.big_relay_url.trim();
    if !big.is_empty() {
        net::validate_relay_url(big)?;
        // INV-5 (Codex round-2/3): the big relay must be a SEPARATE relay the owner runs — never one of
        // the public relays. The full family is published *targeted* to the big relay, so if that URL is
        // also a public relay, the whole family would land on a public relay. Compare against the
        // **effective** public set used at publish time — the configured relays, OR `DEFAULT_RELAYS`
        // when the list is empty (`net::relay_urls` falls back to them), so an owner with no configured
        // relays can't set the big relay to a well-known default public relay and leak the family there.
        let effective_public: &[String] = if settings.relay_urls.is_empty() {
            net::DEFAULT_RELAYS.as_slice()
        } else {
            settings.relay_urls.as_slice()
        };
        if big_relay_overlaps_public(&settings.big_relay_url, effective_public) {
            return Err(
                "Your big relay must be a separate relay you run — it can't also be one of your public \
                 relays, or the full listing would be published to a public relay."
                    .into(),
            );
        }
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

#[cfg(test)]
mod tests {
    use super::big_relay_overlaps_public;

    #[test]
    fn big_relay_overlapping_a_public_relay_is_rejected() {
        let publics =
            vec!["wss://relay.a.example".to_string(), "wss://relay.b.example".to_string()];
        // Exact match with a public relay → overlap (INV-5 violation: the family would go public).
        assert!(big_relay_overlaps_public("wss://relay.a.example", &publics));
        // Trailing-slash / whitespace differences still count as the same relay.
        assert!(big_relay_overlaps_public("  wss://relay.b.example/  ", &publics));
        // A genuinely separate owner-run big relay → no overlap, allowed.
        assert!(!big_relay_overlaps_public("ws://my-big-relay.example:7777", &publics));
        // Blank big relay (feature off) → never an overlap.
        assert!(!big_relay_overlaps_public("", &publics));
        assert!(!big_relay_overlaps_public("   ", &publics));
    }

    #[test]
    fn big_relay_equal_to_a_default_public_relay_is_caught() {
        // Codex round-3: when relay_urls is empty, publish falls back to DEFAULT_RELAYS, so a big relay
        // equal to a default public relay must be rejected too (save_settings compares against the
        // effective set). The pure helper flags any member of that set.
        let defaults = &crate::net::DEFAULT_RELAYS;
        assert!(!defaults.is_empty(), "DEFAULT_RELAYS should be non-empty");
        assert!(big_relay_overlaps_public(&defaults[0], defaults));
    }

    #[test]
    fn big_relay_matching_a_public_relay_case_insensitively_is_caught() {
        // Codex round-4: relay URLs are case-insensitive on host (DNS), so a canonical `RelayUrl`
        // comparison must catch a big relay differing from a public relay only by host case, trailing
        // slash, or a default port — not just an exact string.
        let publics = vec!["wss://Relay.Example".to_string()];
        assert!(big_relay_overlaps_public("wss://relay.example", &publics), "host case must not bypass");
        assert!(big_relay_overlaps_public("wss://relay.example/", &publics), "trailing slash must not bypass");
        assert!(big_relay_overlaps_public("wss://RELAY.EXAMPLE", &publics));
        // A genuinely different host is still allowed (not an overlap).
        assert!(!big_relay_overlaps_public("wss://other.example", &publics));
        // Default port normalization: wss defaults to 443.
        assert!(big_relay_overlaps_public("wss://relay.example:443", &publics), "default port must not bypass");
    }
}
