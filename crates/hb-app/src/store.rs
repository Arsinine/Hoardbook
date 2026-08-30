//! Typed read/write helpers for the on-disk data directory.
//!
//! Layout (v0.9 Nostr model):
//! ```text
//! <app_data_dir>/
//!   identity/
//!     identity.json           StoredIdentity (nsec + account browse-key + transport secret)
//!   collections/
//!     <slug>.draft.json       Collection (the scanned tree + metadata)
//!   published/
//!     <slug>.json             a published listing's nostr Event (opaque JSON; enables NIP-09)
//!     profile.json            the published teaser's nostr Event (opaque JSON)
//!   contacts/
//!     <npub_hash>.json        CachedPeer
//!   sharing/<slug>.json       ShareSettings
//!   groups.json · watches.json · settings.json
//! ```
//!
//! The published-event JSON is treated as an opaque string here — the command layer (which
//! has `nostr`) parses it. This keeps the store free of a `nostr` dependency.

use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Settings — persisted user preferences
// ---------------------------------------------------------------------------

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Configured Nostr relays (seed + write). Empty = the app has no relays yet.
    pub relay_urls: Vec<String>,
    /// When false, only DMs from saved contacts are surfaced.
    #[serde(default = "default_true")]
    pub allow_dms: bool,
    /// The one-time pre-first-download IP-exposure notice has been acknowledged (spec §Onboarding).
    /// Shown iff this is false; acknowledging persists it.
    #[serde(default)]
    pub privacy_notice_acknowledged: bool,
    /// The app version last seen running — drives the "now on vX.Y" visible-after notice. The
    /// writer normalizes it to the running-version string, so comparison is exact-string equality.
    #[serde(default)]
    pub last_seen_version: String,
    /// M9: auto-update a published listing when its source tree changes (filesystem-watch). On by
    /// default; off = today's manual-only "Regenerate" behaviour (Decision #17).
    #[serde(default = "default_true")]
    pub snapshot_auto_update: bool,
    /// M9: an opt-in low-frequency reconcile poll for users who edit their shares from another host
    /// (SMB server-side edits a local watch can't see). Off by default — most users don't need it.
    #[serde(default)]
    pub snapshot_reconcile_poll: bool,
    /// M9: show the optional "🟢 N online" indicator (relay-derived; no telemetry). On by default;
    /// off hides the chip.
    #[serde(default = "default_true")]
    pub show_online_count: bool,
    /// devtest #5: opt into tag/content-type discoverability — when true, the published teaser's
    /// `tags`/`content_types` also surface as `t` hashtags (relay-searchable). **Default false**: a
    /// pre-existing `settings.json` with no such key loads as `false` (bool serde default), which is
    /// the intended silent de-list — no migration. npub lookup and share-code browse are unaffected
    /// either way (they read the teaser body, not the hashtags).
    #[serde(default)]
    pub discoverable: bool,
    /// M16 W3 — the owner's dedicated **big relay** for the full-manifest (Layer 3) path. When a
    /// Public collection is too large to publish whole (truncated to a paywall teaser), the full
    /// split family is *also* published here (only), so a browser holding the share code can fetch
    /// the complete listing. **Empty = the feature is off** (only the truncated teaser is published,
    /// today's behaviour). A pre-M16 `settings.json` with no such key loads empty (serde default) —
    /// no migration.
    #[serde(default)]
    pub big_relay_url: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            relay_urls: Vec::new(),
            allow_dms: true,
            privacy_notice_acknowledged: false,
            last_seen_version: String::new(),
            snapshot_auto_update: true,
            snapshot_reconcile_poll: false,
            show_online_count: true,
            discoverable: false,
            big_relay_url: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// ShareSettings — per-collection persisted on-disk root
// ---------------------------------------------------------------------------

/// Per-collection persisted state. The transfer-era fields (`enabled`, `allowed_paths`,
/// `speed_cap_kbps`, `download_limit`, `require_follow`) were removed with the download UI —
/// Hoardbook moves no *collection files* (INV-4′; M18's plane carries manifests only, so none of
/// these came back). Only `root_path` survives: the collection's on-disk root,
/// persisted so the snapshot re-scan can find the tree again. (Overlaps `ScanSpec.root`; kept
/// separate for now — de-dup is a later cleanup.) Old JSON with the removed fields still loads
/// (serde ignores unknown fields).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShareSettings {
    pub root_path: Option<String>,
}

// ---------------------------------------------------------------------------
// ScanSpec — the parameters a collection was scanned with (M9)
// ---------------------------------------------------------------------------

/// The exact scan parameters a collection draft was built from, persisted so the snapshot watch can
/// **faithfully re-scan** the same tree (same root, same checked folders, same exclusions) when the
/// source changes. Without this the watch couldn't reproduce the user's folder-tree selection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanSpec {
    /// Absolute path of the collection root on disk.
    pub root: String,
    /// Relative "/"-separated directory paths the user checked in the folder-tree picker (M8).
    #[serde(default)]
    pub include: Vec<String>,
    /// Exclude globs.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Total bytes on disk from the last scan. Lives here (a per-slug local sidecar, never
    /// published) rather than on `Collection` so the UI can show an aggregate "Total Size" while the
    /// published listing still **omits** exact bytes (the hb-core `Collection` privacy invariant —
    /// devtest 2026-06-25 #5). `#[serde(default)]` so a pre-existing spec without it loads as 0.
    #[serde(default)]
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// StoredIdentity — the three keys, on disk (v0.9 Nostr model)
// ---------------------------------------------------------------------------

/// On-disk identity: the irreplaceable secp256k1 secret (`nsec`), the account browse-key (the
/// "club pass" carried in the `hbk` share code), and the regenerable transport secret (M18 W2 —
/// the manifest plane's node key). On Windows this whole struct is DPAPI-encrypted at rest; on
/// Linux/macOS it is a 0600 plaintext file until the Phase-2 keyring lands. `ZeroizeOnDrop`
/// (audit I-11): every secret hex is wiped from memory whenever a loaded/saved/backup copy drops.
#[derive(Clone, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct StoredIdentity {
    pub version: u8,
    /// secp256k1 secret key as bech32 `nsec…` — the one irreplaceable secret.
    pub nsec: String,
    /// Hex-encoded 32-byte account browse-key.
    pub browse_key_hex: String,
    /// Hex-encoded 32-byte transport secret (M18 W2). Deliberately **not** named for iroh: the
    /// plane's choice of transport is W1's business, not the file format's. `serde(default)` so a
    /// 2-key record from v0.9.6–v0.12.x loads; [`DataStore::load_identity`] mints and persists one
    /// when it is empty, so the migration needs no user action.
    #[serde(default)]
    pub transport_secret_hex: String,
}

impl std::fmt::Debug for StoredIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredIdentity")
            .field("version", &self.version)
            .field("nsec", &"[REDACTED]")
            .field("browse_key_hex", &"[REDACTED]")
            .field("transport_secret_hex", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

/// The sibling temp path an atomic write stages into: `<name>.tmp.<pid>.<seq>`. Same directory as
/// the target so the rename never crosses a filesystem. The per-call sequence keeps same-process
/// concurrent writers to one target from sharing a stage file (chorus M13 #1: with a shared name,
/// writer A could rename writer B's staged bytes into place as its own); the pid isolates
/// processes. A stage file orphaned by a crash is inert — never read, removed by wipe().
fn tmp_path(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(format!(".tmp.{}.{seq}", std::process::id()));
    path.with_file_name(name)
}

/// Crash-safe write (audit I-11): stage the bytes in a temp file beside the target, then rename
/// over it — a crash mid-write leaves the old content intact, never a truncated/half-written file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(path);
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

// `pub(crate)`: the M13 Part B quarantine store (`dm_quarantine.rs`) is a sibling module that mirrors
// this exact Group/Watch/StoredTopic persistence pattern, so it reuses these helpers (atomicity for
// free) rather than re-implementing them.
pub(crate) fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    write_atomic(path, json.as_bytes())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Like read_json but returns Ok(None) instead of propagating a parse error.
/// Used for settings and contacts so that a version mismatch (new app loading
/// old config) silently falls back to defaults rather than crashing.
pub(crate) fn read_json_lenient<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            tracing::warn!(
                "Config file {:?} could not be parsed (version mismatch?): {e}. \
                 Falling back to defaults.",
                path
            );
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// DataStore
// ---------------------------------------------------------------------------

/// Result of [`DataStore::save_published_guarded`]: whether the marker was written, or a concurrent
/// unpublish bumped the revocation generation and the save was deliberately skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishedSave {
    Saved,
    Revoked,
}

#[derive(Clone)]
pub struct DataStore {
    pub(crate) base: PathBuf,
    /// Per-key locks serializing a published-marker check-and-write against [`Self::delete_published`]'s
    /// remove-and-bump, so the two cannot interleave between the generation re-read and the marker
    /// write (CWE-367). Keyed by slug (or "profile"); shared across clones; single-instance app.
    published_locks: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
    >,
}

impl DataStore {
    pub fn new(base: PathBuf) -> Self {
        Self {
            base,
            published_locks: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    // -- Paths ---------------------------------------------------------------

    /// The root `~/.hoardbook` directory the backup archives.
    pub fn base_dir(&self) -> &Path {
        &self.base
    }

    pub fn identity_path(&self) -> PathBuf {
        // .bin on Windows (DPAPI-encrypted opaque blob), .json on Linux (plain chmod 600).
        #[cfg(target_os = "windows")]
        let filename = "identity.bin";
        #[cfg(not(target_os = "windows"))]
        let filename = "identity.json";
        self.base.join("identity").join(filename)
    }

    pub fn collection_draft_path(&self, slug: &str) -> PathBuf {
        self.base.join("collections").join(format!("{slug}.draft.json"))
    }

    pub fn profile_draft_path(&self) -> PathBuf {
        self.base.join("identity").join("profile.draft.json")
    }

    /// Path of a published nostr Event (listing or teaser), stored to enable NIP-09 unpublish.
    pub fn published_path(&self, key: &str) -> PathBuf {
        self.base.join("published").join(format!("{key}.json"))
    }

    pub fn contact_path(&self, npub_hash: &str) -> PathBuf {
        self.base.join("contacts").join(format!("{npub_hash}.json"))
    }

    pub fn settings_path(&self) -> PathBuf {
        self.base.join("settings.json")
    }

    /// The M16 W4 manifest LRU cache directory (`<base>/manifests/`). Covered by `wipe` for free.
    pub fn manifest_cache_dir(&self) -> PathBuf {
        crate::manifest_cache::cache_dir(&self.base)
    }

    // -- Identity ------------------------------------------------------------

    pub fn save_identity(&self, id: &StoredIdentity) -> Result<()> {
        let path = self.identity_path();
        if let Some(parent) = path.parent() {
            // Mode 0700 on Linux so the identity dir is accessible only to the owner.
            #[cfg(not(target_os = "windows"))]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .ok(); // already-exists is fine
            }
            #[cfg(target_os = "windows")]
            {
                std::fs::create_dir_all(parent)?;
            }
        }

        let json = serde_json::to_string_pretty(id)?;

        #[cfg(target_os = "windows")]
        {
            let encrypted = hb_dpapi::encrypt(json.as_bytes())
                .context("DPAPI encryption failed")?;
            std::fs::write(&path, encrypted)?;
        }

        #[cfg(not(target_os = "windows"))]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let first_write = !path.exists();
            // Create the file *already* at 0600 (the `.mode()` applies at creation) so the nsec is
            // never briefly world-readable in the window a bare `write` + follow-up `chmod` leaves
            // (convergent chorus finding: Codex/Gemini/Kimi). `.mode()` is ignored for an existing
            // file, so re-assert 0600 on the open fd to also cover a pre-existing file left with
            // looser perms by an older build. The parent dir is 0700, so a symlink-swap pre-attack
            // on this path is already out of reach (no O_NOFOLLOW needed).
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            f.write_all(json.as_bytes())?;
            if first_write {
                tracing::warn!(
                    "Private key stored as a plain file at {:?}. Keep your home directory secure.",
                    path
                );
            }
        }

        Ok(())
    }

