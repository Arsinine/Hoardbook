//! M16 W4 — a small on-disk LRU cache of full-listing manifests, keyed `(npub, slug, fingerprint)`.
//!
//! It restores offline browse of a once-imported manifest without a relay re-publish (INV-5-neutral):
//! after a user imports a `.hbmanifest`, the envelope is cached here, and a later truncated-teaser
//! browse of the same `(npub, slug, fingerprint)` re-opens it from disk before touching any relay.
//! **The cache is UNBOUNDED by default** (owner ruling 2026-09-01) — see
//! [`DEFAULT_MANIFEST_CACHE_BYTES`]. Entries live under `<base>/manifests/`, so
//! `DataStore::wipe` (which clears the whole base dir) covers this cache for free — no wipe change.
//!
//! The cache stores only browse-key-*encrypted* envelope bytes (the same safety class as any listing
//! already at rest); the reader re-verifies the author signature + re-decrypts on every read, so a
//! tampered cache file fails closed exactly like a tampered file import.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// **No default ceiling — the cache is unbounded (owner ruling 2026-09-01).**
///
/// This was `256 * 1024 * 1024`, chosen in the original M16 W4 cache commit when
/// [`hb_core::transport_payload::MANIFEST_MAX_TRANSPORT_BYTES`] was 8 MiB. That ceiling later
/// doubled to 16 MiB (QURATOR-106 follow-up) and this constant was never revisited, silently
/// halving worst-case capacity to ~16 entries — and entries are per `(npub, slug, fingerprint)`,
/// so one peer with several collections consumes several of them.
///
/// **Why unbounded rather than a bigger number.** This cache IS the offline copy; there is no
/// second one. Eviction therefore deletes durable user data — the collection you could read on a
/// plane last month is simply gone, with no warning and no way to recover it while offline. That
/// is the failure INV-8 exists to prevent, and it is a worse outcome than disk growth, which the
/// user can see and act on. Hoardbook's audience keeps software and game collections whose file
/// counts track disk contents (the very reason the 16 MiB ceiling was raised), so multi-MB
/// manifests are the expected case here, not the edge.
///
/// The eviction machinery below is retained and still unit-tested: `put` takes an explicit
/// `cap_bytes`, so reinstating a ceiling is a one-line change at the call sites, not a rewrite.
///
/// ⚠ Unbounded storage makes the absence of supersede-invalidation a permanent leak: a superseded
/// `(npub, slug)` entry is unreachable (lookup is fingerprint-exact) but is never removed, and LRU
/// was previously the only thing that ever collected it. Deleting other fingerprints of the same
/// `(npub, slug)` on a successful `put` is the fix; until then this directory grows monotonically
/// with every collection update.
pub const DEFAULT_MANIFEST_CACHE_BYTES: usize = usize::MAX;

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

/// The stable per-key filename: a domain-separated, length-prefixed hash of `(npub, slug)` so
/// distinct keys never collide onto one file and no key char (a slug `/`, say) reaches the path.
///
/// **Deliberately NOT keyed on the fingerprint** (owner ruling 2026-09-01). Including it meant every
/// update wrote a NEW file and left the old one behind — unreachable, because both [`get`] and
/// `resolve_from_cache` demand an exact fingerprint match, yet never collected. Keying on
/// `(npub, slug)` makes an update overwrite in place, so the directory is bounded by the number of
/// collections cached rather than by how often they change, and **no cleanup pass is ever needed**.
/// Reachability is unchanged: [`get`] still compares the stored fingerprint to the requested one, so
/// a superseded snapshot is a miss exactly as before.
fn entry_filename(npub: &str, slug: &str) -> String {
    let mut h = Sha256::new();
    for part in [npub, slug] {
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

    // Replace ATOMICALLY. This file is the only offline copy, and `put` now overwrites an existing
    // one — a crash midway through a plain write would destroy the old copy without landing the new,
    // leaving the user with neither. Write beside it, then rename (atomic within a filesystem). The
    // temp name ends `.tmp`, which `scan` skips, so a stranded temp is never mistaken for an entry.
    let final_path = dir.join(entry_filename(npub, slug));
    let tmp_path = final_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &final_path)?;

    // Only walk the directory when a caller actually asked for a ceiling. `scan` READS EVERY CACHED
    // MANIFEST to build its plan, so running it at the unbounded default would make each write cost
    // O(total cache bytes) — on a cache of multi-MB manifests that is the dominant cost of a fetch.
    if cap_bytes != usize::MAX {
        for name in eviction_plan(&scan(dir), cap_bytes) {
            let _ = std::fs::remove_file(dir.join(name)); // best-effort; a stuck file just lingers
        }
    }
    Ok(())
}

