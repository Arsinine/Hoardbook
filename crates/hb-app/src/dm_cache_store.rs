//! The local, at-rest-encrypted DM cache (devtest v0.12.4 #2) — the persisted store side.
//!
//! Re-opening Chat used to re-download the WHOLE NIP-17 gift-wrap mailbox and re-run the expensive
//! unwrap on every message, every 15 s poll (`get_messages` → `client.fetch` with no `since`, no
//! cache). This caches the **decoded** received-contact inbox so the pane renders instantly, and lets
//! `get_messages` fetch only NEW wraps (`since`-bounded, dedup-by-id). Strangers stay in the Q7
//! `dm_requests` quarantine (unchanged); only contacts/self land here.
//!
//! On disk it is a single JSON string: the base64 NIP-44 ciphertext from `hb_core::seal_dm_cache`
//! (identity-derived key — confidential AND tamper-evident; see `hb_core::dm_cache`). A missing,
//! corrupt, foreign, or crypto-version-bumped blob opens as an empty cache and the next fetch rebuilds
//! it (self-healing). Wiped with the data dir (`DataStore::wipe`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use hb_core::Identity;

use crate::store::{read_json_lenient, write_json, DataStore};

/// Cap on cached contact messages (newest kept). Generous — real conversation volume is small; this
/// only backstops a pathological history (a single flooding contact evicts oldest across all peers —
/// accepted residual at this ceiling). Owner-ratification default.
const MAX_CACHED_MSGS: usize = 10_000;
/// Cap on remembered wrap ids. Sized well above the plausible in-48 h-window wrap count so the "never
/// re-unwrap a seen wrap" property holds under realistic volume; beyond it, an evicted id only costs a
/// redundant re-decode (bounded by the 48 h window + the request LRU), never a correctness loss.
/// Owner-ratification default.
const MAX_SEEN_WRAPS: usize = 20_000;

/// One decoded, verified DM. `wrap_id` (the gift-wrap event id) is the dedup key — a wrap already
/// present is never re-unwrapped. `from` is the Schnorr-verified seal author (never the wrap key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedDm {
    pub wrap_id: String,
    pub from: String,
    pub to: String,
    pub content: String,
    pub sent_at: String,
}

/// The sealed cache payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DmCache {
    /// Decoded contact/self messages, appended in fetch order.
    pub messages: Vec<CachedDm>,
    /// Wrap ids already decoded (contacts AND strangers) — the skip-re-unwrap ledger.
    pub seen_wraps: Vec<String>,
    /// Newest OUTER wrap `created_at` (unix secs) fetched — the incremental `since` cursor. Bandwidth
    /// only; NEVER a security/freshness boundary (NIP-59 fuzzes the outer stamp up to 2 days back, so
    /// the fetch subtracts a margin). `0` ⇒ never fetched (a full initial pull).
    pub newest_seen_outer: u64,
}

impl DmCache {
    /// Prune both vectors to their caps, dropping the oldest (front) entries. Returns whether it
    /// dropped anything — the caller folds that into its dirty flag so a balanced push+prune (which
    /// leaves both lengths unchanged) is still persisted.
    pub fn prune(&mut self) -> bool {
        let mut dropped = false;
        if self.messages.len() > MAX_CACHED_MSGS {
            let drop = self.messages.len() - MAX_CACHED_MSGS;
            self.messages.drain(0..drop);
            dropped = true;
        }
        if self.seen_wraps.len() > MAX_SEEN_WRAPS {
            let drop = self.seen_wraps.len() - MAX_SEEN_WRAPS;
            self.seen_wraps.drain(0..drop);
            dropped = true;
        }
        dropped
    }
}

impl DataStore {
    pub fn dm_cache_path(&self) -> PathBuf {
        self.base.join("dm_cache.json")
    }

