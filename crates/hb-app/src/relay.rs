//! HTTP client for communicating with Hoardbook relays.

use anyhow::{anyhow, Context, Result};
use hb_core::{ChannelMessage, ChatMessage, SignedEnvelope};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::store::CachedPeer;

const BOOTSTRAP_RELAYS: &[&str] = &[
    "http://198.51.100.1:3000",
];

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PeerResponse {
    profile: Option<SignedEnvelope>,
    #[serde(default)]
    collections: Vec<SignedEnvelope>,
    online: bool,
    node_addr: Option<String>,
}

#[derive(Debug, Serialize)]
struct PublishRequest<'a> {
    #[serde(rename = "type")]
    doc_type: &'a str,
    document: &'a SignedEnvelope,
}

#[derive(Debug, Deserialize)]
pub struct DirectoryEntry {
    pub pubkey: String,
    /// The full signed profile envelope (so the caller can extract the profile).
    pub profile: SignedEnvelope,
}

#[derive(Debug, Deserialize)]
struct DirectoryResponse {
    peers: Vec<DirectoryEntry>,
}

#[derive(Debug, Deserialize)]
pub struct NameCheckResponse {
    pub available: bool,
    pub taken_by: Option<String>,
}

// ---------------------------------------------------------------------------
// RelayClient
// ---------------------------------------------------------------------------

pub struct RelayClient {
    http: Client,
    relay_urls: std::sync::RwLock<Vec<String>>,
}

