//! HTTP client for communicating with Hoardbook relays.

use anyhow::{anyhow, Context, Result};
use hb_core::{ChatMessage, SignedEnvelope};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::store::CachedPeer;

const BOOTSTRAP_RELAYS: &[&str] = &[
    "http://141.98.199.138:3000",
];

/// Bootstrap relay URLs are developer-controlled and trusted regardless of scheme.
fn is_bootstrap_relay(url: &str) -> bool {
    BOOTSTRAP_RELAYS.contains(&url.trim())
}

/// Insecure (http) relays are permitted only when explicitly enabled for dev/test.
/// Off by default in release so production clients never speak plaintext to a relay.
fn insecure_relays_allowed() -> bool {
    std::env::var("HB_ALLOW_INSECURE_RELAY").map(|v| v == "1").unwrap_or(false)
}

/// A relay URL is acceptable if it uses https, is a known bootstrap relay, or
/// insecure relays are explicitly enabled via `HB_ALLOW_INSECURE_RELAY=1`.
fn is_acceptable_relay_url(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("https://")
        || is_bootstrap_relay(u)
        || (insecure_relays_allowed() && u.starts_with("http://"))
}

/// Filter a candidate URL list down to acceptable relays, logging each rejection.
fn acceptable_relays<I: IntoIterator<Item = String>>(urls: I) -> Vec<String> {
    urls.into_iter()
        .filter(|u| {
            let ok = is_acceptable_relay_url(u);
            if !ok {
                tracing::warn!(
                    "ignoring insecure relay URL {u:?} (set HB_ALLOW_INSECURE_RELAY=1 to allow http for dev)"
                );
            }
            ok
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Relay GET /v1/peer/:pubkey response — online status + NodeAddr only.
/// Profile and collection data is fetched via iroh (T17/T20), not the relay.
#[derive(Debug, Deserialize)]
struct PeerResponse {
    online: bool,
    last_seen_at: Option<i64>,
    node_addr: Option<String>,
}

#[derive(Debug, Serialize)]
struct PublishRequest<'a> {
    #[serde(rename = "type")]
    doc_type: &'a str,
    document: &'a SignedEnvelope,
}

// ---------------------------------------------------------------------------
// RelayClient
// ---------------------------------------------------------------------------

pub struct RelayClient {
    http: Client,
    relay_urls: tokio::sync::RwLock<Vec<String>>,
}

impl RelayClient {
    pub fn new(extra_relays: Vec<String>) -> Self {
        let mut candidates: Vec<String> = BOOTSTRAP_RELAYS
            .iter()
            .map(|s| s.to_string())
            .collect();
        candidates.extend(extra_relays);
        let relay_urls = acceptable_relays(candidates);

        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            relay_urls: tokio::sync::RwLock::new(relay_urls),
        }
    }

    /// Return the current relay URL list.
    pub async fn get_relay_urls(&self) -> Vec<String> {
        self.relay_urls.read().await.clone()
    }

    /// Update relay URLs, always prepending bootstrap relays so they are never lost.
    pub async fn set_relay_urls(&self, user_urls: Vec<String>) {
        let mut urls: Vec<String> = BOOTSTRAP_RELAYS.iter().map(|s| s.to_string()).collect();
        for url in user_urls {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
        *self.relay_urls.write().await = acceptable_relays(urls);
    }

    /// Publish a signed envelope to all known relays.
    /// Returns Ok(()) if at least one relay accepts the document; logs failures for the rest.
    pub async fn publish(&self, doc_type: &str, envelope: &SignedEnvelope) -> Result<()> {
        let body = PublishRequest { doc_type, document: envelope };
        let relay_urls = self.relay_urls.read().await.clone();
        let mut succeeded = false;
        let mut last_err = anyhow!("no relays configured");

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/publish");
            match self.http.post(&endpoint).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    succeeded = true;
                }
                Ok(resp) => {
                    let err = anyhow!(
                        "relay {} returned {}: {}",
                        url,
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    );
                    tracing::warn!("{err}");
                    last_err = err;
                }
                Err(e) => {
                    tracing::warn!("relay {url} unreachable: {e}");
                    last_err = anyhow!("relay {} unreachable: {e}", url);
                }
            }
        }

        if succeeded { Ok(()) } else { Err(last_err) }
    }

    /// Fetch a peer's cached profile and collections from the relay.
    pub async fn fetch_peer(&self, hb_id: &str) -> Result<CachedPeer> {
        use tokio::task::JoinSet;

        let mut set: JoinSet<Result<PeerResponse>> = JoinSet::new();
        let relay_urls = self.relay_urls.read().await.clone();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/peer/{hb_id}");
            let client = self.http.clone();
            set.spawn(async move {
                let resp = client
                    .get(&endpoint)
                    .send()
                    .await
                    .context("relay unreachable")?;

                if !resp.status().is_success() {
                    return Err(anyhow!("relay returned {}", resp.status()));
                }

                resp.json::<PeerResponse>().await.context("invalid relay response")
            });
        }

        let mut last_err = anyhow!("no relays configured");
        while let Some(result) = set.join_next().await {
            match result {
                Ok(Ok(peer_resp)) => {
                    return Ok(parse_peer_response(hb_id, peer_resp));
                }
                Ok(Err(e)) => last_err = e,
                Err(e) => last_err = anyhow!("task error: {e}"),
            }
        }

        Err(last_err)
    }
}

