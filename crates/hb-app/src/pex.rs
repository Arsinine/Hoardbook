//! Peer Exchange (PEX) — peer address gossip, spec Resolved Decision 9.
//!
//! Every node keeps a local address cache (`peers.json`, hb_id → last-known iroh
//! EndpointAddr + relay URL + last_seen). On every `/hoardbook/node/1` connection the
//! client opens a *second* bi-stream after the primary request completes and both sides
//! swap their caches in the background — the exchange never delays the primary request.
//!
//! Stream wire format (mirrors the node protocol framing):
//!   Initiator → Acceptor  [u32-LE len] [JSON PexMessage]
//!   Acceptor  → Initiator [u32-LE len] [JSON PexMessage]
//!
//! Trust model: entries are hints, not assertions. An `EndpointAddr` embeds the peer's
//! public key, and every consumer (`fetch_profile_via_iroh`, `download_file`) verifies
//! the addr's id against the expected hb_id before connecting, so a poisoned entry can
//! at worst point at an address that fails the TLS handshake.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::store::DataStore;

/// Cache bound (spec: 1,000 entries; oldest evicted when full).
const MAX_PEER_CACHE: usize = 1000;
/// Entries not refreshed in 30 days are purged on startup (spec).
const MAX_ENTRY_AGE_DAYS: i64 = 30;
/// Cap on a framed PEX message (1,000 entries × a few hundred bytes each).
const MAX_PEX_MESSAGE_BYTES: u32 = 2 * 1024 * 1024;
/// How long the server waits for the optional PEX stream after the primary request.
const PEX_ACCEPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// Overall bound on one PEX exchange (open/read/write), client and server side.
const PEX_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub type SharedPeerCache = Arc<Mutex<PeerCache>>;

/// Everything the server-side PEX hook needs: the shared cache plus the local endpoint
/// (for the self-entry it gossips). Cheap to clone into per-connection tasks.
#[derive(Clone)]
pub struct PexState {
    pub cache: SharedPeerCache,
    pub endpoint: iroh::Endpoint,
}

// ---------------------------------------------------------------------------
// Cache entry + wire message
// ---------------------------------------------------------------------------

/// One known peer address: spec data model `peers.json` entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAddrEntry {
    pub hb_id: String,
    /// JSON-serialised `iroh::EndpointAddr` (same encoding the relay heartbeat uses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PexMessage {
    peers: Vec<PeerAddrEntry>,
}

// ---------------------------------------------------------------------------
// PeerCache
// ---------------------------------------------------------------------------

pub struct PeerCache {
    store: DataStore,
    entries: HashMap<String, PeerAddrEntry>,
}

impl PeerCache {
    /// Load `peers.json`, dropping entries older than 30 days (spec startup purge).
    pub fn load(store: DataStore) -> Self {
        let mut entries = store.load_peer_cache().unwrap_or_default();
        let cutoff = Utc::now() - chrono::Duration::days(MAX_ENTRY_AGE_DAYS);
        let before = entries.len();
        entries.retain(|_, e| e.last_seen_at > cutoff);
        let cache = Self { store, entries };
        if before != cache.entries.len() {
            if let Err(e) = cache.save() {
                tracing::warn!("peer cache save after startup purge failed: {e}");
            }
        }
        cache
    }

    fn save(&self) -> Result<()> {
        self.store.save_peer_cache(&self.entries)
    }

    pub fn get(&self, hb_id: &str) -> Option<&PeerAddrEntry> {
        self.entries.get(hb_id)
    }

    /// Drop all in-memory entries (used by wipe_data after peers.json is deleted).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// All entries, for gossiping to a peer. Bounded by `MAX_PEER_CACHE` by construction.
    pub fn snapshot(&self) -> Vec<PeerAddrEntry> {
        self.entries.values().cloned().collect()
    }

    /// Insert or refresh one locally-learned entry (e.g. after a successful browse)
    /// and persist. Keeps whichever of old/new was seen more recently.
    pub fn record(&mut self, entry: PeerAddrEntry) {
        if self.upsert(entry) {
            if let Err(e) = self.save() {
                tracing::warn!("peer cache save failed: {e}");
            }
        }
    }