    pub fn load_identity(&self) -> Result<Option<StoredIdentity>> {
        let path = self.identity_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).context("reading identity file")?;

        // A 0-byte identity file is a failed/partial write (e.g. the DPAPI CRED_SYNC bug wrote
        // an empty blob). Treat it as "absent" so the app regenerates an identity instead of
        // dead-ending forever on an "identity unreadable" recovery screen.
        if bytes.is_empty() {
            return Ok(None);
        }

        #[cfg(target_os = "windows")]
        let json_bytes = hb_dpapi::decrypt(&bytes).context("DPAPI decryption failed")?;

        #[cfg(not(target_os = "windows"))]
        let json_bytes = bytes;

        let mut stored: StoredIdentity =
            serde_json::from_slice(&json_bytes).context("parsing identity")?;

        // M18 W2 migration — a 2-key record (v0.9.6 … v0.12.x) has no transport secret. Mint one
        // here, the single choke point every load path goes through, so the upgrade needs no user
        // action and the node key is STABLE across restarts (minting per-load would hand a peer a
        // different node identity every launch).
        //
        // This ADDS a missing field; it never rewrites one that is present — a background actor
        // must not silently destroy stored data (the v0.12.6 `path_alias` lesson). And a failed
        // write must not fail the load: an identity that reads fine on a read-only data dir keeps
        // working, it just re-mints next launch.
        if stored.transport_secret_hex.is_empty() {
            stored.transport_secret_hex = hex::encode(rand::random::<[u8; 32]>());
            if let Err(e) = self.save_identity(&stored) {
                tracing::warn!("could not persist the minted transport key: {e:#}");
            }
        }

        Ok(Some(stored))
    }

    // -- Profile draft -------------------------------------------------------

    pub fn save_profile_draft(&self, profile: &Profile) -> Result<()> {
        write_json(&self.profile_draft_path(), profile).context("saving profile draft")
    }

    pub fn load_profile_draft(&self) -> Result<Option<Profile>> {
        read_json_lenient(&self.profile_draft_path()).context("loading profile draft")
    }

    // -- Collections ---------------------------------------------------------

    pub fn save_collection_draft(&self, collection: &Collection) -> Result<()> {
        write_json(&self.collection_draft_path(&collection.slug), collection)
            .context("saving collection draft")
    }

    /// Load a draft collection by slug.
    ///
    /// Deliberately does **not** clamp. An earlier version did, to bound legacy metadata from a
    /// restored backup, and it was wrong twice over: the background watcher in `watch.rs` loads and
    /// re-saves on any source-tree change, so the truncation became a silent, permanent edit to a
    /// description the user never touched; and truncating `path_alias` here re-addressed the
    /// collection on its next rescan (see `Collection::clamp_metadata`). The publish budget is
    /// enforced on the outgoing copy instead — `collection_to_listing_json`.
    pub fn load_collection_draft(&self, slug: &str) -> Result<Option<Collection>> {
        read_json(&self.collection_draft_path(slug)).context("loading collection draft")
    }

    /// List every collection draft's slug.
    pub fn list_collection_slugs(&self) -> Result<Vec<String>> {
        let dir = self.base.join("collections");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut slugs = vec![];
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if path.extension().map(|e| e == "json").unwrap_or(false) && stem.ends_with(".draft") {
                slugs.push(stem.trim_end_matches(".draft").to_string());
            }
        }
        Ok(slugs)
    }

    pub fn share_settings_path(&self, slug: &str) -> PathBuf {
        self.base.join("sharing").join(format!("{slug}.json"))
    }

    pub fn delete_collection(&self, slug: &str) -> Result<()> {
        for path in &[
            self.collection_draft_path(slug),
            self.published_path(slug),
            self.share_settings_path(slug),
            self.scan_spec_path(slug),
            self.snapshot_fingerprint_path(slug),
        ] {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    // -- Scan spec (M9 — faithful re-scan for the snapshot watch) ------------

    pub fn scan_spec_path(&self, slug: &str) -> PathBuf {
        self.base.join("collections").join(format!("{slug}.scan.json"))
    }

    pub fn save_scan_spec(&self, slug: &str, spec: &ScanSpec) -> Result<()> {
        write_json(&self.scan_spec_path(slug), spec).context("saving scan spec")
    }

    pub fn load_scan_spec(&self, slug: &str) -> Result<Option<ScanSpec>> {
        read_json_lenient(&self.scan_spec_path(slug)).context("loading scan spec")
    }

    // -- Snapshot fingerprint (M9 — republish storm guard) -------------------

    /// Path of the last-published snapshot fingerprint (the storm-guard baseline). Lives beside the
    /// published-event marker; the published listing is encrypted with a random nonce, so its
    /// ciphertext can't be diffed — the plaintext-tree fingerprint is what the watch compares.
    pub fn snapshot_fingerprint_path(&self, slug: &str) -> PathBuf {
        self.base.join("published").join(format!("{slug}.fp.json"))
    }

    pub fn save_snapshot_fingerprint(
        &self,
        slug: &str,
        fp: &hb_core::SnapshotFingerprint,
    ) -> Result<()> {
        write_json(&self.snapshot_fingerprint_path(slug), fp).context("saving snapshot fingerprint")
    }

    pub fn load_snapshot_fingerprint(&self, slug: &str) -> Result<Option<hb_core::SnapshotFingerprint>> {
        read_json_lenient(&self.snapshot_fingerprint_path(slug)).context("loading snapshot fingerprint")
    }

    /// Slugs of every **published** collection (those with a published-event marker) — the scope the
    /// snapshot watch and the launch re-scan operate over (public listings only; M9).
    pub fn list_published_slugs(&self) -> Result<Vec<String>> {
        Ok(self
            .list_collection_slugs()?
            .into_iter()
            .filter(|slug| self.is_published(slug))
            .collect())
    }

    // -- Published events (NIP-09 enablement) --------------------------------

    /// Persist a published nostr Event (opaque JSON) under `key` (a slug, or "profile").
    ///
    /// This is the UNGUARDED primitive: every production publish now goes through
    /// [`Self::save_published_guarded`], so a concurrent unpublish cannot be silently undone
    /// (CWE-367). `save_published` remains only as test setup, hence the `#[allow(dead_code)]`
    /// outside `test` (the same shape as `logging.rs`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn save_published(&self, key: &str, event_json: &str) -> Result<()> {
        write_atomic(&self.published_path(key), event_json.as_bytes())
            .context("saving published event")
    }

    /// Load a published event's JSON, if it exists.
    pub fn load_published(&self, key: &str) -> Result<Option<String>> {
        let path = self.published_path(key);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(&path).context("loading published event")?))
    }

    pub fn delete_published(&self, key: &str) -> Result<()> {
        let lock = self.published_key_lock(key);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.published_path(key);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.bump_published_generation(key)
    }

    /// Persist a published marker ONLY if `key` has not been unpublished since `expected_generation`
    /// was read (CWE-367). The generation re-read + marker write run under the per-key lock, so an
    /// interleaving [`Self::delete_published`] (which removes the marker and bumps the generation
    /// under the same lock) cannot slip between the check and the write. `Revoked` means the save was
    /// deliberately skipped — the caller must report it, never treat it as success.
    pub fn save_published_guarded(
        &self,
        key: &str,
        event_json: &str,
        expected_generation: u64,
    ) -> Result<PublishedSave> {
        let lock = self.published_key_lock(key);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        if self.published_generation(key) != expected_generation {
            return Ok(PublishedSave::Revoked);
        }
        write_atomic(&self.published_path(key), event_json.as_bytes())
            .context("saving published event")?;
        Ok(PublishedSave::Saved)
    }

    /// The revocation generation for `key` — a small counter bumped by [`Self::delete_published`] so
    /// a publish already in flight can detect a concurrent unpublish. Missing or unparsable files read
    /// as `0` (the counter self-heals on the next delete, which overwrites it with a fresh value).
    pub fn published_generation(&self, key: &str) -> u64 {
        match std::fs::read_to_string(self.published_generation_path(key)) {
            Ok(s) => s.trim().parse::<u64>().unwrap_or(0),
            Err(_) => 0,
        }
    }

    pub fn is_published(&self, key: &str) -> bool {
        self.published_path(key).exists()
    }

    /// Path of the revocation-generation counter, beside the marker it guards.
    fn published_generation_path(&self, key: &str) -> PathBuf {
        self.base.join("published").join(format!("{key}.gen"))
    }

    /// The per-key lock guarding that key's marker check-and-write vs delete-and-bump. The `Arc` must
    /// be held by the caller for as long as the returned guard is alive (it is a local in each use).
    fn published_key_lock(&self, key: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
        let mut map = self
            .published_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.entry(key.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
    }

    /// Bump `key`'s revocation generation by one. Caller holds the per-key lock.
    fn bump_published_generation(&self, key: &str) -> Result<()> {
        let next = self.published_generation(key) + 1;
        write_atomic(
            &self.published_generation_path(key),
            next.to_string().as_bytes(),
        )
        .context("bumping published generation")
    }

    // -- Settings ------------------------------------------------------------

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        write_json(&self.settings_path(), settings).context("saving settings")
    }

    pub fn load_settings(&self) -> Result<Option<Settings>> {
        read_json_lenient(&self.settings_path()).context("loading settings")
    }

    // -- Share settings ------------------------------------------------------

    pub fn save_share_settings(&self, slug: &str, settings: &ShareSettings) -> Result<()> {
        write_json(&self.share_settings_path(slug), settings).context("saving share settings")
    }

    pub fn load_share_settings(&self, slug: &str) -> Result<Option<ShareSettings>> {
        read_json(&self.share_settings_path(slug)).context("loading share settings")
    }

    // -- Wipe ----------------------------------------------------------------

    /// Delete all persisted data. In-memory state must be cleared by the caller.
    ///
    /// Removes **every** entry under the base dir rather than an enumerated file list (audit
    /// I-11: the old list had drifted from what the store writes). The base dir is app-owned —
    /// restore already treats *any* entry as "occupied" — so a future store addition is wiped
    /// automatically instead of surviving as an orphan that then blocks restore.
    pub fn wipe(&self) -> Result<()> {
        let entries = match std::fs::read_dir(&self.base) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    // -- Contacts ------------------------------------------------------------

    pub fn load_contact(&self, npub_hash: &str) -> Result<Option<CachedPeer>> {
        read_json(&self.contact_path(npub_hash)).context("loading contact")
    }

    /// Persist a contact. **`last_presence` is owned by the online poll and is never cleared here**
    /// (W5 review): a writer that rebuilds a `CachedPeer` from a relay resolve — `refresh_contact`,
    /// `follow`, `paste_key` — carries no presence stamp, and Contacts refreshes every contact on
    /// mount, so without this the durable last-seen was wiped seconds after it was written and the
    /// row fell back to "unknown" after every restart. An incoming `Some` still wins (the poll can
    /// always move the stamp forward); only `None` defers to what is already on disk.
    pub fn save_contact(&self, npub_hash: &str, peer: &CachedPeer) -> Result<()> {
        let path = self.contact_path(npub_hash);
        if peer.last_presence.is_none() {
            if let Ok(Some(prev)) = read_json::<CachedPeer>(&path) {
                if prev.last_presence.is_some() {
                    let merged = CachedPeer { last_presence: prev.last_presence, ..peer.clone() };
                    return write_json(&path, &merged).context("saving contact");
                }
            }
        }
        write_json(&path, peer).context("saving contact")
    }

    pub fn delete_contact(&self, npub_hash: &str) -> Result<()> {
        let path = self.contact_path(npub_hash);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn list_contacts(&self) -> Result<Vec<CachedPeer>> {
        let dir = self.base.join("contacts");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut results = vec![];
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(Some(mut peer)) = read_json_lenient::<CachedPeer>(&path) {
                    backfill_fingerprint(&mut peer);
                    results.push(peer);
                }
            }
        }
        Ok(results)
    }
}

