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
//! already at rest). **It has TWO readers, and they do not carry the same guarantee** — this used to
//! read "the reader re-verifies ... on every read", which was true of one of them and false of the
//! other (QURATOR-172 #3, corrected 2026-09-02):
//!
//! - `commands::browse::resolve_from_cache` — the LOCAL browse reader. Does
//!   `get` -> `from_json` -> `verify_author(peer)` -> binds the AUTHOR-SIGNED
//!   `snapshot_fingerprint` to the teaser's -> `decrypt(browse_key)`. A tampered cache file fails
//!   closed here exactly like a tampered file import.
//! - `manifest_source::StoreManifestSource::payload`'s re-serve branch — the CARRIER-4 reader. Does
//!   `get_latest` (QURATOR-177 slice 1: resolves "newest cached for (author, slug)", no fingerprint
//!   pin) -> `from_json` -> `verify_author(ticket.author_npub)` -> `ManifestPayload::seal`.
//!   ⚑ **That `verify_author` was ADDED 2026-09-03** (QURATOR-164, owner ruling "C verifies before
//!   re-serving", closing QURATOR-172 #3); this bullet used to end "**verifies nothing**". `seal`
//!   itself is still only serialize + byte-bound and still leaves author verification to its
//!   caller — the caller now does it.
//!   ⚠ **It changed WHERE the refusal happens, not WHETHER the system is safe.** Read on.
//!
//! The re-serve branch was ALREADY safe before that check existed, and the reason is worth stating
//! because it is NOT "the reader checks": **the RECEIVER checks, before it commits.** The fetching
//! node runs its accept gate (`verify_author` pinned to the ticket's named author, plus slug bind and
//! completeness) INSIDE `fetch_manifest_with_progress`, before the ACK — and the ticket is spent
//! only on the ACK. So a tampered cache re-served by C reaches D and is refused **without burning
//! D's ticket**; the cost is a wasted round-trip and a confusing serve-side error, not a
//! disclosure or a lost capability. **That receiver-side gate remains the actual security
//! boundary** — C's new check is defence in depth and better attribution (the error now names the
//! serving node's own bad cache entry instead of surfacing as a confusing failure at D), and it
//! must not be treated as a reason to relax anything on the receive path.
//!
//! ⚠ That safety is a property of the RECEIVE path, not of this cache. **A third reader that
//! consumes these bytes locally — rendering, importing, or trusting them without its own
//! `verify_author` — would be a real hole**, because nothing in this module establishes
//! authenticity. Verify at the point of use, or hand the bytes to something that does.

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
/// ⚠ This paragraph once warned that unbounded storage made supersede-invalidation a permanent
/// leak, because `entry_filename` hashed `(npub, slug, fingerprint)` and so every update stranded
/// its predecessor, unreachable and uncollected. **That is no longer true, and the warning outlived
/// its fix by long enough to mislead a ticket.** `6f0bdaa` re-keyed on `(npub, slug)` alone, so a
/// `put` OVERWRITES in place and there is exactly one file per collection — pinned by
/// `an_update_overwrites_in_place_and_leaves_exactly_one_file`. Entries stranded by pre-`6f0bdaa`
/// builds are recovered, not discarded, by [`migrate_legacy_entries`]. The directory is therefore
/// bounded by collections-cached, not by how often they change, which is exactly what makes an
/// unbounded cache safe: there is nothing to collect, so no cleanup pass is ever needed.
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
/// Reachability is unchanged for the browse reader: [`get`] still compares the stored fingerprint to
/// the requested one, so a superseded snapshot is a miss exactly as before. (The Carrier-4 re-serve
/// reader [`get_latest`] deliberately opts OUT of that pin — it wants the snapshot currently stored.)
fn entry_filename(npub: &str, slug: &str) -> String {
    let mut h = Sha256::new();
    for part in [npub, slug] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    format!("{}.json", hex::encode(&h.finalize()[..16]))
}