/// Build a `CachedPeer` from a relay status response.
/// Profile and collections are populated later by the iroh browse flow (T20).
fn parse_peer_response(hb_id: &str, resp: PeerResponse) -> CachedPeer {
    let last_seen_at = resp.last_seen_at
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));

    CachedPeer {
        hb_id: hb_id.to_string(),
        profile: None,
        collections: vec![],
        online: resp.online,
        node_addr: resp.node_addr,
        last_fetched: chrono::Utc::now(),
        last_seen_at,
        local_tags: vec![],
    }
}

impl RelayClient {
    /// Fetch messages from all relays addressed to `my_pubkey`.
    pub async fn fetch_messages(
        &self,
        keypair: &hb_core::HoardbookKeypair,
    ) -> Result<Vec<(String, ChatMessage)>> {
        #[derive(Deserialize)]
        struct MessagesResponse {
            messages: Vec<SignedEnvelope>,
        }

        let relay_urls = self.relay_urls.read().await.clone();
        if relay_urls.is_empty() {
            return Err(anyhow!(
                "No secure relay configured. Add an https relay in Settings (or set HB_ALLOW_INSECURE_RELAY=1 for local development)."
            ));
        }

        let my_pubkey = keypair.hb_id();
        // H3: sign the mailbox-read request so the relay can prove we own this key.
        let signed_at = chrono::Utc::now().to_rfc3339();
        let signed = serde_json::json!({
            "purpose": "hoardbook.mailbox.read.v1",
            "public_key": my_pubkey,
            "signed_at": signed_at,
        });
        let signature = keypair.sign(&signed);

        let mut all_messages: Vec<(String, ChatMessage)> = Vec::new();
        let mut seen: std::collections::HashSet<(String, String)> = Default::default();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/messages/{my_pubkey}");
            match self
                .http
                .get(&endpoint)
                .query(&[("signed_at", signed_at.as_str()), ("signature", signature.as_str())])
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<MessagesResponse>().await {
                        for envelope in body.messages {
                            if envelope.verify().is_err() {
                                tracing::warn!("dropped message with invalid signature");
                                continue;
                            }
                            if let Ok(msg) = envelope.parse_payload::<ChatMessage>() {
                                let key = (
                                    envelope.public_key.clone(),
                                    msg.sent_at.to_rfc3339(),
                                );
                                if seen.insert(key) {
                                    all_messages.push((envelope.public_key, msg));
                                }
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("relay {url} messages fetch failed: {e}"),
            }
        }

        all_messages.sort_by_key(|(_, msg)| msg.sent_at);
        Ok(all_messages)
    }

    /// Ping a single relay URL to verify it is reachable.
    pub async fn check_url(url: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct HealthResponse { ok: bool }

        if !is_acceptable_relay_url(url) {
            return Err(anyhow!(
                "relay URL must use https:// (set HB_ALLOW_INSECURE_RELAY=1 to allow http for local development)"
            ));
        }

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;

        let endpoint = format!("{}/v1/health", url.trim_end_matches('/'));
        let resp = client
            .get(&endpoint)
            .send()
            .await
            .context("relay unreachable")?;

        if !resp.status().is_success() {
            return Err(anyhow!("relay returned HTTP {}", resp.status()));
        }

        let health: HealthResponse = resp
            .json()
            .await
            .context("relay response is not valid JSON — probably not an HB relay")?;

        if !health.ok {
            return Err(anyhow!("relay health check returned ok=false"));
        }

        Ok(())
    }

    /// Send a heartbeat to all relays.
    pub async fn send_heartbeat(
        &self,
        keypair: &hb_core::HoardbookKeypair,
        node_addr: Option<String>,
    ) -> Result<()> {
        use hb_core::{types::HeartbeatBody, DocType};

        // L16: heartbeats are a self-contained SignedEnvelope, verified by the relay
        // like every other signed document (no bespoke server-side reconstruction).
        let body = HeartbeatBody {
            node_addr,
            public_key: keypair.hb_id(),
            signed_at: chrono::Utc::now().to_rfc3339(),
        };
        let envelope = SignedEnvelope::create(keypair, DocType::Heartbeat, &body)?;

        let relay_urls = self.relay_urls.read().await.clone();
        for url in &relay_urls {
            let endpoint = format!("{url}/v1/heartbeat");
            if let Err(e) = self.http.post(&endpoint).json(&envelope).send().await {
                tracing::debug!("heartbeat to {url} failed: {e}");
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_peer_response_parsed() {
        let resp = PeerResponse {
            online: true,
            last_seen_at: Some(1_716_400_000),
            node_addr: Some("iroh://abc123".into()),
        };
        let cached = parse_peer_response("hb1_testkey", resp);
        assert!(cached.online);
        assert_eq!(cached.node_addr.as_deref(), Some("iroh://abc123"));
        assert!(cached.profile.is_none(), "relay response must not populate profile");
        assert!(cached.collections.is_empty(), "relay response must not populate collections");
    }

    #[test]
    fn offline_peer_response_parsed() {
        let resp = PeerResponse { online: false, last_seen_at: Some(1_000_000), node_addr: None };
        let cached = parse_peer_response("hb1_testkey", resp);
        assert!(!cached.online);
        assert!(cached.node_addr.is_none());
    }
}