    /// Merge a gossiped peer list: validate each entry, prefer the most recent per
    /// hb_id, skip our own id, enforce the bound, persist. Returns how many entries
    /// were inserted or refreshed.
    pub fn merge(&mut self, incoming: Vec<PeerAddrEntry>, own_hb_id: &str) -> usize {
        let mut changed = 0;
        for entry in incoming.into_iter().take(MAX_PEER_CACHE) {
            if entry.hb_id == own_hb_id || !entry_is_valid(&entry) {
                continue;
            }
            if self.upsert(entry) {
                changed += 1;
            }
        }
        if changed > 0 {
            if let Err(e) = self.save() {
                tracing::warn!("peer cache save failed: {e}");
            }
        }
        changed
    }

    /// Returns true if the entry was inserted or replaced a less-recent one.
    fn upsert(&mut self, mut entry: PeerAddrEntry) -> bool {
        // A future timestamp would let one bad entry pin the cache; clamp to now.
        let now = Utc::now();
        if entry.last_seen_at > now {
            entry.last_seen_at = now;
        }
        match self.entries.get(&entry.hb_id) {
            Some(existing) if existing.last_seen_at >= entry.last_seen_at => false,
            _ => {
                self.entries.insert(entry.hb_id.clone(), entry);
                self.evict_to_bound();
                true
            }
        }
    }

    fn evict_to_bound(&mut self) {
        while self.entries.len() > MAX_PEER_CACHE {
            let oldest = self
                .entries
                .values()
                .min_by_key(|e| e.last_seen_at)
                .map(|e| e.hb_id.clone());
            match oldest {
                Some(id) => {
                    self.entries.remove(&id);
                }
                None => break,
            }
        }
    }
}

/// A gossiped entry is usable only if its hb_id decodes, it carries at least one way
/// to reach the peer, and any node_addr parses as a real `EndpointAddr` whose embedded
/// id matches the entry's hb_id (so a poisoned addr can't impersonate another entry).
fn entry_is_valid(entry: &PeerAddrEntry) -> bool {
    let Ok(pubkey) = hb_core::hb_id_decode(&entry.hb_id) else {
        return false;
    };
    if entry.node_addr.is_none() && entry.relay_url.is_none() {
        return false;
    }
    if let Some(ref addr_json) = entry.node_addr {
        let Ok(addr) = serde_json::from_str::<iroh::EndpointAddr>(addr_json) else {
            return false;
        };
        match iroh::EndpointId::from_bytes(&pubkey) {
            Ok(id) if addr.id == id => {}
            _ => return false,
        }
    }
    if let Some(ref url) = entry.relay_url {
        if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2048 {
            return false;
        }
    }
    true
}

/// Our own cache snapshot plus a fresh self-entry — what we gossip to the other side.
/// The self-entry is how addresses propagate without relay involvement.
fn outgoing_peers(cache: &PeerCache, endpoint: &iroh::Endpoint) -> Vec<PeerAddrEntry> {
    let mut peers = cache.snapshot();
    let self_entry = PeerAddrEntry {
        hb_id: hb_core::hb_id_encode(endpoint.id().as_bytes()),
        node_addr: serde_json::to_string(&endpoint.addr()).ok(),
        relay_url: None,
        last_seen_at: Utc::now(),
    };
    peers.truncate(MAX_PEER_CACHE - 1);
    peers.push(self_entry);
    peers
}

// ---------------------------------------------------------------------------
// Stream-level exchange (tested over duplex pairs)
// ---------------------------------------------------------------------------

async fn write_pex_message(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    peers: Vec<PeerAddrEntry>,
) -> Result<()> {
    let bytes = serde_json::to_vec(&PexMessage { peers }).context("serialize pex message")?;
    send.write_u32_le(bytes.len() as u32).await.context("write pex len")?;
    send.write_all(&bytes).await.context("write pex")?;
    Ok(())
}