/// Derive the §7 word+colour fingerprint for a contact stored before it existed.
///
/// The fingerprint is a **pure function of the npub** — `resolve_peer` already says so — so a
/// contact that predates the field is not missing data, it is missing a computation nobody ran.
/// Until this existed, `list_contacts` returned it as `None` forever and the M21 W4 contact card
/// silently fell back to its no-fingerprint rendering: no avatar ring, no word row. Every contact
/// added before that release looked like the pre-redesign card, and the only escape was refreshing
/// each one by hand — which is exactly what the owner reported as "the uplift was never done".
///
/// Read-time only: this does NOT rewrite the stored file. Nothing is migrated on disk, so the
/// operation is idempotent, costs one hash, and cannot corrupt a contact record. An npub that
/// fails to parse is left `None` rather than guessed at.
fn backfill_fingerprint(peer: &mut CachedPeer) {
    if peer.fingerprint.is_some() {
        return;
    }
    peer.fingerprint =
        hb_core::identity::parse_npub(&peer.npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk));
}

// ---------------------------------------------------------------------------
// CachedPeer — one file per followed peer in contacts/
// ---------------------------------------------------------------------------

use crate::commands::browse::PeerCollection;
use hb_core::types::{Collection, Profile};

/// How a contact entered your local contact list (M11). **`Manual`** = you added them by hand (a
/// share code / paste-key). **`Topic`** = auto-added because you share a §11 Topic — a distinct badge,
/// so topic-sourced contacts are always distinguishable from people you added deliberately. A topic
/// contact still has **no browse-key** (joining a Topic unlocks no listings — INV-2); browsing them
/// needs their share code, exchanged one-to-one as normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ContactSource {
    /// Added by hand. The default, so a pre-M11 contact (no `source` field) loads as `Manual` — a
    /// topic badge is never silently applied on upgrade.
    #[default]
    Manual,
    /// Auto-added via a shared Topic.
    Topic,
}

/// QURATOR-134 — the UI-facing projection of `hb_net::ListingsState`, carried on
/// [`CachedPeer::listings_state`]. Serialized as its bare variant name (`"Fetched"` /
/// `"Sealed"` / `"FetchFailed"`); `FetchFailed`'s diagnostic reason is dropped at this boundary
/// (the UI only needs to know the load failed, not why — it renders error + Retry).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ListingsStatus {
    /// The enumeration completed and the peer authored no listing events — an honest empty.
    #[default]
    Fetched,
    /// The peer authored listings but none decryptable for us — the genuine 🔒 locked case.
    Sealed,
    /// The author-wide listing enumeration itself failed — error + Retry, never a confident
    /// negative on data that never arrived.
    FetchFailed,
}

