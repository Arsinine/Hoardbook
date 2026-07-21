//! M16 W4 — a small on-disk LRU cache of full-listing manifests, keyed `(npub, slug, fingerprint)`.
//!
//! It restores offline browse of a once-imported manifest without a relay re-publish (INV-5-neutral):
//! after a user imports a `.hbmanifest`, the envelope is cached here, and a later truncated-teaser
//! browse of the same `(npub, slug, fingerprint)` re-opens it from disk before touching any relay.
//! Multi-MB entries ⇒ a simple byte cap with least-recently-used eviction (default
//! [`DEFAULT_MANIFEST_CACHE_BYTES`], owner-ratifiable). Entries live under `<base>/manifests/`, so
//! `DataStore::wipe` (which clears the whole base dir) covers this cache for free — no wipe change.
//!
//! The cache stores only browse-key-*encrypted* envelope bytes (the same safety class as any listing
//! already at rest); the reader re-verifies the author signature + re-decrypts on every read, so a
//! tampered cache file fails closed exactly like a tampered file import.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Default cache ceiling in bytes (~256 MB of manifest envelopes). Owner-ratifiable; the eviction is
/// LRU once the stored total exceeds it.
pub const DEFAULT_MANIFEST_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// One cached manifest. `envelope` is the canonical `.hbmanifest` JSON; `last_access` (unix secs) is
/// bumped on every read to drive LRU recency.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    npub: String,
    slug: String,
    fingerprint: String,
    envelope: String,
    last_access: u64,
}

/// The stable per-key filename: a domain-separated, length-prefixed hash of `(npub, slug, fingerprint)`
/// so distinct keys never collide onto one file and no key char (a slug `/`, say) reaches the path.
fn entry_filename(npub: &str, slug: &str, fingerprint: &str) -> String {
    let mut h = Sha256::new();
    for part in [npub, slug, fingerprint] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    format!("{}.json", hex::encode(&h.finalize()[..16]))
}

/// Read every well-formed entry file in `dir` as `(filename, byte_len, last_access)`. A malformed or
/// unreadable file is skipped (a cache is best-effort — never a hard error).
fn scan(dir: &Path) -> Vec<(String, usize, u64)> {
    let mut out = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(parsed) = serde_json::from_slice::<CacheEntry>(&bytes) else { continue };
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else { continue };
        out.push((name, bytes.len(), parsed.last_access));
    }
    out
}

/// Least-recently-used eviction plan: given every entry's `(filename, bytes, last_access)` and a byte
/// `cap`, the filenames to delete — oldest `last_access` first — until the remaining total is within
/// `cap`. The single freshest entry is never evicted (a just-imported manifest survives even if it
/// alone exceeds the cap). Pure — unit-tested.
fn eviction_plan(entries: &[(String, usize, u64)], cap: usize) -> Vec<String> {
    let total: usize = entries.iter().map(|(_, b, _)| *b).sum();
    if total <= cap || entries.len() <= 1 {
        return Vec::new();
    }
    let mut order: Vec<&(String, usize, u64)> = entries.iter().collect();
    order.sort_by_key(|(_, _, la)| *la); // oldest first
    let mut running = total;
    let mut evict = Vec::new();
    for (name, bytes, _) in order {
        // Stop once within cap, or when only the freshest entry would remain (always keep one).
        if running <= cap || evict.len() + 1 >= entries.len() {
            break;
        }
        evict.push(name.clone());
        running -= bytes;
    }
    evict
}

/// Cache a manifest envelope under `(npub, slug, fingerprint)`, then enforce the byte cap by evicting
/// least-recently-used entries. Best-effort: a write or eviction I/O error is returned but never
/// corrupts the store (each entry is an independent file). `now` is the write's access time.
pub fn put(
    dir: &Path,
    npub: &str,
    slug: &str,
    fingerprint: &str,
    envelope: &str,
    now: u64,
    cap_bytes: usize,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let entry = CacheEntry {
        npub: npub.to_string(),
        slug: slug.to_string(),
        fingerprint: fingerprint.to_string(),
        envelope: envelope.to_string(),
        last_access: now,
    };
    let bytes = serde_json::to_vec(&entry).map_err(std::io::Error::other)?;
    std::fs::write(dir.join(entry_filename(npub, slug, fingerprint)), &bytes)?;

    for name in eviction_plan(&scan(dir), cap_bytes) {
        let _ = std::fs::remove_file(dir.join(name)); // best-effort; a stuck file just lingers
    }
    Ok(())
}