    /// Load + open the sealed DM cache. A missing file, a parse/decrypt failure, or a crypto-version
    /// mismatch all yield an empty cache (self-healing — the next fetch rebuilds it from the relay).
    pub fn load_dm_cache(&self, identity: &Identity) -> Result<DmCache> {
        let sealed: Option<String> =
            read_json_lenient(&self.dm_cache_path()).context("reading DM cache")?;
        let Some(sealed) = sealed else { return Ok(DmCache::default()) };
        match hb_core::open_dm_cache(identity, &sealed) {
            Ok(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            Err(_) => Ok(DmCache::default()),
        }
    }

    /// Seal + persist the DM cache. Only the base64 ciphertext touches disk. Callers `prune()` first.
    pub fn save_dm_cache(&self, identity: &Identity, cache: &DmCache) -> Result<()> {
        let json = serde_json::to_string(cache).context("serializing DM cache")?;
        let sealed = hb_core::seal_dm_cache(identity, &json).context("sealing DM cache")?;
        write_json(&self.dm_cache_path(), &sealed).context("saving DM cache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dm(id: &str, from: &str) -> CachedDm {
        CachedDm {
            wrap_id: id.into(),
            from: from.into(),
            to: "npub1me".into(),
            content: "hi".into(),
            sent_at: "2026-07-22T00:00:00Z".into(),
        }
    }

    #[test]
    fn roundtrips_through_the_sealed_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let cache = DmCache {
            messages: vec![dm("w1", "npub1a")],
            seen_wraps: vec!["w1".into(), "w2".into()],
            newest_seen_outer: 1_700_000_000,
        };
        store.save_dm_cache(&me, &cache).unwrap();
        let loaded = store.load_dm_cache(&me).unwrap();
        assert_eq!(loaded.messages, cache.messages);
        assert_eq!(loaded.seen_wraps, cache.seen_wraps);
        assert_eq!(loaded.newest_seen_outer, 1_700_000_000);
    }

    #[test]
    fn on_disk_bytes_are_ciphertext_never_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let cache = DmCache { messages: vec![dm("w1", "npub1a")], ..Default::default() };
        // Use a recognisable plaintext to prove it isn't on disk.
        let cache = DmCache {
            messages: vec![CachedDm { content: "SECRET-BACKROOM".into(), ..cache.messages[0].clone() }],
            ..cache
        };
        store.save_dm_cache(&me, &cache).unwrap();
        let raw = std::fs::read_to_string(store.dm_cache_path()).unwrap();
        assert!(!raw.contains("SECRET-BACKROOM"), "message plaintext must not appear on disk");
    }

    #[test]
    fn a_foreign_identity_gets_an_empty_cache_not_a_decrypt_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let other = Identity::generate();
        store
            .save_dm_cache(&me, &DmCache { messages: vec![dm("w1", "npub1a")], ..Default::default() })
            .unwrap();
        // A different identity can't open it → self-heals to empty rather than erroring.
        assert!(store.load_dm_cache(&other).unwrap().messages.is_empty());
    }

    #[test]
    fn missing_file_is_an_empty_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let loaded = store.load_dm_cache(&me).unwrap();
        assert!(loaded.messages.is_empty() && loaded.seen_wraps.is_empty() && loaded.newest_seen_outer == 0);
    }

    #[test]
    fn prune_caps_both_vectors_dropping_oldest() {
        let mut cache = DmCache::default();
        for i in 0..(MAX_CACHED_MSGS + 10) {
            cache.messages.push(dm(&format!("w{i}"), "npub1a"));
        }
        for i in 0..(MAX_SEEN_WRAPS + 10) {
            cache.seen_wraps.push(format!("s{i}"));
        }
        assert!(cache.prune(), "prune reports it dropped over-cap entries");
        assert_eq!(cache.messages.len(), MAX_CACHED_MSGS);
        assert_eq!(cache.seen_wraps.len(), MAX_SEEN_WRAPS);
        // Newest kept: the last-appended id survives, the very first is evicted.
        assert_eq!(cache.messages.last().unwrap().wrap_id, format!("w{}", MAX_CACHED_MSGS + 9));
        assert!(!cache.seen_wraps.contains(&"s0".to_string()));
        // Under cap → no drop, reports false.
        let mut small = DmCache { messages: vec![dm("w1", "npub1a")], ..Default::default() };
        assert!(!small.prune());
    }
}