impl From<hb_net::ListingsState> for ListingsStatus {
    fn from(s: hb_net::ListingsState) -> Self {
        match s {
            hb_net::ListingsState::Fetched => ListingsStatus::Fetched,
            hb_net::ListingsState::Sealed => ListingsStatus::Sealed,
            // The reason string is diagnostic-only here (logged upstream in hb-net).
            hb_net::ListingsState::FetchFailed(_) => ListingsStatus::FetchFailed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPeer {
    /// The peer's Nostr identity (bech32 `npub`) — the stable key the follower-gate keys on.
    pub npub: String,
    /// How this contact was added — `Manual` (by hand) or `Topic` (auto-added via a shared Topic).
    /// `#[serde(default)]` ⇒ a pre-M11 contact loads as `Manual` (never silently flagged a topic
    /// contact on upgrade).
    #[serde(default)]
    pub source: ContactSource,
    /// The peer's account browse-key (hex), captured from a full `hbk` share code — lets us
    /// decrypt their listings + unseal their presence address. `None` for a follow-only contact.
    #[serde(default)]
    pub browse_key_hex: Option<String>,
    /// Local impersonation-resistant petname (bound to `npub`, never shared).
    #[serde(default)]
    pub petname: Option<String>,
    pub profile: Option<Profile>,
    /// The peer's collections as browsed with a full share code (M13 HANDOVER gap #5): each carries
    /// the `Collection` plus the K-of-N part counts, when known. `#[serde(flatten)]` +
    /// `#[serde(default)]` on `PeerCollection`'s parts fields keep a pre-M13 cache (plain `Collection`
    /// objects, no parts info) loading with `parts_total`/`parts_present` as `None`.
    pub collections: Vec<PeerCollection>,
    /// QURATOR-134 — WHY `collections` looks the way it does for a KEYLESS contact, threaded from
    /// `hb_net::ListingsState` (the one implementation; the UI must never re-derive it from
    /// `collections.is_empty()`): `Fetched` = the enumeration completed and the peer authored no
    /// listing events (an honest empty — "No public collections"); `Sealed` = they authored
    /// listings but none decryptable for us (the genuine 🔒 locked case); `FetchFailed` = the
    /// enumeration itself failed (error + Retry, never a confident negative). Only meaningful
    /// when `browse_key_hex` is `None` — a keyed contact's `collections` is authoritative.
    /// `#[serde(default)]` ⇒ a pre-QURATOR-134 cached contact loads as `Fetched` (the honest
    /// empty — the least-wrong reading of data the old code never classified).
    #[serde(default)]
    pub listings_state: ListingsStatus,
    pub online: bool,
    pub last_fetched: chrono::DateTime<chrono::Utc>,
    /// **When we last saw this peer's presence beacon** — real last-seen, as opposed to
    /// `last_fetched`, which is when *we* last polled (M17 W5: the contact row used to render
    /// `last_fetched` as "seen {t}" and so said "just now" about someone gone for a week).
    /// Stamped by the 60s online poll from the fresh-presence map it already fetches; `None` means
    /// "we have never observed a beacon", which the UI renders as unknown — never "never".
    /// `#[serde(default)]` ⇒ a pre-W5 stored contact loads as `None`.
    #[serde(default)]
    pub last_presence: Option<chrono::DateTime<chrono::Utc>>,
    /// User-defined tags for organizing contacts locally. Never shared.
    #[serde(default)]
    pub local_tags: Vec<String>,
    /// The §7 word+color impersonation-fingerprint, derived deterministically from `npub`. Populated
    /// when a peer is resolved (lookup/follow); `#[serde(default)]` ⇒ a pre-fingerprint stored contact
    /// loads as `None` until its next refresh. The UI renders it verbatim (never re-derives — M3 #7).
    #[serde(default)]
    pub fingerprint: Option<hb_core::fingerprint::Fingerprint>,
}

impl CachedPeer {
    pub fn pubkey_hash(npub: &str) -> String {
        // First 16 bytes (32 hex chars) of SHA256 of the npub as a stable filename (audit I-11:
        // widened from 8 bytes; pre-launch, so old cache filenames simply orphan).
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(npub.as_bytes());
        hex::encode(&hash[..16])
    }
}

// ---------------------------------------------------------------------------
// Group — local-only contact grouping (not signed, not shared)
// ---------------------------------------------------------------------------

fn default_group_modified_at() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    /// npubs of the contacts in this group.
    pub pubkeys: Vec<String>,
    /// Last modification time — used to order groups most-recently-modified first.
    #[serde(default = "default_group_modified_at")]
    pub modified_at: chrono::DateTime<chrono::Utc>,
    /// Optional user-chosen colour (CSS hex, e.g. `"#ff00aa"`) for the group chip in the UI (M13
    /// W5, item 3). `#[serde(default)]` ⇒ a pre-existing group with no `color` field loads as
    /// `None` (no colour). Local-only, never shared.
    #[serde(default)]
    pub color: Option<String>,
}

impl DataStore {
    pub fn groups_path(&self) -> PathBuf {
        self.base.join("groups.json")
    }

    pub fn load_groups(&self) -> Result<Vec<Group>> {
        let mut groups = read_json_lenient::<Vec<Group>>(&self.groups_path())
            .context("loading groups")?
            .unwrap_or_default();
        groups.sort_by_key(|g| std::cmp::Reverse(g.modified_at));
        Ok(groups)
    }

    pub fn save_groups(&self, groups: &[Group]) -> Result<()> {
        write_json(&self.groups_path(), groups).context("saving groups")
    }

    // M21 W5: the Private-collection audience is decoupled from groups. `private_audience.json`
    // holds an explicit `Vec<String>` of npubs who receive every Private collection. Groups are
    // purely a shorthand for commonality of interests (owner ruling 2026-08-04) — membership never
    // grants access to Private collections. Migration = start empty; an absent file ⇒ empty vec.
    pub fn private_audience_path(&self) -> PathBuf {
        self.base.join("private_audience.json")
    }

    pub fn load_private_audience(&self) -> Result<Vec<String>> {
        Ok(read_json_lenient::<Vec<String>>(&self.private_audience_path())
            .context("loading private_audience")?
            .unwrap_or_default())
    }

    pub fn save_private_audience(&self, audience: &[String]) -> Result<()> {
        write_json(&self.private_audience_path(), audience).context("saving private_audience")
    }
}

// ---------------------------------------------------------------------------
// Watch — saved tag/content-type query (local-only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    pub name: String,
    pub tags: Vec<String>,
    pub content_types: Vec<String>,
    #[serde(default)]
    pub last_fired: Option<chrono::DateTime<chrono::Utc>>,
    /// npubs already notified — prevents re-firing for the same peer.
    #[serde(default)]
    pub seen_pubkeys: Vec<String>,
}

/// Parse a persisted `sent_at` (RFC3339, any offset) into a UTC instant. `None` on unparseable input.
fn parse_watermark_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// The future-skew ceiling a read watermark may not exceed: `now + FUTURE_SKEW_SECS`. A `sent_at`
/// inside the skew is admitted (two machines' clocks may legitimately differ by a little); anything
/// beyond it is a poison and is clamped/rejected. Shares the single `hb_core::FUTURE_SKEW_SECS` skew
/// with the presence freshness gate so the two "clock slightly ahead" tolerances can't silently
/// disagree.
fn read_watermark_ceiling(now: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    now + chrono::Duration::seconds(hb_core::FUTURE_SKEW_SECS as i64)
}

/// Whether a parsed watermark sits past the future-skew ceiling — i.e. is poisoned (a peer stamped
/// year 9999, or a pre-fix poisoned value persisted to disk). Such an entry reads as "absent".
fn watermark_is_poisoned(ts: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> bool {
    ts > read_watermark_ceiling(now)
}

// ---------------------------------------------------------------------------
// Read state — per-peer persisted last-read watermark (devtest #16: unifies the three
// unsynchronized unread-badge mechanisms into one persisted signal)
// ---------------------------------------------------------------------------

impl DataStore {
    pub fn read_state_path(&self) -> PathBuf {
        self.base.join("read_state.json")
    }

    /// The per-peer last-read watermark: npub → RFC3339 `sent_at` of the newest message the user has
    /// seen in that conversation. Lenient + defaults empty, like the other small local-state files —
    /// a version mismatch or absent file just means "nothing read yet".
    ///
    /// **Self-heals a poisoned watermark on read.** `sent_at` is peer-controlled (the inner NIP-17
    /// rumor stamp), so a followed peer can stamp year 9999; a watermark past the future-skew ceiling
    /// (or one that no longer parses) is dropped from the returned map, reading as "nothing read yet"
    /// instead of "everything already read". The drop is in-memory — the next
    /// `advance_read_watermark` re-saves the map and persists the heal to disk.
    pub fn load_read_state(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut m = read_json_lenient::<std::collections::HashMap<String, String>>(&self.read_state_path())
            .context("loading read state")?
            .unwrap_or_default();
        let now = chrono::Utc::now();
        m.retain(|_, ts| match parse_watermark_ts(ts) {
            Some(parsed) => !watermark_is_poisoned(parsed, now),
            None => false, // unparseable → treat as absent (self-heal)
        });
        Ok(m)
    }

    pub fn save_read_state(&self, m: &std::collections::HashMap<String, String>) -> Result<()> {
        write_json(&self.read_state_path(), m).context("saving read state")
    }

    /// Advance `npub`'s watermark to `ts`, never rewinding it. `ts` is parsed to a canonical instant
    /// (RFC3339) and **clamped to `now + FUTURE_SKEW_SECS`** before the compare/insert — `sent_at` is
    /// the peer-controlled inner NIP-17 rumor stamp, so a followed peer can send one far-future stamp
    /// (year 9999) and, under a raw string compare, permanently suppress every later unread badge.
    /// Unparseable input is rejected. The compare is against the self-healed map from
    /// `load_read_state`, so an already-poisoned watermark is overwritten on the next legitimate
    /// advance (the self-heal).
    ///
    /// The load→max→save sequence is a read-modify-write over the single `read_state.json` file, so
    /// two overlapping calls (e.g. two DM-poll ticks racing) could otherwise interleave: both load the
    /// same old map, both compute their own max, and whichever save lands second wins — even if it
    /// carries the OLDER of the two timestamps, rewinding the watermark and resurrecting a phantom
    /// unread badge. `READ_STATE_LOCK` serializes the whole load+max+save so the RMW is atomic
    /// process-wide; the guarded section is a couple of small synchronous file ops, never held across
    /// an `.await`.
    pub fn advance_read_watermark(&self, npub: &str, ts: &str) -> Result<()> {
        static READ_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = READ_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Parse the peer-controlled ts to a canonical instant; reject unparseable — no ordering can
        // be established, so the watermark must not move (and must not be poisoned) on garbage input.
        let parsed = parse_watermark_ts(ts)
            .ok_or_else(|| anyhow::anyhow!("unparseable read-watermark timestamp: {ts:?}"))?;
        let ceiling = read_watermark_ceiling(chrono::Utc::now());
        // Clamp a far-future stamp down to the future-skew ceiling before it can poison the compare.
        let clamped = parsed.min(ceiling);
        // Only the poison case changes what is persisted: a legitimate `ts` is stored verbatim (the
        // pre-existing behaviour), while a clamped stamp is stored in the canonical RFC3339 the rest
        // of the codebase emits (`to_rfc3339`).
        let stored = if parsed > ceiling { clamped.to_rfc3339() } else { ts.to_string() };

        let mut m = self.load_read_state()?; // already self-heals poisoned/corrupt entries
        let advance = match m.get(npub) {
            // `load_read_state` pre-filters to parseable, unpoisoned entries, so this parse succeeds;
            // the `unwrap_or(true)` defaults to the "advance" direction (heal, never rewind) as a
            // defensive fallback that should be unreachable.
            Some(existing) => parse_watermark_ts(existing).map(|e| clamped > e).unwrap_or(true),
            None => true,
        };
        if advance {
            m.insert(npub.to_string(), stored);
            self.save_read_state(&m)?;
        }
        Ok(())
    }

    // ── Per-topic announcement-seen watermark (devtest #2) — the Topics nav badge's persisted signal,
    //    the topic-channel analogue of `read_state.json`: topic_id → newest announcement `ts` the user
    //    has seen (opened the channel past). Announcement ts is a unix second, so a numeric max is the
    //    chronological compare (unlike read_state's RFC3339 strings).
    pub fn announce_seen_path(&self) -> PathBuf {
        self.base.join("announce_seen.json")
    }

    pub fn load_announce_seen(&self) -> Result<std::collections::HashMap<String, u64>> {
        Ok(
            read_json_lenient::<std::collections::HashMap<String, u64>>(&self.announce_seen_path())
                .context("loading announce-seen state")?
                .unwrap_or_default(),
        )
    }

    pub fn save_announce_seen(&self, m: &std::collections::HashMap<String, u64>) -> Result<()> {
        write_json(&self.announce_seen_path(), m).context("saving announce-seen state")
    }

    /// Advance `topic_id`'s announcement watermark to `ts`, never rewinding. Serialized like
    /// [`advance_read_watermark`] so two overlapping poll ticks can't interleave the read-modify-write
    /// and rewind the watermark (resurrecting a phantom badge).
    pub fn advance_announce_seen(&self, topic_id: &str, ts: u64) -> Result<()> {
        static ANNOUNCE_SEEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ANNOUNCE_SEEN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_announce_seen()?;
        let advance = match m.get(topic_id) {
            Some(existing) => ts > *existing,
            None => true,
        };
        if advance {
            m.insert(topic_id.to_string(), ts);
            self.save_announce_seen(&m)?;
        }
        Ok(())
    }

    // ── Manifest-request ask trace (M17 W7.1a) — the persisted "I already asked this peer for this
    //    collection's full list" record. `send_dm_inner` (`chat.rs:108`) delivers a gift-wrap to the
    //    recipient's inbox only — NO self-copy — so without this record the ask leaves zero local trace
    //    and the button reads as dead. One entry per `(npub, slug)`, overwritten on re-ask. Keyed by
    //    `"{npub}|{slug}"` so the same slug across two peers (or two slugs on one peer) stay distinct.
    pub fn manifest_asks_path(&self) -> PathBuf {
        self.base.join("manifest_asks.json")
    }

    pub fn load_manifest_asks(&self) -> Result<std::collections::HashMap<String, ManifestAsk>> {
        Ok(read_json_lenient::<std::collections::HashMap<String, ManifestAsk>>(
            &self.manifest_asks_path(),
        )
        .context("loading manifest asks")?
        .unwrap_or_default())
    }

    pub fn save_manifest_asks(
        &self,
        m: &std::collections::HashMap<String, ManifestAsk>,
    ) -> Result<()> {
        write_json(&self.manifest_asks_path(), m).context("saving manifest asks")
    }

    /// Record that we asked `npub` for `slug`'s full manifest at `sent_at`, persisting `fingerprint_seen`
    /// alongside it (the requester's view of the snapshot when they asked). Overwrites any prior entry for
    /// the same `(npub, slug)` — a re-ask is a re-ask; the newest send wins. Serialized like
    /// [`advance_read_watermark`] so two overlapping asks (a double-click, two windows) can't interleave
    /// the load→modify→save and drop one another's entry.
    pub fn record_manifest_ask(
        &self,
        npub: &str,
        slug: &str,
        fingerprint_seen: &str,
        sent_at: &str,
        nonce: &str,
    ) -> Result<()> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        m.insert(
            manifest_ask_key(npub, slug),
            ManifestAsk {
                fingerprint_seen: fingerprint_seen.to_string(),
                sent_at: sent_at.to_string(),
                nonce: nonce.to_string(),
                // A re-ask is a fresh authorization: new nonce, so any prior claim or spent flag
                // must be cleared with it.
                claimed_by: None,
                spent: false,
            },
        );
        self.save_manifest_asks(&m)
    }

    /// Consume the ask for `(npub, slug)` — called after a redemption **succeeds**, so the
    /// authorization it represents is spent (owner ruling ①: one ask, one auto-dial).
    ///
    /// Deliberately not called on a failed attempt: a dial that never connected has cost nothing and
    /// must remain retryable, exactly as the ticket itself does.
    /// **Atomically claim this ask for one ticket, before any dial.**
    ///
    /// The whole check-and-claim happens under [`MANIFEST_ASKS_LOCK`], which is what makes it a gate
    /// rather than a suggestion. Validating and *then* dialing was a TOCTOU: two concurrent invokes
    /// carrying different peer-crafted tickets with the same valid nonce both passed and both
    /// connected. **Validation that is not a claim is not a gate.**
    ///
    /// Re-claiming with the *same* `request_id` is granted, so a failed dial can be retried; a
    /// different one is refused until the user makes a fresh ask. That is what stops a peer sending
    /// ticket after ticket — each with a new `request_id` and a new node address — and collecting an
    /// automatic dial per attempt.
    pub fn claim_manifest_ask(
        &self,
        npub: &str,
        slug: &str,
        nonce: &str,
        request_id: &str,
    ) -> Result<AskClaim> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        let key = manifest_ask_key(npub, slug);
        let Some(ask) = m.get_mut(&key) else { return Ok(AskClaim::Unsolicited) };
        // An empty nonce on either side never matches — a pre-ruling ask fails closed by
        // construction rather than by a branch.
        if ask.nonce.is_empty() || nonce.is_empty() || ask.nonce != nonce {
            return Ok(AskClaim::Unsolicited);
        }
        if ask.spent {
            return Ok(AskClaim::Spent);
        }
        match ask.claimed_by.as_deref() {
            Some(owner) if owner != request_id => return Ok(AskClaim::ClaimedByAnother),
            Some(_) => return Ok(AskClaim::Granted),
            None => {}
        }
        ask.claimed_by = Some(request_id.to_string());
        self.save_manifest_asks(&m)?;
        Ok(AskClaim::Granted)
    }

    /// Mark the ask answered — **durably**, so a restart cannot re-authorize it. The in-memory
    /// marker this replaces died with the page.
    ///
    /// Conditional on `expected_nonce`: a re-ask made while an older ticket was in flight must not be
    /// marked spent by that older ticket's completion.
    pub fn spend_manifest_ask(&self, npub: &str, slug: &str, expected_nonce: &str) -> Result<()> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        if let Some(ask) = m.get_mut(&manifest_ask_key(npub, slug)) {
            if ask.nonce == expected_nonce {
                ask.spent = true;
                self.save_manifest_asks(&m)?;
            }
        }
        Ok(())
    }

    // ── Issued transport tickets (M18 W4) — the owner side's record of what it actually issued, so a
    //    redemption arriving minutes or a restart later can be authorized against it. This is the
    //    durable half of `ManifestSource::issued`/`record_consumed`; the plane's in-flight set covers
    //    only the same-process race, and a ticket is valid until redeemed, so the *spent* bit has to
    //    survive a restart or a replay after relaunch would be served a second manifest.
    //
    //    Keyed by `request_id` (a fresh 128-bit value per approval), which is also the ticket's
    //    binding — one ticket per request, so one record per request.
    pub fn issued_tickets_path(&self) -> PathBuf {
        self.base.join("issued_tickets.json")
    }

    pub fn load_issued_tickets(&self) -> Result<std::collections::HashMap<String, IssuedTicketRecord>> {
        Ok(read_json_lenient::<std::collections::HashMap<String, IssuedTicketRecord>>(
            &self.issued_tickets_path(),
        )
        .context("loading issued tickets")?
        .unwrap_or_default())
    }

    pub fn save_issued_tickets(
        &self,
        m: &std::collections::HashMap<String, IssuedTicketRecord>,
    ) -> Result<()> {
        write_json(&self.issued_tickets_path(), m).context("saving issued tickets")
    }

    pub fn load_issued_ticket(&self, request_id: &str) -> Result<Option<IssuedTicketRecord>> {
        Ok(self.load_issued_tickets()?.remove(request_id))
    }

    /// Persist a freshly-minted ticket. Called **before** the ticket is DM'd: the reverse order would
    /// hand a peer a ticket this node cannot authorize, which is indistinguishable from a forgery. An
    /// orphaned record (the DM then failed) is harmless — nobody holds the ticket that matches it.
    ///
    /// LRU-evicts the stalest **consumed** record when over [`ISSUED_TICKET_CAP`], never an unspent
    /// one: evicting an unspent ticket would silently revoke an approval a human already gave, and
    /// the asker would see the refusal reserved for a forgery.
    pub fn record_issued_ticket(&self, record: &IssuedTicketRecord) -> Result<()> {
        let _guard = ISSUED_TICKETS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_issued_tickets()?;
        m.insert(record.ticket.request_id.clone(), record.clone());
        prune_issued_tickets(&mut m);
        self.save_issued_tickets(&m)
    }

    /// Record the receipt for `request_id`. Idempotent, and deliberately a no-op for an unknown id —
    /// the plane hands us a `ConsumedTicket` it could only have obtained from a `RedemptionGrant`,
    /// but this layer is not the place to turn a bookkeeping surprise into a failed delivery the peer
    /// has already received.
    pub fn mark_ticket_consumed(
        &self,
        request_id: &str,
        delivered_bytes: usize,
        now: u64,
    ) -> Result<()> {
        let _guard = ISSUED_TICKETS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_issued_tickets()?;
        if let Some(rec) = m.get_mut(request_id) {
            rec.consumed_at = Some(now);
            rec.delivered_bytes = Some(delivered_bytes);
            self.save_issued_tickets(&m)?;
        }
        Ok(())
    }
}