/// Look up a cached manifest by `(npub, slug, fingerprint)`, returning its envelope JSON and bumping
/// its `last_access` to `now` (LRU recency). `None` when absent, unreadable, or the stored key does
/// not match (defends against a hash collision). The caller re-verifies + re-decrypts the envelope.
pub fn get(dir: &Path, npub: &str, slug: &str, fingerprint: &str, now: u64) -> Option<String> {
    let path = dir.join(entry_filename(npub, slug, fingerprint));
    let bytes = std::fs::read(&path).ok()?;
    let mut entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    if entry.npub != npub || entry.slug != slug || entry.fingerprint != fingerprint {
        return None;
    }
    let envelope = entry.envelope.clone();
    // Bump recency (best-effort — a failed touch doesn't fail the read).
    entry.last_access = now;
    if let Ok(updated) = serde_json::to_vec(&entry) {
        let _ = std::fs::write(&path, &updated);
    }
    Some(envelope)
}

/// The cache directory under a store base — `DataStore::manifest_cache_dir` returns this.
pub fn cache_dir(base: &Path) -> PathBuf {
    base.join("manifests")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_roundtrips_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp1", "ENV-1", 100, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert_eq!(get(dir.path(), "npubA", "films", "fp1", 200).as_deref(), Some("ENV-1"));
    }

    #[test]
    fn get_is_key_scoped() {
        // A different npub, slug, OR fingerprint is a cache MISS (never serves the wrong manifest).
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp1", "ENV-1", 100, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert!(get(dir.path(), "npubB", "films", "fp1", 200).is_none());
        assert!(get(dir.path(), "npubA", "music", "fp1", 200).is_none());
        assert!(get(dir.path(), "npubA", "films", "fp2", 200).is_none(), "stale fingerprint misses");
    }

    #[test]
    fn get_of_absent_key_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(get(dir.path(), "npubA", "films", "fp1", 1).is_none());
    }

    #[test]
    fn eviction_drops_the_least_recently_used_until_under_cap() {
        // Three 100-byte entries, cap 250 → evict the single oldest (by last_access), keep two.
        let entries = vec![
            ("old.json".to_string(), 100, 10),
            ("mid.json".to_string(), 100, 20),
            ("new.json".to_string(), 100, 30),
        ];
        assert_eq!(eviction_plan(&entries, 250), vec!["old.json"]);
        // Under cap → evict nothing.
        assert!(eviction_plan(&entries, 1000).is_empty());
    }

    #[test]
    fn eviction_keeps_the_freshest_even_when_it_alone_exceeds_cap() {
        let entries = vec![("old.json".to_string(), 100, 10), ("huge.json".to_string(), 500, 30)];
        // cap 50 is smaller than either entry, but the freshest survives — only the old one is evicted.
        assert_eq!(eviction_plan(&entries, 50), vec!["old.json"]);
    }

    #[test]
    fn put_evicts_over_cap_on_disk_and_bumps_recency_on_read() {
        let dir = tempfile::tempdir().unwrap();
        // Each entry is well over 20 bytes serialized, so a tiny cap forces eviction to one entry.
        put(dir.path(), "n", "a", "f", "AAAA", 10, 10).unwrap();
        put(dir.path(), "n", "b", "f", "BBBB", 20, 10).unwrap();
        // 'a' is older; the tiny cap keeps only the freshest ('b').
        assert!(get(dir.path(), "n", "a", "f", 30).is_none(), "the LRU entry was evicted");
        assert_eq!(get(dir.path(), "n", "b", "f", 30).as_deref(), Some("BBBB"));
    }
}