async fn read_pex_message(
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Vec<PeerAddrEntry>> {
    let len = recv.read_u32_le().await.context("read pex len")?;
    if len > MAX_PEX_MESSAGE_BYTES {
        return Err(anyhow!("pex message too large: {len} bytes"));
    }
    let mut bytes = vec![0u8; len as usize];
    recv.read_exact(&mut bytes).await.context("read pex")?;
    let msg: PexMessage = serde_json::from_slice(&bytes).context("parse pex message")?;
    Ok(msg.peers)
}

/// Initiator side: send ours, read theirs.
pub(crate) async fn pex_exchange_initiate(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    ours: Vec<PeerAddrEntry>,
) -> Result<Vec<PeerAddrEntry>> {
    write_pex_message(&mut send, ours).await?;
    send.shutdown().await.context("shutdown pex send")?;
    read_pex_message(&mut recv).await
}

/// Acceptor side: read theirs, send ours.
pub(crate) async fn pex_exchange_accept(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    ours: Vec<PeerAddrEntry>,
) -> Result<Vec<PeerAddrEntry>> {
    let theirs = read_pex_message(&mut recv).await?;
    write_pex_message(&mut send, ours).await?;
    send.shutdown().await.context("shutdown pex send")?;
    Ok(theirs)
}

// ---------------------------------------------------------------------------
// Connection-level hooks
// ---------------------------------------------------------------------------

/// Client side: spawned (detached) after the primary request on a node connection
/// completes, so gossip never delays a profile fetch or DM. Takes ownership of the
/// connection and keeps it alive for the exchange; all failures are debug-logged.
pub(crate) fn spawn_client_pex(
    conn: iroh::endpoint::Connection,
    endpoint: iroh::Endpoint,
    cache: SharedPeerCache,
) {
    tokio::spawn(async move {
        let result = tokio::time::timeout(PEX_EXCHANGE_TIMEOUT, async {
            let ours = outgoing_peers(&*cache.lock().await, &endpoint);
            let (send, recv) = conn.open_bi().await.context("open pex stream")?;
            let theirs = pex_exchange_initiate(send, recv, ours).await?;
            let own_hb_id = hb_core::hb_id_encode(endpoint.id().as_bytes());
            let merged = cache.lock().await.merge(theirs, &own_hb_id);
            Ok::<usize, anyhow::Error>(merged)
        })
        .await;
        match result {
            Ok(Ok(n)) if n > 0 => tracing::debug!("pex: merged {n} peer entries"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::debug!("pex exchange failed (peer may predate PEX): {e}"),
            Err(_) => tracing::debug!("pex exchange timed out"),
        }
        // Let the peer observe our close once gossip is done.
        conn.close(0u32.into(), b"");
    });
}

/// Server side: after the primary node request, wait briefly for an optional PEX
/// stream from the client. Clients that predate PEX simply never open one and the
/// wait falls through to the connection drain. Failures are debug-logged.
pub(crate) async fn serve_pex_on_conn(
    conn: &iroh::endpoint::Connection,
    endpoint: &iroh::Endpoint,
    cache: &SharedPeerCache,
) {
    let accepted = tokio::time::timeout(PEX_ACCEPT_TIMEOUT, conn.accept_bi()).await;
    let Ok(Ok((send, recv))) = accepted else {
        return; // no PEX stream offered — not an error
    };
    let result = tokio::time::timeout(PEX_EXCHANGE_TIMEOUT, async {
        let ours = outgoing_peers(&*cache.lock().await, endpoint);
        let theirs = pex_exchange_accept(send, recv, ours).await?;
        let own_hb_id = hb_core::hb_id_encode(endpoint.id().as_bytes());
        Ok::<usize, anyhow::Error>(cache.lock().await.merge(theirs, &own_hb_id))
    })
    .await;
    match result {
        Ok(Ok(n)) if n > 0 => tracing::debug!("pex: merged {n} peer entries"),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::debug!("pex serve failed: {e}"),
        Err(_) => tracing::debug!("pex serve timed out"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::HoardbookKeypair;
    use tempfile::TempDir;

    fn test_cache() -> (TempDir, PeerCache) {
        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, PeerCache::load(store))
    }

    fn entry_for(kp: &HoardbookKeypair, age_days: i64) -> PeerAddrEntry {
        let id = iroh::EndpointId::from_bytes(&hb_core::hb_id_decode(&kp.hb_id()).unwrap()).unwrap();
        let addr = iroh::EndpointAddr { id, addrs: Default::default() };
        PeerAddrEntry {
            hb_id: kp.hb_id(),
            node_addr: Some(serde_json::to_string(&addr).unwrap()),
            relay_url: None,
            last_seen_at: Utc::now() - chrono::Duration::days(age_days),
        }
    }

    #[test]
    fn merge_keeps_most_recent_per_peer() {
        let (_dir, mut cache) = test_cache();
        let kp = HoardbookKeypair::generate();
        let old = entry_for(&kp, 10);
        let new = entry_for(&kp, 1);

        assert_eq!(cache.merge(vec![old.clone()], "me"), 1);
        assert_eq!(cache.merge(vec![new.clone()], "me"), 1, "newer entry must replace older");
        assert_eq!(cache.merge(vec![old], "me"), 0, "older entry must not replace newer");
        assert_eq!(
            cache.get(&kp.hb_id()).unwrap().last_seen_at,
            new.last_seen_at
        );
    }

    #[test]
    fn merge_skips_own_id_and_invalid_entries() {
        let (_dir, mut cache) = test_cache();
        let me = HoardbookKeypair::generate();
        let other = HoardbookKeypair::generate();

        let self_entry = entry_for(&me, 0);
        let bad_id = PeerAddrEntry {
            hb_id: "not_a_key".into(),
            node_addr: None,
            relay_url: Some("https://r.example".into()),
            last_seen_at: Utc::now(),
        };
        let no_address = PeerAddrEntry {
            hb_id: other.hb_id(),
            node_addr: None,
            relay_url: None,
            last_seen_at: Utc::now(),
        };
        // node_addr whose embedded endpoint id belongs to a different key.
        let mut mismatched = entry_for(&other, 0);
        mismatched.node_addr = entry_for(&me, 0).node_addr;

        let merged = cache.merge(vec![self_entry, bad_id, no_address, mismatched], &me.hb_id());
        assert_eq!(merged, 0, "self, invalid, address-less and id-mismatched entries must all be skipped");
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_bounded_evicts_oldest() {
        let (_dir, mut cache) = test_cache();
        // Fill beyond the bound with relay-url-only entries (cheap to construct).
        for i in 0..(MAX_PEER_CACHE + 5) {
            let kp = HoardbookKeypair::generate();
            cache.upsert(PeerAddrEntry {
                hb_id: kp.hb_id(),
                node_addr: None,
                relay_url: Some("https://r.example".into()),
                // Strictly increasing recency; the first 5 are the oldest.
                last_seen_at: Utc::now() - chrono::Duration::seconds((MAX_PEER_CACHE + 10 - i) as i64),
            });
        }
        assert_eq!(cache.len(), MAX_PEER_CACHE, "cache must stay bounded");
    }

    #[test]
    fn startup_purge_drops_stale_entries() {
        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let fresh = HoardbookKeypair::generate();
        let stale = HoardbookKeypair::generate();
        let mut entries = HashMap::new();
        entries.insert(fresh.hb_id(), entry_for(&fresh, 5));
        entries.insert(stale.hb_id(), entry_for(&stale, 31));
        store.save_peer_cache(&entries).unwrap();

        let cache = PeerCache::load(store);
        assert!(cache.get(&fresh.hb_id()).is_some(), "fresh entry survives startup");
        assert!(cache.get(&stale.hb_id()).is_none(), ">30-day entry purged on startup");
    }

    #[test]
    fn record_persists_to_disk() {
        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let mut cache = PeerCache::load(store.clone());
        let kp = HoardbookKeypair::generate();
        cache.record(entry_for(&kp, 0));

        let reloaded = PeerCache::load(store);
        assert!(reloaded.get(&kp.hb_id()).is_some(), "recorded entry must survive reload");
    }

    #[tokio::test]
    async fn pex_stream_roundtrip_merges_both_sides() {
        let (_d1, mut client_cache) = test_cache();
        let (_d2, mut server_cache) = test_cache();
        let client_kp = HoardbookKeypair::generate();
        let server_kp = HoardbookKeypair::generate();
        let known_by_client = HoardbookKeypair::generate();
        let known_by_server = HoardbookKeypair::generate();
        client_cache.record(entry_for(&known_by_client, 1));
        server_cache.record(entry_for(&known_by_server, 1));

        let (server_side, client_side) = tokio::io::duplex(64 * 1024);
        let (client_recv, client_send) = tokio::io::split(client_side);
        let (server_recv, server_send) = tokio::io::split(server_side);

        let server_ours = server_cache.snapshot();
        let server_task = tokio::spawn(async move {
            pex_exchange_accept(server_send, server_recv, server_ours).await.unwrap()
        });

        let received_by_client =
            pex_exchange_initiate(client_send, client_recv, client_cache.snapshot())
                .await
                .unwrap();
        let received_by_server = server_task.await.unwrap();

        assert_eq!(client_cache.merge(received_by_client, &client_kp.hb_id()), 1);
        assert!(client_cache.get(&known_by_server.hb_id()).is_some());
        assert_eq!(server_cache.merge(received_by_server, &server_kp.hb_id()), 1);
        assert!(server_cache.get(&known_by_client.hb_id()).is_some());
    }

    #[tokio::test]
    async fn oversized_pex_message_rejected() {
        let (mut a, b) = tokio::io::duplex(64 * 1024);
        let (mut b_recv, _b_send) = tokio::io::split(b);
        a.write_u32_le(MAX_PEX_MESSAGE_BYTES + 1).await.unwrap();
        let err = read_pex_message(&mut b_recv).await.unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }
}