/// Serializes every load-modify-save of the issued-ticket map.
///
/// **One shared static, not one per function.** A `static` declared inside a function body is its own
/// distinct item, so a per-function `ISSUED_TICKETS_LOCK` in `record_issued_ticket` and another in
/// `mark_ticket_consumed` never serialized against *each other* — a concurrent approval and consume
/// could interleave their load→modify→save and revert `consumed_at` back to `None`, letting an
/// already-delivered ticket be redeemed again. Same defect `MANIFEST_ASKS_LOCK` already fixed once.
static ISSUED_TICKETS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes every load-modify-save of the ask trace.
///
/// **One shared static, not one per function.** A `static` declared inside a function body is its own
/// distinct item, so the two locks this replaced never serialized against *each other* — a
/// `record_manifest_ask` and a `claim_manifest_ask` overlapping could interleave their
/// load→modify→save and lose the newer write entirely.
static MANIFEST_ASKS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How many issued-ticket records to keep. Only **consumed** records are eligible for eviction, so
/// this bounds the audit tail rather than the set of live approvals.
pub const ISSUED_TICKET_CAP: usize = 512;

/// The persisted record of one approval: the ticket verbatim (so redemption compares against the
/// exact bytes issued, not a reconstruction), who it was addressed to, and whether it has been spent.
///
/// **`redeemer_npub` is stored rather than the standing** — standing is re-read at redeem time by
/// owner ruling (2026-07-30), so freezing it here would defeat revocability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedTicketRecord {
    pub ticket: hb_core::TransportTicket,
    /// The npub this ticket was DM'd to — whose live standing gates the redemption.
    pub redeemer_npub: String,
    /// Unix seconds the asker's receipt landed; `None` while the ticket is unspent.
    pub consumed_at: Option<u64>,
    /// Payload size the receipt attested to. Provenance only.
    pub delivered_bytes: Option<usize>,
}

/// Drop the stalest consumed records until the map is within [`ISSUED_TICKET_CAP`]. Unspent tickets
/// are never candidates — see [`DataStore::record_issued_ticket`].
pub(crate) fn prune_issued_tickets(m: &mut std::collections::HashMap<String, IssuedTicketRecord>) {
    if m.len() <= ISSUED_TICKET_CAP {
        return;
    }
    let mut spent: Vec<(String, u64)> = m
        .iter()
        .filter(|(_, r)| r.consumed_at.is_some())
        .map(|(k, r)| (k.clone(), r.ticket.issued_at))
        .collect();
    spent.sort_by_key(|(_, issued_at)| *issued_at);
    for (key, _) in spent {
        if m.len() <= ISSUED_TICKET_CAP {
            break;
        }
        m.remove(&key);
    }
}

/// The persisted ask trace: `fingerprint_seen` (the snapshot fingerprint the requester observed when
/// they asked — for staleness notes on the fulfil side) + `sent_at` (RFC3339 UTC, as everywhere else
/// here — the asked-state relative label and the re-ask cooldown both derive from it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestAsk {
    pub fingerprint_seen: String,
    pub sent_at: String,
    /// **The nonce we minted for THIS ask** (owner ruling ① 2026-07-31). A ticket auto-redeems only
    /// if it echoes this exact value, which is what turns "I asked this peer once" from a standing
    /// reusable authorization into a one-ask one-dial permission.
    ///
    /// `serde(default)` so a trace written before the ruling still loads — it deserializes empty,
    /// and an empty stored nonce **never matches**, so those asks simply stop auto-dialling until
    /// the user asks again. Fail closed by construction rather than by a branch.
    #[serde(default)]
    pub nonce: String,
    /// The **one** ticket allowed to answer this ask — its `request_id`, recorded durably the first
    /// time a redemption claims it.
    ///
    /// Binding the ticket to a nonce was not enough. A failed dial released the ask, and the peer
    /// (which *receives* the nonce, being the party asked) could then send a fresh ticket with a new
    /// `request_id` and **a new node address**, which claimed it again.
    #[serde(default)]
    pub claimed_by: Option<String>,
    /// Answered. Durable, so a restart cannot resurrect the authorization.
    #[serde(default)]
    pub spent: bool,
}

/// Why a redemption may not proceed. Anything but [`AskClaim::Granted`] must not dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskClaim {
    /// This ticket owns the ask — dial.
    Granted,
    /// No ask, or the nonce does not match: we never asked for this.
    Unsolicited,
    /// Already answered.
    Spent,
    /// A different ticket already claimed this ask.
    ClaimedByAnother,
}

/// The on-disk key for an ask trace: `"{npub}|{slug}"`. The pipe is unambiguous because npubs and
/// slugs never contain `|` (bech32 / URL-safe slug charset).
pub fn manifest_ask_key(npub: &str, slug: &str) -> String {
    format!("{npub}|{slug}")
}

impl DataStore {
    pub fn watches_path(&self) -> PathBuf {
        self.base.join("watches.json")
    }

    pub fn load_watches(&self) -> Result<Vec<Watch>> {
        Ok(read_json_lenient::<Vec<Watch>>(&self.watches_path())
            .context("loading watches")?
            .unwrap_or_default())
    }

    pub fn save_watches(&self, watches: &[Watch]) -> Result<()> {
        write_json(&self.watches_path(), watches).context("saving watches")
    }
}

// ---------------------------------------------------------------------------
// StoredTopic — a §11 Topic I'm a member of (local, M11)
// ---------------------------------------------------------------------------

use hb_core::topic::{TopicKey, TopicMeta};

/// A Topic I have joined, persisted locally so I can read/post/leave across restarts. The `key` is the
/// room secret (hex-serialized, the gate to the roster + channel); `membership_json` is my published
/// membership event (opaque), kept so leaving can NIP-09-retract it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTopic {
    #[serde(flatten)]
    pub meta: TopicMeta,
    pub key: TopicKey,
    pub joined_at: u64,
    /// My published membership event JSON (opaque) — kept so `leave` can retract exactly that event.
    #[serde(default)]
    pub membership_json: Option<String>,
}

impl DataStore {
    pub fn topics_path(&self) -> PathBuf {
        self.base.join("topics.json")
    }

    pub fn load_topics(&self) -> Result<Vec<StoredTopic>> {
        Ok(read_json_lenient::<Vec<StoredTopic>>(&self.topics_path())
            .context("loading topics")?
            .unwrap_or_default())
    }

    pub fn save_topics(&self, topics: &[StoredTopic]) -> Result<()> {
        write_json(&self.topics_path(), topics).context("saving topics")
    }

    pub fn topic_nonces_path(&self) -> PathBuf {
        self.base.join("topic_nonces.json")
    }

    /// The persisted **seen-nonce set** (redeemed invites, keyed `(topic_id, invitee)`). Persisting it
    /// is what stops a restart re-accepting an old invite (M11 Decision E). Device-local by design.
    pub fn load_topic_nonces(&self) -> Result<std::collections::HashSet<String>> {
        Ok(read_json_lenient::<std::collections::HashSet<String>>(&self.topic_nonces_path())
            .context("loading topic nonces")?
            .unwrap_or_default())
    }

    pub fn save_topic_nonces(&self, nonces: &std::collections::HashSet<String>) -> Result<()> {
        write_json(&self.topic_nonces_path(), nonces).context("saving topic nonces")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn sample_identity() -> StoredIdentity {
        use nostr::prelude::ToBech32;
        let id = hb_core::Identity::generate();
        let nsec = id.keys().secret_key().to_bech32().unwrap();
        StoredIdentity {
            version: 1,
            nsec,
            browse_key_hex: hex::encode([9u8; 32]),
            transport_secret_hex: hex::encode([7u8; 32]),
        }
    }

    /// **M18 W2 — the migration PERSISTS, and is stable across loads.**
    ///
    /// `load_identity` is the single choke point every load path goes through, so it is where a
    /// 2-key record gains its transport secret. Two properties, and the second is the one that
    /// matters: minting per-load would hand a peer a different node identity every launch.
    #[test]
    fn load_identity_mints_a_transport_key_once_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());

        // Write a genuine 2-key record: save a full one, then strip the field back out on disk.
        let mut legacy = sample_identity();
        legacy.transport_secret_hex = String::new();
        store.save_identity(&legacy).unwrap();

        let first = store.load_identity().unwrap().unwrap();
        assert_eq!(first.transport_secret_hex.len(), 64, "the load mints a transport key");
        assert_eq!(first.browse_key_hex, legacy.browse_key_hex, "the browse-key is untouched");
        assert_eq!(first.nsec, legacy.nsec, "the nsec is untouched");