/// Read every well-formed entry file in `dir` as
/// `(filename, byte_len, last_access, npub, slug, fingerprint)`. A
/// malformed or unreadable file is skipped (a cache is best-effort — never a hard error). The
/// `npub`/`slug` are the entry's OWN recorded key, not anything derived from the filename — that is
/// what lets [`migrate_legacy_entries`] tell a legacy file (named by a hash of
/// `(npub, slug, fingerprint)`) from a canonical one by comparison, not by parsing.
fn scan(dir: &Path) -> Vec<(String, usize, u64, String, String, String)> {
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
        out.push((
            name,
            bytes.len(),
            parsed.last_access,
            parsed.npub,
            parsed.slug,
            parsed.fingerprint,
        ));
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
    migrate_legacy_entries(dir);
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
    // The projection drops the key fields `scan` also returns — eviction sorts on bytes/recency only.
    if cap_bytes != usize::MAX {
        let entries: Vec<(String, usize, u64)> =
            scan(dir).into_iter().map(|(n, b, la, _, _, _)| (n, b, la)).collect();
        for name in eviction_plan(&entries, cap_bytes) {
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
    get_inner(dir, npub, slug, Some(fingerprint), now)
}

/// Look up the **newest cached copy** for `(npub, slug)` — the Carrier-4 re-serve reader
/// (QURATOR-177 slice 1, owner ruling 2026-09-03). The cache holds exactly ONE entry per
/// `(npub, slug)` — `entry_filename` hashes that pair and `put` replaces it atomically — so there
/// is no set of versions to choose among: "newest" is simply the entry at that path, whatever
/// fingerprint it carries. Skipping the fingerprint comparison is the ONLY behavioural difference
/// from [`get`]: the stored `npub`/`slug` are still validated (load-bearing against a mis-keyed or
/// hand-edited file — without it, an entry file renamed onto another key's name would serve the
/// wrong author's manifest), and `last_access` is still bumped best-effort so the read counts for
/// LRU recency.
pub fn get_latest(dir: &Path, npub: &str, slug: &str, now: u64) -> Option<String> {
    get_inner(dir, npub, slug, None, now)
}

/// The shared body of [`get`] and [`get_latest`]: read the single file keyed `(npub, slug)`,
/// validate the stored key against the lookup key, pin the fingerprint only when the caller asked
/// for an exact snapshot, and bump `last_access`.
fn get_inner(
    dir: &Path,
    npub: &str,
    slug: &str,
    fingerprint: Option<&str>,
    now: u64,
) -> Option<String> {
    let path = dir.join(entry_filename(npub, slug));
    let bytes = std::fs::read(&path).ok()?;
    let mut entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    if entry.npub != npub || entry.slug != slug {
        return None;
    }
    // `None` = the caller wants whatever snapshot is stored (no pin); `Some` = exact-match or miss.
    if fingerprint.is_some_and(|fp| entry.fingerprint != fp) {
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

/// Does this cache hold anything at all?
///
/// Deliberately CHEAP and IMPRECISE — it stops at the first entry and never parses one, because the
/// only caller uses it to decide whether to bind a listening endpoint. Binding one nothing dials
/// costs an idle endpoint; NOT binding one something dials costs a silently unreachable node, which
/// is the failure that actually hurts. When in doubt it must say yes.
///
/// ⚠ **It lives here rather than at its call site for a structural reason.** The caller is
/// `transport_state::has_servable_content`, and `transport_state.rs` is inside INV-4′'s swept
/// transport surface, which must touch NO filesystem — `std::fs`, a bare `fs::`, `PathBuf` and
/// `std::path::Path` are all flagged there. That fence is one of the four mechanisms keeping the
/// plane manifest-only, and any one of them alone is just a comment, so the read moves to the
/// module that owns the cache instead of the fence being widened to admit it.
pub fn has_any(dir: &Path) -> bool {
    std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// One cached manifest's identity: which collection, at which snapshot.
///
/// Deliberately carries no envelope — a caller enumerating the cache wants to know *what is here*,
/// and loading every manifest body to answer that would cost O(total cache bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedKey {
    pub npub: String,
    pub slug: String,
    pub fingerprint: String,
}

/// Every manifest currently cached, as `(npub, slug, fingerprint)`.
///
/// **This is the only way to ask "what do I hold?".** Both readers ([`get`] and [`get_latest`])
/// require a key the caller already has, so until this existed the cache could answer *do I have
/// this?* but never *what have I got?* — which is precisely what a background fetch driver needs
/// before it can notice that a held collection's fingerprint has moved on.
///
/// Order is filesystem order and carries no meaning; do not depend on it. A malformed or
/// unreadable entry is skipped rather than erroring, exactly as `scan` skips it — a cache is
/// best-effort, and one corrupt file must not blind the driver to every other collection.
///
/// Since the cache is keyed on `(npub, slug)` alone, a collection appears **at most once**, at
/// whatever snapshot was written last.
pub fn list(dir: &Path) -> Vec<CachedKey> {
    scan(dir)
        .into_iter()
        .map(|(_, _, _, npub, slug, fingerprint)| CachedKey { npub, slug, fingerprint })
        .collect()
}

/// The cache directory under a store base — `DataStore::manifest_cache_dir` returns this.
pub fn cache_dir(base: &Path) -> PathBuf {
    base.join("manifests")
}

/// Clear the ENTIRE cache — one global, all-or-nothing directory removal (QURATOR-176, owner
/// ruling 2026-09-03). NOT eviction: `eviction_plan`/LRU is never consulted here, and no cap is
/// applied — this is the cleanup affordance the no-caps ruling of 2026-09-02 acknowledged would
/// be needed, and it takes no per-entry policy because it deletes the directory wholesale.
///
/// Idempotent: an already-absent directory is success, not an error (`put` recreates the
/// directory on demand, so nothing needs to be recreated here). Scope is the manifest cache
/// ONLY — never the wider store (`DataStore::wipe` is a different, destructive operation).
pub fn clear_all(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Directories already migrated this process. Hoisted to MODULE scope — a per-function static is
/// never serialized (CLAUDE.md §9) and the guard must cover every call site, not one closure.
static MIGRATED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::OnceLock::new();

/// The on-disk sentinel naming a directory whose legacy migration has completed. The in-process
/// [`MIGRATED`] guard is keyed per PROCESS — without this marker, every launch would pay a full
/// `scan` on its first `put`, and `scan` READS EVERY CACHED MANIFEST: the exact O(total cache
/// bytes) cost the eviction comment in `put` refuses to pay per write, reintroduced once per
/// launch.
///
/// Invisible to the cache by construction: `scan` admits only `.json` files that parse as
/// [`CacheEntry`], and this name is neither, so `eviction_plan` (fed by `scan`) and `get` (an
/// exact `<hex>.json` path lookup) can never see it.
const MIGRATION_MARKER: &str = ".hb-migrated-v1";

/// One-time, best-effort migration of legacy fingerprint-keyed entries (QURATOR-165).
///
/// Before the `(npub, slug)` re-keying, `entry_filename` hashed THREE parts —
/// `(npub, slug, fingerprint)` — so every update wrote a NEW file and left the old one behind. On
/// any install that ran a pre-`6f0bdaa` build, those files are still there: unreachable ([`get`]
/// looks up the two-part name, a different path) and never collected (the cap defaults to
/// `usize::MAX`, so LRU never runs). This pass recovers the data instead of discarding it — this
/// cache holds the ONLY offline copy of a manifest, and discarding a recoverable copy is an INV-8
/// outcome, not cleanup.
///
/// **The discriminator is exact, not heuristic:** every entry file carries its own `npub`/`slug`
/// (see [`CacheEntry`]), so a file is legacy iff `filename != entry_filename(entry.npub,
/// entry.slug)`. No filename parsing, no timestamp guessing.
///
/// Per distinct `(npub, slug)` among legacy files: if no canonical file exists, RENAME the newest
/// (by `last_access`) legacy entry into the canonical name — recovering an offline copy that is
/// currently invisible — then delete the rest; if a canonical file already exists, delete all the
/// legacy ones for that key. **Ordering is load-bearing:** the rename (or confirmed canonical
/// presence) completes BEFORE any delete for that key, so a crash mid-migration leaves a readable
/// copy, never neither — the same posture as `put`'s write-beside-then-rename.
///
/// Best-effort, matching `put`: a single failed rename or unlink skips that entry and carries on;
/// nothing is propagated as a hard error that could break browsing. Runs once per process per
/// directory (see [`MIGRATED`]), invoked from [`put`]. The work itself lives in
/// [`migrate_legacy_dir`], unguarded, so the tests can run it twice for idempotency.
fn migrate_legacy_entries(dir: &Path) {
    // Marker first: once a directory has completed a pass, the sentinel alone short-circuits, so
    // steady state is one `exists()` per process — not a full `scan` on every first launch.
    if dir.join(MIGRATION_MARKER).exists() {
        return;
    }
    {
        let mut done = MIGRATED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if done.contains(dir) {
            return;
        }
        done.insert(dir.to_path_buf());
    }
    migrate_legacy_dir(dir);
    // Mark only AFTER the pass completes, so a crash mid-pass leaves the directory unmarked and
    // the next launch retries. Best-effort: an unwritable marker means the pass simply runs again
    // next launch — never an error, never a skipped migration.
    let _ = std::fs::write(dir.join(MIGRATION_MARKER), b"");
}

/// The migration work, unguarded — see [`migrate_legacy_entries`] for the rationale and posture.
fn migrate_legacy_dir(dir: &Path) {
    // Group legacy files by the key they themselves claim. A malformed/unreadable file never
    // reaches here (scan skips it) and is therefore left alone — it is indistinguishable from a
    // non-entry file, and deleting unknown files is not this pass's business.
    let mut by_key: std::collections::HashMap<(String, String), Vec<(String, u64)>> =
        std::collections::HashMap::new();
    for (name, _bytes, last_access, npub, slug, _fingerprint) in scan(dir) {
        if name != entry_filename(&npub, &slug) {
            by_key.entry((npub, slug)).or_default().push((name, last_access));
        }
    }
    for ((npub, slug), mut group) in by_key {
        let canonical = entry_filename(&npub, &slug);
        // The survivor, if a rename is needed: the NEWEST legacy copy by `last_access`. The rename
        // lands its content under the canonical name FIRST; only then are the others deleted, so
        // no delete can leave the key with neither file. (A legacy name can never equal the
        // canonical one — that inequality is what put the file in this group — so the renamed
        // source's own `remove_file` below no-ops on a path that no longer exists.)
        group.sort_by_key(|(_, la)| std::cmp::Reverse(*la)); // newest first
        if !dir.join(&canonical).exists() {
            if let Some((newest, _)) = group.first() {
                if std::fs::rename(dir.join(newest), dir.join(&canonical)).is_err() {
                    continue; // best-effort: skip this key entirely, retry on a later process
                }
            }
        }
        for (name, _) in group {
            let _ = std::fs::remove_file(dir.join(name)); // best-effort
        }
    }
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

    /// QURATOR-177 slice 1 — the Carrier-4 re-serve reader resolves "newest cached copy for
    /// `(npub, slug)`": the single entry at that key's path, whatever fingerprint it carries.
    /// A refetch `put` replaces the entry in place, so a stored fingerprint that differs from
    /// anything an older reader asked for is the NORMAL state, not a mismatch.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `get_inner`
    /// (crates/hb-app/src/manifest_cache.rs), change the fingerprint gate
    /// `if fingerprint.is_some_and(|fp| entry.fingerprint != fp) { return None; }` to fire on
    /// EVERY caller — `if Some(entry.fingerprint.as_str()) != fingerprint { return None; }`.
    /// `get_latest` passes `None` while the stored entry holds `fp2`, so the mutant misses and
    /// the `assert_eq!` below (which expects `Some("NEW")`) reds with `None`. The `get` round-trip
    /// above keeps passing — that caller passes a matching `Some`, which is the point: the
    /// mutation discriminates the two readers.
    #[test]
    fn get_latest_serves_the_stored_snapshot_without_a_fingerprint_pin() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp1", "OLD", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        // The refetch: same key, new fingerprint — replaces in place.
        put(dir.path(), "npubA", "films", "fp2", "NEW", 20, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert_eq!(get_latest(dir.path(), "npubA", "films", 30).as_deref(), Some("NEW"));
        // And the pinned reader still misses on the superseded fingerprint — unchanged.
        assert!(get(dir.path(), "npubA", "films", "fp1", 30).is_none(), "stale fingerprint misses");
    }

    /// The stored-key check in `get_latest` is load-bearing, not decoration: the file is named by
    /// a hash of `(npub, slug)`, so an entry whose RECORDED key disagrees with the lookup key
    /// means a mis-keyed or hand-edited file — serving it would answer one author's re-serve with
    /// another author's manifest. Mismatch ⇒ `None`, exactly as for `get`.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `get_inner`
    /// (crates/hb-app/src/manifest_cache.rs), delete the stored-key guard
    /// `if entry.npub != npub || entry.slug != slug { return None; }`. The fixture below uses
    /// `write_entry` (the same helper the migration tests use) to plant an entry whose recorded
    /// key is `(npubA, films)` under the FILENAME `entry_filename("npubB", "films")` resolves
    /// to — a name/key disagreement no `put` can produce. The mutant serves it and the `None`
    /// assertion reds. (`get` has its own copy of the pin through the same guard; the mutation
    /// therefore also reds `get_is_key_scoped`, which is fine — the point is THIS test names the
    /// `get_latest` half.)
    #[test]
    fn get_latest_still_rejects_a_mis_keyed_entry_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        write_entry(
            dir.path(),
            &entry_filename("npubB", "films"),
            "npubA",
            "films",
            "fp1",
            "WRONG-KEY",
            10,
        );
        assert!(get_latest(dir.path(), "npubB", "films", 20).is_none());
    }

    /// `get_latest` bumps `last_access` exactly as `get` does, so a re-serve read counts for LRU
    /// recency — a re-served collection must not become the first thing a future cap evicts.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `get_inner`
    /// (crates/hb-app/src/manifest_cache.rs), delete the best-effort recency bump
    /// (`entry.last_access = now;` and the `if let Ok(updated) = ... { let _ = ... }` block).
    /// Re-read the entry with `scan` afterwards and assert the recorded `last_access` moved from
    /// 10 to 200; under the mutant it stays 10 and the assert reds. (`scan` returns
    /// `(filename, byte_len, last_access, npub, slug, fingerprint)`, so the bump is directly
    /// observable.)
    #[test]
    fn get_latest_bumps_last_access_for_lru_recency() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp1", "ENV-1", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert!(get_latest(dir.path(), "npubA", "films", 200).is_some());
        let entries = scan(dir.path());
        let entry = entries.iter().find(|(_, _, _, npub, slug, _)| npub == "npubA" && slug == "films")
            .expect("the entry is still there");
        assert_eq!(entry.2, 200, "a get_latest read must bump last_access to `now`");
    }

    /// MUTATION (P-10) — in `has_any`, change `d.next().is_some()` to `d.next().is_none()` →
    /// this test reds on both asserts.
    ///
    /// The direction is the whole point: this answers "bind a listener?", and the doc's own rule is
    /// that when in doubt it must say YES. Inverted, a node holding cached manifests would decline
    /// to bind and go silently unreachable — the failure the check exists to prevent.
    #[test]
    fn has_any_is_true_exactly_when_the_cache_holds_something() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_any(dir.path()), "an empty cache dir holds nothing");
        put(dir.path(), "npubA", "films", "fp-A", "ENV-A", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert!(has_any(dir.path()), "one cached manifest is enough to make a dial possible");
    }

    /// MUTATION (P-10) — in `has_any`, change `.unwrap_or(false)` to `.unwrap_or(true)` → this test
    /// reds. A missing directory is a cold cache, not a reason to bind.
    #[test]
    fn has_any_is_false_for_a_directory_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_any(&dir.path().join("never-created")));
    }

    /// MUTATION (P-10) — in `scan`'s `out.push((...))`, replace the `parsed.fingerprint` field
    /// with `String::new()` → this test reds. (Anchored to the push expression in `scan`, not to
    /// the word "fingerprint", which appears throughout this file's prose and struct fields.)
    #[test]
    fn list_reports_every_cached_collection_with_its_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp-A", "ENV-A", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        put(dir.path(), "npubB", "music", "fp-B", "ENV-B", 11, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();

        let mut got = list(dir.path());
        got.sort_by(|a, b| a.npub.cmp(&b.npub));
        assert_eq!(
            got,
            vec![
                CachedKey { npub: "npubA".into(), slug: "films".into(), fingerprint: "fp-A".into() },
                CachedKey { npub: "npubB".into(), slug: "music".into(), fingerprint: "fp-B".into() },
            ],
            "the driver needs the fingerprint to tell a current copy from a superseded one"
        );
    }

    /// MUTATION (P-10) — in `scan`, replace the `let Ok(read_dir) = std::fs::read_dir(dir) else
    /// { return out; };` binding with `let read_dir = std::fs::read_dir(dir).unwrap();` → this
    /// test reds (it panics on the absent directory instead of reporting nothing held).
    #[test]
    fn list_is_empty_for_a_cache_directory_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        assert!(list(&missing).is_empty(), "a cold cache holds nothing; it is not an error");
    }

    /// MUTATION (P-10) — in `entry_filename`, add the fingerprint to the hashed parts (change the
    /// `for part in [npub, slug]` array to `[npub, slug, "x"]` is NOT enough — instead take a
    /// third `fingerprint: &str` param and hash it, updating the two call sites) → this test reds
    /// with 2 entries instead of 1.
    ///
    /// This is the pin on the owner ruling of 2026-09-01 that the filename is keyed on
    /// `(npub, slug)` ALONE: keying it on the fingerprint too made every update strand an
    /// unreachable file. If that ever regresses, `list` would report the same collection several
    /// times at different snapshots and the driver would chase snapshots that no reader can reach.
    #[test]
    fn list_reports_one_entry_per_collection_at_the_newest_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp-old", "ENV-1", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        put(dir.path(), "npubA", "films", "fp-new", "ENV-2", 20, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();

        let got = list(dir.path());
        assert_eq!(got.len(), 1, "the cache is keyed on (npub, slug), so an update overwrites");
        assert_eq!(got[0].fingerprint, "fp-new", "and what remains is the newest snapshot");
    }

    /// MUTATION (P-10) — in `scan`, replace the `let Ok(parsed) =
    /// serde_json::from_slice::<CacheEntry>(&bytes) else { continue };` binding with
    /// `let parsed = serde_json::from_slice::<CacheEntry>(&bytes).unwrap();` → this test reds
    /// (it panics on the corrupt file instead of skipping it).
    #[test]
    fn list_skips_a_malformed_entry_rather_than_losing_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp-A", "ENV-A", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        std::fs::write(dir.path().join("corrupt.json"), b"{not json").unwrap();

        let got = list(dir.path());
        assert_eq!(got.len(), 1, "one bad file must not blind the driver to every other collection");
        assert_eq!(got[0].npub, "npubA");
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

    // ── QURATOR-165: recovery of legacy fingerprint-keyed entries ─────────────────────────
    //
    // CLAUDE.md §5 scoping: a local on-disk cache touches no relay/transport/presence/DM/
    // discovery/crypto trigger, so these unit tests ARE the gate for the migration.

    /// The PRE-`6f0bdaa` filename shape: `entry_filename` hashed THREE parts —
    /// `for part in [npub, slug, fingerprint]`. TEST-ONLY by design; putting it in production
    /// code would be rebuilding the very scheme this migration exists to retire.
    fn legacy_entry_filename(npub: &str, slug: &str, fingerprint: &str) -> String {
        let mut h = Sha256::new();
        for part in [npub, slug, fingerprint] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
        format!("{}.json", hex::encode(&h.finalize()[..16]))
    }

    /// Write a `CacheEntry` under an explicit filename (canonical or legacy), bypassing `put` —
    /// how the tests plant the exact on-disk state either scheme produces.
    fn write_entry(
        dir: &Path,
        filename: &str,
        npub: &str,
        slug: &str,
        fingerprint: &str,
        envelope: &str,
        last_access: u64,
    ) -> PathBuf {
        let entry = CacheEntry {
            npub: npub.to_string(),
            slug: slug.to_string(),
            fingerprint: fingerprint.to_string(),
            envelope: envelope.to_string(),
            last_access,
        };
        let path = dir.join(filename);
        std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        path
    }

    /// Plant an entry under its LEGACY name — the state an install that ran a pre-`6f0bdaa`
    /// build wakes up with: a valid entry, filed under a name nothing looks up any more.
    fn plant_legacy(
        dir: &Path,
        npub: &str,
        slug: &str,
        fingerprint: &str,
        envelope: &str,
        last_access: u64,
    ) -> PathBuf {
        write_entry(dir, &legacy_entry_filename(npub, slug, fingerprint), npub, slug, fingerprint, envelope, last_access)
    }

    /// Plant an entry under its CANONICAL name, without `put` (so no migration or marker runs
    /// as a side effect — the pass under test starts from a known virgin directory).
    fn plant_canonical(
        dir: &Path,
        npub: &str,
        slug: &str,
        fingerprint: &str,
        envelope: &str,
        last_access: u64,
    ) -> PathBuf {
        write_entry(dir, &entry_filename(npub, slug), npub, slug, fingerprint, envelope, last_access)
    }

    /// The directory's entry files as sorted `(filename, bytes)` — canonical before/after shape.
    /// Non-`.json` files (the migration marker, `.tmp` strays) are EXCLUDED on purpose: only
    /// `scan`'s own admission rule decides what is an entry.
    fn entry_snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .map(|e| {
                let p = e.path();
                let name = p.file_name().unwrap().to_str().unwrap().to_string();
                (name, std::fs::read(&p).unwrap())
            })
            .collect();
        out.sort();
        out
    }

    /// A legacy file with no canonical sibling is RENAMED into place — the offline copy is
    /// RECOVERED, not merely cleaned up: `get` for its key serves its envelope afterwards.
    ///
    /// MUTATION (P-10) — resolved by containing item: in `migrate_legacy_dir`, replace
    /// `std::fs::rename(dir.join(newest), dir.join(&canonical))` with
    /// `Ok::<(), std::io::Error>(())` (no rename happens; `.is_err()` is then false and the
    /// deletes still run). No canonical file ever appears, so `get` misses and every assertion
    /// here reds.
    #[test]
    fn a_legacy_entry_with_no_canonical_sibling_is_recovered_by_rename() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = plant_legacy(dir.path(), "npubA", "films", "fp1", "ENV-LEGACY", 100);

        migrate_legacy_dir(dir.path());

        assert_eq!(get(dir.path(), "npubA", "films", "fp1", 200).as_deref(), Some("ENV-LEGACY"));
        assert!(!legacy.exists(), "renamed away, not copied");
        assert_eq!(entry_snapshot(dir.path()).len(), 1, "exactly the canonical file remains");
    }

    /// A legacy file for a key that ALREADY has a canonical entry is deleted, and the canonical
    /// entry is byte-identical afterwards — the canonical copy wins by existing, never by
    /// being overwritten.
    ///
    /// MUTATION (P-10): in `migrate_legacy_dir`, replace the guard
    /// `if !dir.join(&canonical).exists()` with `if true`, so the newest legacy file always
    /// renames OVER the canonical one. The reread differs from `canonical_bytes` and reds.
    #[test]
    fn a_legacy_entry_is_deleted_when_a_canonical_sibling_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp9", "ENV-CANON", 10, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        let canonical = dir.path().join(entry_filename("npubA", "films"));
        let canonical_bytes = std::fs::read(&canonical).unwrap();
        let legacy = plant_legacy(dir.path(), "npubA", "films", "fp1", "ENV-LEGACY", 999);

        migrate_legacy_dir(dir.path());

        assert!(!legacy.exists(), "the legacy copy is deleted");
        assert_eq!(
            std::fs::read(&canonical).unwrap(),
            canonical_bytes,
            "the canonical entry must be byte-identical — nothing overwrote it"
        );
    }

    /// Of two legacy files for the same `(npub, slug)`, the NEWER by `last_access` survives as
    /// the canonical entry and the older is deleted.
    ///
    /// MUTATION (P-10): in `migrate_legacy_dir`, change
    /// `group.sort_by_key(|(_, la)| std::cmp::Reverse(*la))` to
    /// `group.sort_by_key(|(_, la)| *la)` (oldest first). The OLDER copy is renamed into place,
    /// `get` for `fp-new` misses, and the assertions red.
    #[test]
    fn the_newest_legacy_copy_for_a_key_survives_as_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let older = plant_legacy(dir.path(), "npubA", "films", "fp-old", "ENV-OLD", 100);
        let newer = plant_legacy(dir.path(), "npubA", "films", "fp-new", "ENV-NEW", 200);

        migrate_legacy_dir(dir.path());

        assert_eq!(get(dir.path(), "npubA", "films", "fp-new", 300).as_deref(), Some("ENV-NEW"));
        assert!(get(dir.path(), "npubA", "films", "fp-old", 300).is_none(), "the older copy is gone");
        assert!(!older.exists(), "the older legacy filename is gone");
        assert!(!newer.exists(), "the newer legacy filename is gone");
        assert_eq!(entry_snapshot(dir.path()).len(), 1);
    }

    /// A canonical-only directory is left completely unchanged — no entry file added, removed,
    /// or renamed. The sentinel `.hb-migrated-v1` IS expected to appear; the assertions are
    /// deliberately on ENTRY files (`.json`) only, plus the marker's presence.
    ///
    /// MUTATION (P-10): in `migrate_legacy_dir`, invert the discriminator —
    /// `if name != entry_filename(&npub, &slug)` → `if name == entry_filename(&npub, &slug)`.
    /// Both canonical files land in groups whose canonical path exists, so the pass deletes
    /// them and the before/after snapshot reds. (Deleting the marker write in
    /// `migrate_legacy_entries` reds the marker assertion instead.)
    #[test]
    fn a_canonical_only_directory_is_left_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        plant_canonical(dir.path(), "npubA", "films", "fp1", "ENV-1", 10);
        plant_canonical(dir.path(), "npubB", "music", "fp2", "ENV-2", 20);
        let before = entry_snapshot(dir.path());

        migrate_legacy_entries(dir.path());

        assert_eq!(entry_snapshot(dir.path()), before, "no entry file added, removed, or renamed");
        assert!(dir.path().join(MIGRATION_MARKER).exists(), "a completed pass marks the directory");
    }

    /// The migration is idempotent — a second `migrate_legacy_dir` over an already-migrated
    /// directory changes nothing.
    ///
    /// Two legacy files, deliberately: with only one, the pass renames it and the delete loop
    /// no-ops on a path that no longer exists, so the whole delete arm goes unexercised and NO
    /// mutation of it can red this test. The older file is what forces that arm to run.
    ///
    /// MUTATION (P-10), verified red 2026-09-01 — replace the delete loop's body in
    /// `migrate_legacy_dir` with
    /// `let _ = std::fs::rename(dir.join(&name), dir.join(format!("{name}x.json")));`. The first
    /// pass then leaves the older entry as a `.json` residue that `scan` still reads and
    /// `entry_snapshot` still sees; the second pass renames it again, so the snapshots differ.
    ///
    /// ⚠ ANCHOR BY CONTAINING FUNCTION, not by the line. `let _ = std::fs::remove_file(dir.join(
    /// name)); // best-effort` appears TWICE — the twin is `put`'s eviction loop, and its comment
    /// merely continues (`; a stuck file just lingers`), so the shorter text is a prefix of both.
    /// A mutation keyed on that line alone silently matches nothing, leaves the file PRISTINE,
    /// and the run comes back green — a phantom proof. Anchor on the `for (name, _) in group {`
    /// header, which is unique to this function.
    ///
    /// ⚠ Three candidates were tried and rejected, each for a different reason worth keeping:
    /// `rename` → `copy` converges to the same end state (the delete removes the source either
    /// way); a `.bak` residue is invisible because `entry_snapshot` filters on
    /// `extension == "json"`; and with only ONE legacy file planted the delete arm never runs at
    /// all (the sole entry is renamed away, so its own delete no-ops). Hence the second
    /// `plant_legacy` below — it is what gives this test any teeth.
    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        plant_legacy(dir.path(), "npubA", "films", "fp1", "ENV-1", 100);
        plant_legacy(dir.path(), "npubA", "films", "fp2", "ENV-2", 200); // newer; fp1 must be deleted

        migrate_legacy_dir(dir.path());
        assert_eq!(get(dir.path(), "npubA", "films", "fp2", 250).as_deref(), Some("ENV-2"));
        // Snapshot AFTER the read: `get` bumps `last_access` and rewrites the file, and that
        // legitimate rewrite must not be mistaken for a second-pass change below.
        let first = entry_snapshot(dir.path());

        migrate_legacy_dir(dir.path());
        assert_eq!(entry_snapshot(dir.path()), first, "the second pass changes nothing");
    }

    /// The ON-DISK marker — not just the in-process guard — suppresses a later pass: after a
    /// completed migration, a newly planted legacy file is left alone by a subsequent
    /// `migrate_legacy_entries` on fresh guard state (the directory dropped from `MIGRATED`,
    /// i.e. exactly what the next process launch sees). This is the test that pins the
    /// sentinel; without it the marker is decorative and every launch still pays a full `scan`.
    ///
    /// MUTATION (P-10): delete the marker short-circuit at the top of `migrate_legacy_entries`
    /// (`if dir.join(MIGRATION_MARKER).exists() { return; }`). The second call then falls
    /// through the cleared in-process guard, runs the pass, migrates the planted file, and both
    /// assertions red.
    #[test]
    fn the_on_disk_marker_suppresses_a_second_pass_on_fresh_guard_state() {
        let dir = tempfile::tempdir().unwrap();
        plant_legacy(dir.path(), "npubA", "films", "fp1", "ENV-1", 100);
        migrate_legacy_entries(dir.path());
        assert_eq!(get(dir.path(), "npubA", "films", "fp1", 150).as_deref(), Some("ENV-1"));

        // Fresh "process": this directory is no longer in the in-process guard.
        MIGRATED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(dir.path());

        let planted = plant_legacy(dir.path(), "npubB", "music", "fp2", "ENV-2", 200);
        migrate_legacy_entries(dir.path());
        assert!(planted.exists(), "the marker must suppress the pass — no rename happened");
        assert!(get(dir.path(), "npubB", "music", "fp2", 250).is_none(), "no canonical file was created");
    }

    // QURATOR-176 — the global, all-or-nothing clear.
    //
    // MUTATION PROOF (P-10, for the orchestrator to apply and revert): replace the
    // `std::fs::remove_dir_all(dir)` expression in `clear_all`'s body with `Ok(())` (a no-op body).
    // `clear_all_removes_every_entry_across_npubs` must go RED (the entries survive, both `get`s
    // return `Some`); `clear_all_on_an_absent_cache_is_success` stays green under that mutation —
    // its discriminator is the NotFound arm, reds under the mutation named beside it.
    #[test]
    fn clear_all_removes_every_entry_across_npubs() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "npubA", "films", "fp1", "ENV-A", 100, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        put(dir.path(), "npubB", "music", "fp2", "ENV-B", 100, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert!(get(dir.path(), "npubA", "films", "fp1", 200).is_some(), "precondition: entries are cached");

        clear_all(dir.path()).unwrap();

        assert!(get(dir.path(), "npubA", "films", "fp1", 200).is_none(), "npubA entry must be gone");
        assert!(get(dir.path(), "npubB", "music", "fp2", 200).is_none(), "npubB entry must be gone");
        assert!(!dir.path().exists(), "the cache directory itself is removed, not emptied");

        // The cache must keep working afterwards: `put` recreates the directory on demand, so a
        // clear cannot brick later browsing.
        put(dir.path(), "npubC", "books", "fp3", "ENV-C", 300, DEFAULT_MANIFEST_CACHE_BYTES).unwrap();
        assert_eq!(get(dir.path(), "npubC", "books", "fp3", 400).as_deref(), Some("ENV-C"));
    }

    // MUTATION PROOF (P-10): in `clear_all`, change the NotFound arm
    // `Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(())` to return `Err(e)` instead.
    // This test must go RED (removing an absent dir returns NotFound, now propagated); the
    // removes-every-entry test above stays green under this mutation.
    #[test]
    fn clear_all_on_an_absent_cache_is_success() {
        let base = tempfile::tempdir().unwrap();
        // The cache dir has never been created — `put` is what creates it, and it was never called.
        clear_all(&cache_dir(base.path())).unwrap();
        // Idempotent: a second clear of the now-still-absent directory is also success.
        clear_all(&cache_dir(base.path())).unwrap();
    }
}