impl RelayClient {
    pub fn new(extra_relays: Vec<String>) -> Self {
        let mut relay_urls: Vec<String> = BOOTSTRAP_RELAYS
            .iter()
            .map(|s| s.to_string())
            .collect();
        relay_urls.extend(extra_relays);

        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            relay_urls: std::sync::RwLock::new(relay_urls),
        }
    }

    /// Update relay URLs, always prepending bootstrap relays so they are never lost.
    pub fn set_relay_urls(&self, user_urls: Vec<String>) {
        let mut urls: Vec<String> = BOOTSTRAP_RELAYS.iter().map(|s| s.to_string()).collect();
        for url in user_urls {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
        *self.relay_urls.write().unwrap() = urls;
    }

    /// Publish a signed envelope to all known relays.
    pub async fn publish(&self, doc_type: &str, envelope: &SignedEnvelope) -> Result<()> {
        let body = PublishRequest { doc_type, document: envelope };
        let mut last_err = anyhow!("no relays configured");
        let relay_urls = self.relay_urls.read().unwrap().clone();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/publish");
            match self.http.post(&endpoint).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    last_err = anyhow!(
                        "relay {} returned {}: {}",
                        url,
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    );
                }
                Err(e) => {
                    last_err = anyhow!("relay {} unreachable: {e}", url);
                    tracing::warn!("relay {url} unreachable: {e}");
                }
            }
        }

        Err(last_err)
    }

    /// Fetch a peer's cached profile and collections from the relay.
    pub async fn fetch_peer(&self, hb_id: &str) -> Result<CachedPeer> {
        use tokio::task::JoinSet;

        let mut set: JoinSet<Result<PeerResponse>> = JoinSet::new();
        let relay_urls = self.relay_urls.read().unwrap().clone();

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
                    let profile = peer_resp.profile
                        .as_ref()
                        .and_then(|e| e.parse_payload().ok());
                    let collections = peer_resp.collections
                        .iter()
                        .filter_map(|e| e.parse_payload().ok())
                        .collect();
                    return Ok(CachedPeer {
                        hb_id: hb_id.to_string(),
                        profile,
                        collections,
                        online: peer_resp.online,
                        node_addr: peer_resp.node_addr,
                        last_fetched: chrono::Utc::now(),
                        local_tags: vec![],
                    });
                }
                Ok(Err(e)) => last_err = e,
                Err(e) => last_err = anyhow!("task error: {e}"),
            }
        }

        Err(last_err)
    }

    /// Fetch messages from all relays addressed to `my_pubkey`.
    pub async fn fetch_messages(
        &self,
        my_pubkey: &str,
    ) -> Result<Vec<(String, ChatMessage)>> {
        #[derive(Deserialize)]
        struct MessagesResponse {
            messages: Vec<SignedEnvelope>,
        }

        let relay_urls = self.relay_urls.read().unwrap().clone();
        let mut all_messages: Vec<(String, ChatMessage)> = Vec::new();
        let mut seen: std::collections::HashSet<(String, String)> = Default::default();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/messages/{my_pubkey}");
            match self.http.get(&endpoint).send().await {
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

    /// Fetch the relay's public directory of listed peers.
    /// Tries the first relay that responds.
    pub async fn fetch_directory(&self) -> Result<Vec<DirectoryEntry>> {
        let relay_urls = self.relay_urls.read().unwrap().clone();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/directory");
            match self.http.get(&endpoint).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<DirectoryResponse>().await {
                        return Ok(body.peers);
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("relay {url} directory fetch failed: {e}"),
            }
        }

        Ok(vec![])
    }

    /// Fetch recent messages from the general channel.
    pub async fn fetch_channel_messages(
        &self,
        channel: &str,
    ) -> Result<Vec<(String, ChannelMessage)>> {
        #[derive(Deserialize)]
        struct ChannelResponse {
            messages: Vec<SignedEnvelope>,
        }

        let relay_urls = self.relay_urls.read().unwrap().clone();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/channel/{channel}");
            match self.http.get(&endpoint).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<ChannelResponse>().await {
                        let mut msgs = Vec::new();
                        for envelope in body.messages {
                            if envelope.verify().is_err() {
                                tracing::warn!("dropped channel message with invalid signature");
                                continue;
                            }
                            if let Ok(msg) = envelope.parse_payload::<ChannelMessage>() {
                                msgs.push((envelope.public_key, msg));
                            }
                        }
                        return Ok(msgs);
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("relay {url} channel fetch failed: {e}"),
            }
        }

        Ok(vec![])
    }

    /// Post a signed channel message envelope to all relays.
    pub async fn post_channel_message(
        &self,
        channel: &str,
        envelope: &SignedEnvelope,
    ) -> Result<()> {
        let mut last_err = anyhow!("no relays configured");
        let relay_urls = self.relay_urls.read().unwrap().clone();

        for url in &relay_urls {
            let endpoint = format!("{url}/v1/channel/{channel}");
            match self.http.post(&endpoint).json(envelope).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                    last_err = anyhow!(
                        "Relay {} does not support public channels — redeploy with the latest hb-relay image.",
                        url
                    );
                }
                Ok(resp) => {
                    last_err = anyhow!(
                        "relay {} returned {}: {}",
                        url,
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    );
                }
                Err(e) => {
                    last_err = anyhow!("relay {} unreachable: {e}", url);
                    tracing::warn!("relay {url} unreachable: {e}");
                }
            }
        }

        Err(last_err)
    }

    /// Check if a display name is available on the relay (anti-spoofing).
    /// Returns `(available, taken_by_pubkey)`.
    pub async fn check_name(&self, display_name: &str) -> Result<NameCheckResponse> {
        let relay_urls = self.relay_urls.read().unwrap().clone();

        for url in &relay_urls {
            let encoded = urlencoding::encode(display_name);
            let endpoint = format!("{url}/v1/name/{encoded}");
            match self.http.get(&endpoint).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<NameCheckResponse>().await {
                        return Ok(body);
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("relay {url} name check failed: {e}"),
            }
        }

        // If no relay responds, assume available (don't block publishing).
        Ok(NameCheckResponse { available: true, taken_by: None })
    }

    /// Ping a single relay URL to verify it is reachable.
    pub async fn check_url(url: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct HealthResponse { ok: bool }

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
        listed: bool,
    ) -> Result<()> {
        use hb_core::types::HeartbeatBody;

        let signed_at = chrono::Utc::now().to_rfc3339();
        let body = HeartbeatBody {
            listed: if listed { Some(true) } else { None },
            node_addr: node_addr.clone(),
            public_key: keypair.hb_id(),
            signed_at: signed_at.clone(),
        };
        let body_value = serde_json::to_value(&body)?;
        let signature = keypair.sign(&body_value);

        #[derive(Serialize)]
        struct HeartbeatRequest {
            public_key: String,
            signed_at: String,
            node_addr: Option<String>,
            signature: String,
            listed: Option<bool>,
        }

        let req = HeartbeatRequest {
            public_key: keypair.hb_id(),
            signed_at,
            node_addr,
            signature,
            listed: if listed { Some(true) } else { None },
        };

        let relay_urls = self.relay_urls.read().unwrap().clone();
        for url in &relay_urls {
            let endpoint = format!("{url}/v1/heartbeat");
            if let Err(e) = self.http.post(&endpoint).json(&req).send().await {
                tracing::debug!("heartbeat to {url} failed: {e}");
            }
        }

        Ok(())
    }
}