        let second = store.load_identity().unwrap().unwrap();
        assert_eq!(
            second.transport_secret_hex, first.transport_secret_hex,
            "the minted key was PERSISTED — a second load reads it back rather than re-minting"
        );
    }

    /// The other half of the same rule: a record that already has a transport secret keeps it.
    /// A background actor must not silently rewrite stored data (the v0.12.6 `path_alias` lesson).
    #[test]
    fn load_identity_never_rewrites_an_existing_transport_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let id = sample_identity();
        store.save_identity(&id).unwrap();

        let loaded = store.load_identity().unwrap().unwrap();
        assert_eq!(
            loaded.transport_secret_hex, id.transport_secret_hex,
            "an existing transport secret survives the load untouched"
        );
    }

    fn contact_fixture(npub: &str) -> CachedPeer {
        CachedPeer {
            npub: npub.into(),
            source: ContactSource::Manual,
            browse_key_hex: None,
            petname: None,
            profile: None,
            collections: vec![],
            listings_state: Default::default(), // QURATOR-134: fixtures predate the tri-state; Fetched is the least-wrong default
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        }
    }

    #[test]
    fn a_contact_stored_before_the_fingerprint_gets_one_on_read() {
        // The owner's report, 2026-08-13: "Contacts/Browse/Topics UI uplift have not been done and
        // look nothing like what the artifacts promised". They HAD been done — but `fingerprint` is
        // only written when a peer is RESOLVED (follow / refresh / paste_key), and `list_contacts`
        // was a straight disk read. So every contact saved before M21 W4 came back with `None`, the
        // card took its documented no-fingerprint path (no avatar ring, no word row), and the whole
        // redesign was invisible on real data until each contact was refreshed by hand.
        //
        // The fingerprint is a pure function of the npub, so the fix is to derive it on read.
        let (_dir, store) = test_store();
        let ident = hb_core::Identity::generate();
        let npub = ident.npub();
        let hash = CachedPeer::pubkey_hash(&npub);

        // A record exactly as it sits on disk for a pre-M21-W4 contact.
        let stored = contact_fixture(&npub);
        assert!(stored.fingerprint.is_none(), "fixture must model the pre-fingerprint record");
        store.save_contact(&hash, &stored).unwrap();

        let listed = store.list_contacts().unwrap();
        let got = listed.iter().find(|p| p.npub == npub).expect("contact must come back");
        let fp = got.fingerprint.as_ref().expect("a stored contact with no fingerprint must get one on read");

        // It must be the REAL derivation, not merely non-empty — a wrong-but-present fingerprint is
        // worse than none, because the whole point is impersonation resistance.
        let expected = hb_core::fingerprint::fingerprint(&ident.public_key());
        assert_eq!(fp.words, expected.words, "backfilled words must match the npub's true fingerprint");
        assert_eq!(fp.color_hex, expected.color_hex, "backfilled colour must match the npub's true fingerprint");

        // Read-time only: the file on disk is NOT rewritten. Nothing is migrated, so this can never
        // corrupt a contact record.
        let raw = std::fs::read_to_string(store.base.join("contacts").join(format!("{hash}.json"))).unwrap();
        let disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            disk.get("fingerprint").map(|v| v.is_null()).unwrap_or(true),
            "backfill must not write to disk — it is a read-time derivation"
        );
    }

    #[test]
    fn a_stored_fingerprint_is_never_overwritten_by_the_backfill() {
        // The backfill fills a HOLE; it must not fight a real resolve. If it recomputed
        // unconditionally it would be indistinguishable from the derive-always case and would mask a
        // genuine mismatch between a stored fingerprint and its npub.
        let (_dir, store) = test_store();
        let ident = hb_core::Identity::generate();
        let npub = ident.npub();
        let hash = CachedPeer::pubkey_hash(&npub);

        let mut stored = contact_fixture(&npub);
        let sentinel = hb_core::fingerprint::Fingerprint {
            words: vec![
                "sentinel".into(),
                "sentinel".into(),
                "sentinel".into(),
                "sentinel".into(),
                "sentinel".into(),
            ],
            color_hex: "#abcdef12".into(),
        };
        stored.fingerprint = Some(sentinel.clone());
        store.save_contact(&hash, &stored).unwrap();

        let listed = store.list_contacts().unwrap();
        let got = listed.iter().find(|p| p.npub == npub).unwrap();
        assert_eq!(
            got.fingerprint.as_ref().unwrap().words,
            sentinel.words,
            "an already-stored fingerprint must be returned untouched"
        );
    }

    #[test]
    fn an_unparseable_npub_is_left_alone_rather_than_guessed_at() {
        // `contact_fixture` uses "npub1a", which is not a decodable npub. The backfill must leave
        // such a record as None — inventing a fingerprint for a key we cannot parse would attach an
        // impersonation signal to an identity we never verified.
        let (_dir, store) = test_store();
        let hash = CachedPeer::pubkey_hash("npub1a");
        store.save_contact(&hash, &contact_fixture("npub1a")).unwrap();

        let listed = store.list_contacts().unwrap();
        let got = listed.iter().find(|p| p.npub == "npub1a").unwrap();
        assert!(got.fingerprint.is_none(), "an unparseable npub must not receive a fabricated fingerprint");
    }

    #[test]
    fn a_resolve_rebuilt_contact_cannot_wipe_last_presence() {
        // W5 review (HIGH): `refresh_contact` / `follow` / `paste_key` rebuild a CachedPeer from a
        // relay resolve, which carries no presence stamp — and Contacts refreshes every contact on
        // mount. Without the save-side guard the durable last-seen was erased seconds after the
        // poll wrote it, and the row read "Last seen — unknown" after every restart.
        let (_dir, store) = test_store();
        let hash = CachedPeer::pubkey_hash("npub1a");
        let seen = chrono::Utc::now() - chrono::Duration::hours(3);

        let mut polled = contact_fixture("npub1a");
        polled.last_presence = Some(seen);
        store.save_contact(&hash, &polled).unwrap();

        // A refresh saves a freshly resolved peer whose last_presence is None.
        let mut resolved = contact_fixture("npub1a");
        resolved.petname = Some("alice".into());
        store.save_contact(&hash, &resolved).unwrap();

        let loaded = store.load_contact(&hash).unwrap().unwrap();
        assert_eq!(loaded.last_presence, Some(seen), "the presence stamp survives the rebuild");
        assert_eq!(loaded.petname.as_deref(), Some("alice"), "and the resolve's own fields land");
    }

    #[test]
    fn the_poll_can_still_move_the_presence_stamp_forward() {
        // The guard defers to disk only for `None` — an incoming Some always wins, in both
        // directions, so the poll stays the owner of the value.
        let (_dir, store) = test_store();
        let hash = CachedPeer::pubkey_hash("npub1b");
        let old = chrono::Utc::now() - chrono::Duration::hours(5);
        let new = chrono::Utc::now();

        let mut first = contact_fixture("npub1b");
        first.last_presence = Some(old);
        store.save_contact(&hash, &first).unwrap();

        let mut second = contact_fixture("npub1b");
        second.last_presence = Some(new);
        store.save_contact(&hash, &second).unwrap();

        assert_eq!(store.load_contact(&hash).unwrap().unwrap().last_presence, Some(new));
    }

    // A 0-byte identity file (the on-disk symptom of a failed/partial write) must be treated
    // as "absent" so the app regenerates, not as an unreadable identity that dead-ends.
    #[test]
    fn empty_identity_file_treated_as_absent() {
        let (_dir, store) = test_store();
        let path = store.identity_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();

        let loaded = store.load_identity().expect("empty identity file must not error");
        assert!(loaded.is_none(), "a 0-byte identity file must load as None, got {loaded:?}");
    }

    #[test]
    fn identity_save_load_roundtrip() {
        let (_dir, store) = test_store();
        let stored = sample_identity();
        store.save_identity(&stored).unwrap();
        let loaded = store.load_identity().unwrap().unwrap();
        assert_eq!(loaded.nsec, stored.nsec);
        assert_eq!(loaded.browse_key_hex, stored.browse_key_hex);
    }

    #[test]
    fn stored_identity_debug_redacts_secrets() {
        let stored = sample_identity();
        let debug_str = format!("{stored:?}");
        assert!(!debug_str.contains(&stored.nsec), "Debug must not leak the nsec");
        assert!(!debug_str.contains(&stored.browse_key_hex), "Debug must not leak the browse-key");
        assert!(debug_str.contains("[REDACTED]"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn identity_file_has_mode_600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = test_store();
        store.save_identity(&sample_identity()).unwrap();
        let mode = std::fs::metadata(store.identity_path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "identity.json must have mode 600");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn save_identity_tightens_a_preexisting_loose_file() {
        // Regression for the convergent chorus finding: even if an older build (or a tampered
        // profile) left the identity file world-readable, a re-save (e.g. an import / restore
        // re-wrap) must re-assert 0600 — never leave a widen-window on the nsec.
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = test_store();
        store.save_identity(&sample_identity()).unwrap();
        let path = store.identity_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.save_identity(&sample_identity()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a re-save must re-assert 0600 on a pre-existing loose file");
    }

    #[test]
    fn settings_gains_fields_with_backward_compatible_defaults() {
        // An old settings.json lacking the M5/M9 fields must still deserialize (serde(default)).
        let old = r#"{"relay_urls":["wss://r.example"],"allow_dms":true}"#;
        let s: Settings = serde_json::from_str(old).expect("old settings must still deserialize");
        assert_eq!(s.relay_urls, vec!["wss://r.example".to_string()]);
        assert!(!s.privacy_notice_acknowledged, "defaults to not-acknowledged");
        assert_eq!(s.last_seen_version, "", "defaults to empty (fresh install)");
        // M9 fields default sensibly on an old file: auto-update + online-count ON, reconcile OFF.
        assert!(s.snapshot_auto_update, "snapshot auto-update defaults ON");
        assert!(!s.snapshot_reconcile_poll, "reconcile poll defaults OFF");
        assert!(s.show_online_count, "online-count chip defaults ON");
        // devtest #5: a pre-existing settings.json with no `discoverable` key loads as false — the
        // intended silent de-list, no migration.
        assert!(!s.discoverable, "discoverable defaults OFF on an old file");
        // M16 W3: a pre-M16 file with no `big_relay_url` loads empty — the full-manifest feature is
        // off until the owner configures a big relay.
        assert_eq!(s.big_relay_url, "", "big_relay_url defaults empty (feature off) on an old file");
    }

    #[test]
    fn full_object_save_preserves_all_m9_fields() {
        // The M5 fullSettings() gotcha guard: saving the whole object must round-trip every field,
        // never silently drop one. Persist a non-default mix and reload it.
        let (_dir, store) = test_store();
        let s = Settings {
            relay_urls: vec!["wss://r.example".into()],
            allow_dms: false,
            privacy_notice_acknowledged: true,
            last_seen_version: "0.9.7".into(),
            snapshot_auto_update: false,
            snapshot_reconcile_poll: true,
            show_online_count: false,
            discoverable: true,
            big_relay_url: "ws://big.example:7777".into(),
        };
        store.save_settings(&s).unwrap();
        let r = store.load_settings().unwrap().unwrap();
        assert_eq!(r.relay_urls, s.relay_urls);
        assert!(!r.allow_dms);
        assert!(r.privacy_notice_acknowledged);
        assert_eq!(r.last_seen_version, "0.9.7");
        assert!(!r.snapshot_auto_update, "auto-update toggle preserved");
        assert!(r.snapshot_reconcile_poll, "reconcile toggle preserved");
        assert!(!r.show_online_count, "online-count toggle preserved");
        assert!(r.discoverable, "discoverable toggle preserved");
        assert_eq!(r.big_relay_url, "ws://big.example:7777", "big_relay_url preserved");
    }

    #[test]
    fn snapshot_fingerprint_and_scan_spec_roundtrip() {
        use hb_core::SnapshotFingerprint;
        let (_dir, store) = test_store();
        let fp = SnapshotFingerprint("deadbeef".into());
        store.save_snapshot_fingerprint("films", &fp).unwrap();
        assert_eq!(store.load_snapshot_fingerprint("films").unwrap(), Some(fp));

        let spec = ScanSpec {
            root: "/mnt/share/films".into(),
            include: vec!["criterion".into()],
            exclude: vec!["*.nfo".into()],
            total_bytes: 4096,
        };
        store.save_scan_spec("films", &spec).unwrap();
        let loaded = store.load_scan_spec("films").unwrap().unwrap();
        assert_eq!(loaded.root, "/mnt/share/films");
        assert_eq!(loaded.include, vec!["criterion".to_string()]);
        assert_eq!(loaded.total_bytes, 4096, "total_bytes round-trips through the scan spec");
    }

    #[test]
    fn list_published_slugs_only_returns_published() {
        let (_dir, store) = test_store();
        let mk = |slug: &str| {
            let col = Collection {
                slug: slug.into(),
                path_alias: slug.into(),
                description: None,
                item_count: 0,
                est_size: None,
                content_types: vec![],
                tags: vec![],
                languages: vec![],
                visibility: hb_core::types::Visibility::Public,
                sorted: false,
                last_updated: chrono::Utc::now(),
                listing: vec![],
            };
            store.save_collection_draft(&col).unwrap();
        };
        mk("published-one");
        mk("draft-only");
        store.save_published("published-one", "{}").unwrap();
        let slugs = store.list_published_slugs().unwrap();
        assert_eq!(slugs, vec!["published-one".to_string()], "only the published collection is in scope");
    }

    #[test]
    fn privacy_notice_shown_once_then_acknowledged_persists() {
        let (_dir, store) = test_store();
        // Fresh profile: the notice should show (not yet acknowledged).
        let s = store.load_settings().unwrap().unwrap_or_default();
        assert!(!s.privacy_notice_acknowledged, "shown iff not acknowledged");
        // Acknowledge + persist.
        let mut s = s;
        s.privacy_notice_acknowledged = true;
        store.save_settings(&s).unwrap();
        // Reload: it stays acknowledged, so it never shows again.
        let reloaded = store.load_settings().unwrap().unwrap();
        assert!(reloaded.privacy_notice_acknowledged, "acknowledgement persists across reload");
    }

    fn _assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    #[test]
    fn stored_identity_zeroizes_secrets_on_drop() {
        // Type-level, mirroring hb-core's DerivedKey pattern: assert the compile-time bound
        // rather than UB memory inspection — the nsec + browse-key hex strings are wiped when
        // any in-memory copy (load/save/backup) drops.
        _assert_zeroize_on_drop::<StoredIdentity>();
    }

    #[test]
    fn write_json_replaces_via_rename_and_leaves_no_tmp_residue() {
        // Contract (revised by chorus M13 #1): a write's OWN stage file never persists. A stage
        // file left by a *crashed* earlier write is inert — never read (read_json opens the exact
        // target path), removed by wipe() — and deliberately NOT consumed by later writes: the
        // old shared-name consumption was exactly the same-process collision the finding flagged.
        let (_dir, store) = test_store();
        store.save_settings(&Settings::default()).unwrap();

        let s = Settings { last_seen_version: "0.11.0".into(), ..Default::default() };
        store.save_settings(&s).unwrap();

        let reloaded = store.load_settings().unwrap().unwrap();
        assert_eq!(reloaded.last_seen_version, "0.11.0", "content is entirely the new write");
        let residue: Vec<String> = std::fs::read_dir(store.base_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(residue.is_empty(), "no temp files may persist after a write, found: {residue:?}");
    }

    #[test]
    fn tmp_paths_are_unique_per_call_so_same_process_writers_cannot_collide() {
        // Chorus M13 finding #1: `<name>.tmp.<pid>` alone is shared by every writer in this
        // process — two concurrent tasks staging the same target could interleave through ONE
        // temp file (A stages, B re-stages, A renames B's bytes into place as its own). Each
        // stage must be private to its call.
        let target = Path::new("settings.json");
        assert_ne!(
            tmp_path(target),
            tmp_path(target),
            "two stages of the same target must not share a temp file"
        );
    }

    #[test]
    fn concurrent_writers_to_one_target_all_succeed_and_leave_one_complete_file() {
        let (_dir, store) = test_store();
        let path = std::sync::Arc::new(store.base_dir().join("contended.json"));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = std::sync::Arc::clone(&path);
                std::thread::spawn(move || {
                    for j in 0..25 {
                        write_json(&path, &serde_json::json!({ "writer": i, "iter": j })).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&*path).unwrap()).unwrap();
        assert!(v.get("writer").is_some(), "the surviving file is one complete write, got {v}");
        let residue: Vec<String> = std::fs::read_dir(store.base_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(residue.is_empty(), "no temp residue after contended writes: {residue:?}");
    }

    #[test]
    fn pubkey_hash_is_16_bytes_32_hex_chars() {
        let h = CachedPeer::pubkey_hash("npub1exampleexampleexample");
        assert_eq!(h.len(), 32, "16 bytes of SHA-256 → 32 hex chars");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "hex only, got {h}");
        assert_eq!(h, CachedPeer::pubkey_hash("npub1exampleexampleexample"), "stable for the same npub");
        assert_ne!(h, CachedPeer::pubkey_hash("npub1other"));
    }

    /// M13 HANDOVER gap #5: a pre-M13 cached contact stored `collections` as plain `Collection`
    /// objects — the K-of-N parts fields didn't exist yet. `PeerCollection`'s `#[serde(flatten)]` +
    /// `#[serde(default)]` on `parts_total`/`parts_present` must still load such a file, with those
    /// fields defaulting to `None` (never fabricate a "K of N" badge from stale cache data).
    #[test]
    fn pre_m13_cached_contact_still_loads() {
        let (_dir, store) = test_store();
        let hash = CachedPeer::pubkey_hash("npub1exampleexampleexample");
        let legacy_json = r#"{
            "npub": "npub1exampleexampleexample",
            "source": "Manual",
            "browse_key_hex": null,
            "petname": null,
            "profile": null,
            "collections": [{
                "slug": "films",
                "path_alias": "Films",
                "item_count": 3,
                "content_types": ["video"],
                "tags": [],
                "languages": [],
                "visibility": "Public",
                "sorted": false,
                "last_updated": "2026-01-01T00:00:00Z",
                "listing": []
            }],
            "online": false,
            "last_fetched": "2026-01-01T00:00:00Z",
            "local_tags": [],
            "fingerprint": null
        }"#;
        std::fs::create_dir_all(store.contact_path(&hash).parent().unwrap()).unwrap();
        std::fs::write(store.contact_path(&hash), legacy_json).unwrap();

        let loaded = store.load_contact(&hash).unwrap().expect("a pre-M13 cached contact must still load");
        assert_eq!(loaded.collections.len(), 1);
        assert_eq!(loaded.collections[0].collection.slug, "films");
        assert_eq!(loaded.collections[0].parts_total, None, "an old cache entry carries no parts info");
        assert_eq!(loaded.collections[0].parts_present, None);
    }

    #[test]
    fn wipe_clears_everything_the_store_writes() {
        use std::collections::HashSet;
        let (_dir, store) = test_store();
        // Exercise every write path the store has today.
        store.save_identity(&sample_identity()).unwrap();
        let profile: Profile =
            serde_json::from_str(r#"{"display_name":"h","updated":"2026-01-01T00:00:00Z"}"#).unwrap();
        store.save_profile_draft(&profile).unwrap();
        let col = Collection {
            slug: "films".into(),
            path_alias: "films".into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: hb_core::types::Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        store.save_scan_spec("films", &ScanSpec::default()).unwrap();
        store
            .save_snapshot_fingerprint("films", &hb_core::SnapshotFingerprint("fp".into()))
            .unwrap();
        store.save_published("films", "{}").unwrap();
        store.save_share_settings("films", &ShareSettings::default()).unwrap();
        store.save_settings(&Settings::default()).unwrap();
        let peer = CachedPeer {
            npub: "npub1x".into(),
            source: ContactSource::Manual,
            browse_key_hex: None,
            petname: None,
            profile: None,
            collections: vec![],
            listings_state: Default::default(), // QURATOR-134: fixtures predate the tri-state; Fetched is the least-wrong default
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        };
        store.save_contact(&CachedPeer::pubkey_hash("npub1x"), &peer).unwrap();
        store
            .save_groups(&[Group {
                name: "g".into(),
                pubkeys: vec![],
                modified_at: chrono::Utc::now(),
                color: None,
            }])
            .unwrap();
        store
            .save_watches(&[Watch {
                name: "w".into(),
                tags: vec![],
                content_types: vec![],
                last_fired: None,
                seen_pubkeys: vec![],
            }])
            .unwrap();
        let (meta, key) = hb_core::new_topic("private room", "", vec![], true).unwrap();
        store
            .save_topics(&[StoredTopic { meta, key, joined_at: 0, membership_json: None }])
            .unwrap();
        store.save_topic_nonces(&HashSet::from(["n1".to_string()])).unwrap();
        // M16 W4: the imported-manifest LRU cache lives under base/manifests/ — wipe must clear it too.
        crate::manifest_cache::put(
            &store.manifest_cache_dir(),
            "npub1x",
            "films",
            "fp",
            "ENV",
            1,
            crate::manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .unwrap();
        // A file the store does not know about yet — a future workstream's addition (chat
        // requests, topic announce timestamps, …) must be wiped too, never survive as an orphan.
        std::fs::write(store.base_dir().join("future_addition.json"), b"{}").unwrap();

        store.wipe().unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(store.base_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "wipe must leave the profile dir empty, found: {leftovers:?}");
    }

    #[test]
    fn published_marker_roundtrips() {
        let (_dir, store) = test_store();
        assert!(!store.is_published("films"));
        store.save_published("films", r#"{"id":"abc"}"#).unwrap();
        assert!(store.is_published("films"));
        assert_eq!(store.load_published("films").unwrap().as_deref(), Some(r#"{"id":"abc"}"#));
        store.delete_published("films").unwrap();
        assert!(!store.is_published("films"));
    }

    // ── Announcement-seen watermark (devtest #2) ────────────────────────────────────────────────

    #[test]
    fn announce_seen_defaults_empty_and_advances_by_max() {
        let (_dir, store) = test_store();
        assert!(store.load_announce_seen().unwrap().is_empty(), "no announce-seen state defaults to empty");

        store.advance_announce_seen("topic1", 500).unwrap();
        // An older ts must not rewind the watermark (would resurrect a phantom badge).
        store.advance_announce_seen("topic1", 300).unwrap();
        assert_eq!(store.load_announce_seen().unwrap().get("topic1").copied(), Some(500));

        // A newer ts advances it.
        store.advance_announce_seen("topic1", 900).unwrap();
        assert_eq!(store.load_announce_seen().unwrap().get("topic1").copied(), Some(900));
    }

    #[test]
    fn announce_seen_is_wiped_with_the_rest_of_the_profile() {
        let (_dir, store) = test_store();
        store.advance_announce_seen("topic1", 1).unwrap();
        assert!(store.announce_seen_path().exists());
        store.wipe().unwrap();
        assert!(!store.announce_seen_path().exists(), "announce_seen.json must be removed by wipe()");
    }

    // ── Read state (devtest #16) ────────────────────────────────────────────────────────────────

    #[test]
    fn read_state_defaults_empty_and_roundtrips() {
        let (_dir, store) = test_store();
        assert!(store.load_read_state().unwrap().is_empty(), "no read state yet defaults to empty");

        let mut m = std::collections::HashMap::new();
        m.insert("npub1a".to_string(), "2026-01-01T00:00:00Z".to_string());
        store.save_read_state(&m).unwrap();

        let loaded = store.load_read_state().unwrap();
        assert_eq!(loaded.get("npub1a").map(String::as_str), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn advance_read_watermark_takes_max_never_rewinds() {
        let (_dir, store) = test_store();
        store.advance_read_watermark("npub1a", "2026-01-05T00:00:00Z").unwrap();
        // An older timestamp must not rewind an already-advanced watermark.
        store.advance_read_watermark("npub1a", "2026-01-03T00:00:00Z").unwrap();
        let loaded = store.load_read_state().unwrap();
        assert_eq!(
            loaded.get("npub1a").map(String::as_str),
            Some("2026-01-05T00:00:00Z"),
            "an older ts must not rewind the watermark"
        );

        // A newer timestamp does advance it.
        store.advance_read_watermark("npub1a", "2026-01-09T00:00:00Z").unwrap();
        let loaded = store.load_read_state().unwrap();
        assert_eq!(loaded.get("npub1a").map(String::as_str), Some("2026-01-09T00:00:00Z"));
    }

    #[test]
    fn year_9999_sent_at_is_clamped_not_poisoning_the_watermark() {
        let (_dir, store) = test_store();
        store.advance_read_watermark("npub1a", "9999-01-01T00:00:00Z").unwrap();
        let loaded = store.load_read_state().unwrap();
        let stored = loaded.get("npub1a").expect("a clamped watermark is stored, not dropped");
        assert_ne!(stored, "9999-01-01T00:00:00Z", "the raw poison stamp must not be persisted");
        let stored_secs = parse_watermark_ts(stored).expect("stored watermark parses").timestamp();
        let ceiling_secs = read_watermark_ceiling(chrono::Utc::now()).timestamp();
        assert!(
            stored_secs <= ceiling_secs,
            "a year-9999 sent_at must clamp to now+skew (stored {stored_secs} > ceiling {ceiling_secs})"
        );
    }

    #[test]
    fn already_poisoned_watermark_self_heals_on_read_and_advance() {
        let (_dir, store) = test_store();
        // Simulate the attack having landed before this fix: a year-9999 watermark already on disk.
        let mut m = std::collections::HashMap::new();
        m.insert("npub1a".to_string(), "9999-01-01T00:00:00Z".to_string());
        store.save_read_state(&m).unwrap();

        // Read heals: the poisoned entry is dropped (reads as absent), not served as "everything read".
        let loaded = store.load_read_state().unwrap();
        assert!(
            !loaded.contains_key("npub1a"),
            "a poisoned watermark must read as absent so the badge can recover"
        );

        // The next legitimate advance persists the heal.
        store.advance_read_watermark("npub1a", "2026-01-05T00:00:00Z").unwrap();
        let healed = store.load_read_state().unwrap();
        assert_eq!(
            healed.get("npub1a").map(String::as_str),
            Some("2026-01-05T00:00:00Z"),
            "the poisoned watermark is replaced by the legitimate one on the next advance"
        );
    }

    #[test]
    fn unparseable_sent_at_is_rejected_not_stored() {
        let (_dir, store) = test_store();
        let res = store.advance_read_watermark("npub1a", "not-a-timestamp");
        assert!(res.is_err(), "an unparseable sent_at must be rejected, not stored");
        assert!(store.load_read_state().unwrap().is_empty(), "nothing persisted for the rejected ts");
    }

    #[test]
    fn concurrent_advances_never_rewind_the_watermark() {
        // Regression: a non-atomic load→max→save let an older-timestamp writer's save land last and
        // rewind the watermark (phantom unread badge). 8 threads race distinct, shuffled timestamps
        // for the SAME peer; the stored watermark must end up at the maximum regardless of interleaving.
        let (_dir, store) = test_store();
        let store = std::sync::Arc::new(store);
        let mut timestamps: Vec<String> =
            (0..8).map(|i| format!("2026-01-{:02}T00:00:00Z", i + 1)).collect();
        // Shuffle deterministically (no external rand dep needed) so threads don't race in order.
        timestamps.swap(0, 7);
        timestamps.swap(1, 5);
        timestamps.swap(2, 6);
        let max_ts = timestamps.iter().max().cloned().unwrap();

        let handles: Vec<_> = timestamps
            .into_iter()
            .map(|ts| {
                let store = std::sync::Arc::clone(&store);
                std::thread::spawn(move || {
                    store.advance_read_watermark("npub1contended", &ts).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let loaded = store.load_read_state().unwrap();
        assert_eq!(
            loaded.get("npub1contended").map(String::as_str),
            Some(max_ts.as_str()),
            "the watermark must land on the maximum timestamp regardless of thread interleaving"
        );
    }

    #[test]
    fn consume_then_concurrent_record_does_not_revert_the_consumption() {
        // M19 W9 regression: `record_issued_ticket` and `mark_ticket_consumed` each used to declare
        // their OWN `static ISSUED_TICKETS_LOCK` inside the function body — and a `static` inside a
        // function is its own distinct item, so the two never serialized against each other. Both do a
        // load→modify→save over the SAME `issued_tickets.json` map, so an interleaved approval (which
        // inserts a fresh record for its OWN request_id and prunes) and a redemption receipt (which
        // sets `consumed_at` on a DIFFERENT request_id) could each load the same stale map, apply
        // their own edit, and the save that lands second clobbers the other — reverting a just-written
        // `consumed_at` back to `None` (or dropping the newly-issued record). The hoisted module-level
        // `ISSUED_TICKETS_LOCK` makes the two RMWs serialize. This is the same defect class
        // `MANIFEST_ASKS_LOCK` already fixed for the ask-trace map.
        //
        // The serialization is FORCED, not hoped for. The main thread holds the shared lock, then
        // spawns a consume (for req-A) and an approval (for a DIFFERENT req-B); both must queue on
        // that lock. A shared `AtomicBool` proves the consume is actually blocked (it flips the flag
        // on entry, before blocking): under the fix the flag is set but the consume cannot finish
        // while the lock is held; under the old per-function statics the consume held a DIFFERENT
        // lock, never blocked, and finished before the main thread released. This is the asymmetric
        // rendezvous the M18 lesson demands: a concurrency test not forced to overlap is
        // timing-dependent. (The approval targets its own request_id, as in production where each
        // fulfilment mints a fresh id — so neither RMW overwrites the other's key; the only way an
        // edit is lost is the unserialized load→modify→save the lock now prevents.)
        use hb_core::TransportTicket;
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_dir, store) = test_store();
        let store = std::sync::Arc::new(store);

        // Pre-seed an unspent ticket (req-A) that the consume will mark spent.
        let ticket_a = TransportTicket::issue("req-A", "slug-a", "addr", 1_700_000_000, Some("n1"));
        store
            .record_issued_ticket(&IssuedTicketRecord {
                ticket: ticket_a.clone(),
                redeemer_npub: "npub1race".into(),
                consumed_at: None,
                delivered_bytes: None,
            })
            .unwrap();

        // Acquire the real shared lock on the main thread and hold it.
        let real_guard = ISSUED_TICKETS_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let consume_started = std::sync::Arc::new(AtomicBool::new(false));
        let store_consume = std::sync::Arc::clone(&store);
        let started_clone = std::sync::Arc::clone(&consume_started);
        let consume = std::thread::spawn(move || {
            // Signal we are running, THEN block on the lock `mark_ticket_consumed` needs.
            started_clone.store(true, Ordering::SeqCst);
            store_consume
                .mark_ticket_consumed("req-A", 4096, 1_700_000_100)
                .unwrap();
        });

        // The approval issues a DIFFERENT ticket (req-B) — as production does (new_request_id() each
        // fulfilment). Its edit touches a different key in the same map, so under the shared lock it
        // cannot lose the consume's write to req-A.
        let ticket_b = TransportTicket::issue("req-B", "slug-b", "addr", 1_700_000_000, Some("n2"));
        let store_record = std::sync::Arc::clone(&store);
        let record = std::thread::spawn(move || {
            store_record
                .record_issued_ticket(&IssuedTicketRecord {
                    ticket: ticket_b,
                    redeemer_npub: "npub1other".into(),
                    consumed_at: None,
                    delivered_bytes: None,
                })
                .unwrap();
        });

        // Wait for the consume thread to have started, then assert neither spawned thread could finish
        // while we hold the lock. Under the old per-function statics the consume ran to completion
        // immediately (it held a different lock) and this assertion would red.
        while !consume_started.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Brief grace so a blocked thread is definitely parked, not just unscheduled.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !consume.is_finished(),
            "mark_ticket_consumed must block on the shared ISSUED_TICKETS_LOCK while the main thread \
             holds it — if it finished, the two functions do not share a lock (M19 W9)"
        );
        assert!(
            !record.is_finished(),
            "record_issued_ticket must block on the same shared lock (M19 W9)"
        );

        // Release the lock: both queued threads proceed SERIALLY. Each loads the current map (with the
        // other's edit already applied, because they no longer interleave), applies its own edit, and
        // saves — so BOTH edits survive. Under the old per-function statics both could load the same
        // stale map and the save landing second would drop the other's edit (reverting consumed_at).
        drop(real_guard);

        record.join().unwrap();
        consume.join().unwrap();

        // Both edits must survive: req-A is consumed, req-B is present and unspent.
        let a = store.load_issued_ticket("req-A").unwrap().unwrap();
        assert_eq!(
            a.consumed_at,
            Some(1_700_000_100),
            "a concurrent approval must not revert a ticket's consumption (M19 W9)"
        );
        assert_eq!(a.delivered_bytes, Some(4096));
        let b = store.load_issued_ticket("req-B").unwrap().unwrap();
        assert_eq!(
            b.consumed_at, None,
            "the concurrently-issued ticket must also survive the serialized RMW"
        );
    }

    #[test]
    fn read_state_is_wiped_with_the_rest_of_the_profile() {
        let (_dir, store) = test_store();
        store.advance_read_watermark("npub1a", "2026-01-01T00:00:00Z").unwrap();
        assert!(store.read_state_path().exists());

        store.wipe().unwrap();
        assert!(!store.read_state_path().exists(), "read_state.json must be removed by wipe()");
    }

    // ── Manifest-request ask trace (M17 W7.1a) ———————————————————————————————————————
    // The ask leaves zero local trace without this record (send_dm_inner delivers to the recipient's
    // inbox only, no self-copy). Pinned: round-trip, overwrite-on-re-ask, lenient-absent-file, and
    // that the key disambiguates (npub, slug) pairs.

    #[test]
    fn manifest_asks_defaults_empty_on_absent_file() {
        // Lenient load: a missing file ⇒ empty map (not an error). Matches load_read_state.
        let (_dir, store) = test_store();
        assert!(!store.manifest_asks_path().exists());
        assert!(store.load_manifest_asks().unwrap().is_empty(), "absent file ⇒ empty map");
    }

    #[test]
    fn manifest_ask_roundtrips_and_is_keyed_by_npub_and_slug() {
        let (_dir, store) = test_store();
        store
            .record_manifest_ask("npub1a", "criterion", "fp-1", "2026-01-01T00:00:00Z", "nonce-1")
            .unwrap();
        // Same slug, different peer ⇒ distinct entry (don't clobber).
        store
            .record_manifest_ask("npub1b", "criterion", "fp-2", "2026-01-02T00:00:00Z", "nonce-x")
            .unwrap();
        // Same peer, different slug ⇒ distinct entry.
        store
            .record_manifest_ask("npub1a", "other", "fp-3", "2026-01-03T00:00:00Z", "nonce-x")
            .unwrap();
        let m = store.load_manifest_asks().unwrap();
        assert_eq!(m.len(), 3);
        let key_a = manifest_ask_key("npub1a", "criterion");
        assert_eq!(m[&key_a].fingerprint_seen, "fp-1");
        assert_eq!(m[&key_a].sent_at, "2026-01-01T00:00:00Z");
        assert_eq!(m[&manifest_ask_key("npub1b", "criterion")].sent_at, "2026-01-02T00:00:00Z");
        assert_eq!(m[&manifest_ask_key("npub1a", "other")].sent_at, "2026-01-03T00:00:00Z");
    }

    #[test]
    fn manifest_ask_overwrites_on_re_ask_for_same_pair() {
        // A re-ask is a re-ask: the newest send wins. One entry per (npub, slug), not a history.
        let (_dir, store) = test_store();
        store
            .record_manifest_ask("npub1a", "criterion", "fp-old", "2026-01-01T00:00:00Z", "nonce-x")
            .unwrap();
        store
            .record_manifest_ask("npub1a", "criterion", "fp-new", "2026-01-09T00:00:00Z", "nonce-x")
            .unwrap();
        let m = store.load_manifest_asks().unwrap();
        assert_eq!(m.len(), 1, "exactly one entry per (npub, slug)");
        let entry = &m[&manifest_ask_key("npub1a", "criterion")];
        assert_eq!(entry.fingerprint_seen, "fp-new");
        assert_eq!(entry.sent_at, "2026-01-09T00:00:00Z");
    }

    #[test]
    fn manifest_asks_is_wiped_with_the_rest_of_the_profile() {
        let (_dir, store) = test_store();
        store
            .record_manifest_ask("npub1a", "criterion", "fp-1", "2026-01-01T00:00:00Z", "nonce-1")
            .unwrap();
        assert!(store.manifest_asks_path().exists());

        store.wipe().unwrap();
        assert!(
            !store.manifest_asks_path().exists(),
            "manifest_asks.json must be removed by wipe()"
        );
    }
}