/// Look up a cached manifest by `(npub, slug, fingerprint)`, returning its envelope JSON and bumping
/// its `last_access` to `now` (LRU recency). `None` when absent, unreadable, or the stored key does
/// not match. The caller re-verifies + re-decrypts the envelope.
///
/// The stored-key comparison is **load-bearing, not just hash-collision defence**: since the file is
/// keyed on `(npub, slug)` alone, the entry on disk may hold a DIFFERENT (superseded) fingerprint,
/// and returning it would serve a stale snapshot as if it were current. Mismatch ⇒ `None`.
pub fn get(dir: &Path, npub: &str, slug: &str, fingerprint: &str, now: u64) -> Option<String> {
    let path = dir.join(entry_filename(npub, slug));
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

    /// Owner ruling 2026-09-01: an update OVERWRITES rather than accumulating a second file, so the
    /// directory is bounded by collections-cached, not by how often they change. This is what makes
    /// an unbounded cache safe: there is nothing to collect, so no cleanup pass is ever needed.
    ///
    /// MUTATION (P-10) — resolved by containing item: restore the fingerprint to `entry_filename`'s
    /// hashed parts (`for part in [npub, slug, fingerprint]`). The file count goes 1 → 2 and the
    /// first assertion reds; the superseded-miss assertion keeps passing, which is the point — the
    /// old scheme was correct about reachability and wrong only about storage.
    #[test]
    fn an_update_overwrites_in_place_and_leaves_exactly_one_file() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp1", "OLD", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        put(dir.path(), "npubA", "films", "fp2", "NEW", 20, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();

        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        assert_eq!(files.len(), 1, "an update must overwrite, not accumulate a second entry");

        // Reachability is unchanged by the re-keying: the current fingerprint hits, the superseded
        // one misses (it is simply gone now, rather than present-but-unreachable as before).
        assert_eq!(get(dir.path(), "npubA", "films", "fp2", 30).as_deref(), Some("NEW"));
        assert!(get(dir.path(), "npubA", "films", "fp1", 30).is_none(), "superseded is a miss");

        // A different collection under the same npub keeps its own file.
        put(dir.path(), "npubA", "music", "fpX", "OTHER", 40, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert_eq!(get(dir.path(), "npubA", "music", "fpX", 50).as_deref(), Some("OTHER"));
        assert_eq!(get(dir.path(), "npubA", "films", "fp2", 50).as_deref(), Some("NEW"));
    }

    /// No `.tmp` file survives a successful write — a stranded temp would be invisible to `scan`
    /// (wrong extension) and so would leak silently.
    ///
    /// MUTATION (P-10): replace the write+rename pair in `put` with a direct
    /// `std::fs::write(&final_path, &bytes)?` and delete the rename — this test still passes, so it
    /// is NOT the atomicity control; it only pins that no temp is left behind. Atomicity itself is
    /// not unit-testable without fault injection, and is claimed here only as far as `rename` gives it.
    #[test]
    fn a_successful_put_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "n", "s", "f", "ENV", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("tmp"))
            .collect();
        assert!(strays.is_empty(), "a stranded .tmp is invisible to scan and leaks silently");
    }

    /// Owner ruling 2026-09-01: the cache is UNBOUNDED, because it holds the only offline copy and
    /// eviction therefore destroys durable user data with no warning and no offline recovery.
    ///
    /// MUTATION (P-10) — resolved by containing item: change `DEFAULT_MANIFEST_CACHE_BYTES` from
    /// `usize::MAX` back to `256 * 1024 * 1024`. The `usize::MAX - 1` case below still passes (the
    /// entries are nowhere near 256 MB), so the first assertion is what reds: with a real ceiling
    /// the plan is no longer unconditionally empty at the default.
    #[test]
    fn the_default_cap_never_evicts_anything() {
        // Two entries far apart in recency and enormous in size: under ANY finite ceiling below
        // their total the older one would be evicted. At the default, nothing is.
        let entries = vec![
            ("old.json".to_string(), usize::MAX / 4, 10),
            ("new.json".to_string(), usize::MAX / 4, 30),
        ];
        assert!(
            eviction_plan(&entries, DEFAULT_MANIFEST_CACHE_BYTES).is_empty(),
            "the default must be unbounded — eviction deletes the user's only offline copy"
        );
        // And the machinery still works when a caller passes a real ceiling, so reinstating one
        // stays a one-line change rather than a rewrite.
        assert_eq!(eviction_plan(&entries, 100), vec!["old.json"]);
    }
}
