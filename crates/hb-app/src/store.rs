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
    /// QURATOR-164 — the ONE opt-in switch for the swarm-caching tier. Covers two things at once
    /// (owner ruling 2026-09-04, "relay-caching is the same switch"): discovery-triggered
    /// auto-fetch of every public collection of a newly-surfaced peer, AND relay-caching
    /// (retaining data that passed through this node to a recipient). Opting in means holding far
    /// more and therefore being asked far more — the Settings copy must say so plainly. This is
    /// the switch tier only; the always-on baseline (serving what this node itself browsed) has
    /// NO switch. **Default false**: a pre-existing `settings.json` with no such key loads as
    /// `false` (bool serde default) — the intended silent opt-out, no migration.
    #[serde(default)]
    pub swarm_caching: bool,
    /// QURATOR-164 — the one-time baseline startup notice ("you pass along collections you have
    /// browsed, in the background") has been shown. A notice, NOT consent: there is no decline,
    /// and nothing is gated by it — shown iff this is false; acknowledging persists it.
    #[serde(default)]
    pub serving_notice_acknowledged: bool,
    /// QURATOR-208 — the **known-defaults watermark**: every default relay this install has
    /// already been offered. At startup `net::reconcile_default_relays` appends to `relay_urls`
    /// any current default that is absent here (a genuinely-new default the user has never seen),
    /// then sets this to the full current default list — so a default the user DELIBERATELY
    /// REMOVED stays removed across upgrades (it is in the watermark, so it is never "new" again),
    /// while an upgrade that ships new defaults still reaches existing installs. **Empty (the
    /// serde default for a pre-QURATOR-208 `settings.json`) means "never offered ANY default"**,
    /// which makes the first reconcile a one-time union with the current defaults: a pre-watermark
    /// install frozen on an old default set is un-stranded (the owner's v0.20.0 report: 2 relays
    /// shown where 4 ship), at the cost that a pre-watermark user who had deleted a then-default
    /// relay sees it return once — re-deletable, and the deletion sticks from then on, because the
    /// watermark is written in the same pass. That trade is deliberate: the alternative (treat an
    /// absent watermark as "already seen everything") permanently strands every pre-watermark
    /// install at its stale set, silently eroding the INV-5 floor, while the one-time resurrection
    /// is visible in Settings, INV-5-ward, and happens at most once per relay.
    #[serde(default)]
    pub known_default_relays: Vec<String>,
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
            swarm_caching: false,
            serving_notice_acknowledged: false,
            known_default_relays: Vec::new(),
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

    /// The single source of truth for a slug's per-collection local sidecars — every file a
    /// delete sweep removes, with the published-event marker returned separately because for a
    /// reserved marker slug that file is the LIVE profile teaser's marker, not the collection's
    /// (INV-8: the marker belongs to the profile teaser subsystem). Both delete variants below
    /// consume exactly this list, and the reserved-marker teardown in `commands/collection.rs`
    /// calls the sparing variant instead of hand-rolling its own path list.
    /// **A sixth store sidecar is a one-line addition to the Vec here** — no other production
    /// code needs editing.
    fn collection_sidecars(&self, slug: &str) -> (Vec<PathBuf>, PathBuf) {
        (
            vec![
                self.collection_draft_path(slug),
                self.share_settings_path(slug),
                self.scan_spec_path(slug),
                self.snapshot_fingerprint_path(slug),
            ],
            self.published_path(slug),
        )
    }

    /// Remove every local sidecar of a slug, including its published-event marker.
    pub fn delete_collection(&self, slug: &str) -> Result<()> {
        let (mut sidecars, published_marker) = self.collection_sidecars(slug);
        sidecars.push(published_marker);
        for path in &sidecars {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    /// [`Self::delete_collection`] SPARING the published-event marker: for a reserved marker
    /// slug that file is the live profile teaser's marker and must survive (INV-8). The sweep
    /// is otherwise identical — both variants consume [`Self::collection_sidecars`], so a sixth
    /// sidecar added there is swept here for free.
    pub fn delete_collection_sparing_published_marker(&self, slug: &str) -> Result<()> {
        let (sidecars, _published_marker) = self.collection_sidecars(slug);
        for path in &sidecars {
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
/// `"Sealed"` / `"FetchFailed"` / `"Pending"`); `FetchFailed`'s diagnostic reason is dropped at
/// this boundary (the UI only needs to know the load failed, not why — it renders error + Retry).
///
/// Four states, not three: `Pending` (QURATOR-332 slice A) is a locally-added state with no
/// `hb_net` counterpart — `From<hb_net::ListingsState>` never produces it.
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
    /// QURATOR-332 slice A — never enumerated yet; the background queue or a click will classify
    /// it — never a confident negative. Stamped only by the add-time stub paths (topic join,
    /// request accept) and the one-time heal; `From<hb_net::ListingsState>` never produces it.
    Pending,
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

/// Serializes every load-modify-save of `groups.json` (QURATOR-252).
///
/// **One shared static, not one per function** — the M19 W9 lesson [`MANIFEST_ASKS_LOCK`]
/// documents. Eight production call sites mutate this file (`groups_create`,
/// `groups_create_with_members`, `groups_rename`, `groups_delete`, `groups_assign`,
/// `groups_unassign`, `contact_update_groups`, and browse.rs's follow-into-group helper), so a
/// function-local static would mint one lock per function and the race would survive. Every
/// mutator must route through [`DataStore::mutate_groups`], which holds this lock across the
/// WHOLE load→mutate→save sequence — locking `load_groups`/`save_groups` individually fixes
/// nothing, because the race is the sequence, not either call. `std::sync::Mutex` is not
/// reentrant: nothing inside this critical section may call back into `mutate_groups`.
static GROUPS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes every load-modify-save of `private_audience.json` (QURATOR-252). Same shape and
/// same one-shared-static reasoning as [`GROUPS_LOCK`]; the lock is taken for the whole
/// load→mutate→save inside [`DataStore::set_private_audience_member`] so a revocation racing a
/// concurrent enrolment can no longer be clobbered. Deliberately a SEPARATE lock from
/// [`GROUPS_LOCK`]: the audience is explicit and never group-derived (owner ruling 2026-08-04,
/// pinned by `group_mutations_do_not_touch_private_audience`), so a group mutation must never
/// share a critical section with an audience write.
static PRIVATE_AUDIENCE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// QURATOR-252 — one atomic load→mutate→save of `groups.json` under [`GROUPS_LOCK`], the
    /// group-mutating twin of `note_failed_dial`'s locked read-modify-write. The mutation runs
    /// only if the load succeeded, and the file is saved only when the closure returned `Ok` — a
    /// refused mutation ("group not found", "already exists") writes NOTHING, preserving the
    /// byte-identical refusal behaviour the command tests pin. `f` must not call back into this
    /// method: `std::sync::Mutex` is not reentrant.
    pub fn mutate_groups<T>(
        &self,
        f: impl FnOnce(&mut Vec<Group>) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = GROUPS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut groups = self.load_groups().map_err(|e| e.to_string())?;
        let out = f(&mut groups)?;
        self.save_groups(&groups).map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// QURATOR-252 — enrol or revoke one npub in the Private-collection audience as a single
    /// load→mutate→save under [`PRIVATE_AUDIENCE_LOCK`]. Idempotent in both directions (the
    /// command's documented contract). Locking the whole sequence is what fixes the ticket's
    /// race: before, a revocation racing a concurrent enrolment interleaved two unlocked
    /// load→mutate→save cycles and whichever saved second erased the other's change. The command
    /// layer (`private_audience_set`) is a thin shim over this.
    pub fn set_private_audience_member(&self, npub: &str, receives: bool) -> Result<()> {
        let _guard = PRIVATE_AUDIENCE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut audience = self.load_private_audience()?;
        if receives {
            if !audience.iter().any(|n| n == npub) {
                audience.push(npub.to_string());
            }
        } else {
            audience.retain(|n| n != npub);
        }
        self.save_private_audience(&audience)
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
/// Also the fetch driver's QURATOR-197 `since` anchor parse.
pub(crate) fn parse_watermark_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
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

    /// Where the auto-approve loop remembers which asks it has already answered (QURATOR-184).
    pub fn answered_asks_path(&self) -> PathBuf {
        self.base.join("answered_asks.json")
    }

    /// Load the answered-ask dedup set.
    ///
    /// Missing or unreadable ⇒ empty, never an error: this is a de-duplication memory, and losing
    /// it costs redundant work, never correctness. A re-answered ask is refused by the asker's own
    /// claim gate as `Spent` or nonce-mismatched.
    pub fn load_answered_asks(&self) -> Result<std::collections::HashSet<String>> {
        Ok(read_json_lenient::<std::collections::HashSet<String>>(&self.answered_asks_path())
            .context("loading answered asks")?
            .unwrap_or_default())
    }

    /// Persist the answered-ask dedup set.
    ///
    /// ⚠ The caller MUST keep this bounded. Every key carries an attacker-chosen nonce, so an
    /// unbounded set is a durable disk-growth vector a peer can drive — which is exactly why the
    /// in-memory set was capped and cleared rather than grown. Persisting it does not relax that;
    /// see `auto_approve::remember_answered`.
    pub fn save_answered_asks(&self, seen: &std::collections::HashSet<String>) -> Result<()> {
        write_json(&self.answered_asks_path(), seen).context("saving answered asks")
    }

    pub fn load_manifest_asks(&self) -> Result<std::collections::HashMap<String, ManifestAsk>> {
        let mut m = read_json_lenient::<std::collections::HashMap<String, ManifestAsk>>(
            &self.manifest_asks_path(),
        )
        .context("loading manifest asks")?
        .unwrap_or_default();
        // Lenient load (Carrier 4): an ask map written by an older build carries 2-segment
        // `{npub}|{slug}` keys. Every such ask was by construction a self-ask — the only kind that
        // existed — so widen it in memory to `{npub}|{npub}|{slug}`. The rewrite is not persisted
        // here (a pure read must stay a pure read); the next `record`/`claim`/`spend` write saves
        // the widened map, so the migration converges without a dedicated pass.
        if m.keys().any(|k| k.matches('|').count() == 1) {
            m = m
                .into_iter()
                .map(|(k, v)| (widen_legacy_ask_key(&k).unwrap_or(k), v))
                .collect();
        }
        Ok(m)
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
        author: &str,
        slug: &str,
        fingerprint_seen: &str,
        sent_at: &str,
        nonce: &str,
    ) -> Result<()> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        m.insert(
            manifest_ask_key(npub, author, slug),
            ManifestAsk {
                fingerprint_seen: fingerprint_seen.to_string(),
                sent_at: sent_at.to_string(),
                nonce: nonce.to_string(),
                // A re-ask is a fresh authorization: new nonce, so any prior claim or spent flag
                // must be cleared with it — and a fresh DIAL BUDGET (QURATOR-197): renewal of the
                // authorization renews the retries it pays for.
                claimed_by: None,
                spent: false,
                dial_attempts: 0,
                dial_last_fail_unix: 0,
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
        author: &str,
        slug: &str,
        nonce: &str,
        request_id: &str,
    ) -> Result<AskClaim> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        let key = manifest_ask_key(npub, author, slug);
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
    pub fn spend_manifest_ask(
        &self,
        npub: &str,
        author: &str,
        slug: &str,
        expected_nonce: &str,
    ) -> Result<()> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        if let Some(ask) = m.get_mut(&manifest_ask_key(npub, author, slug)) {
            if ask.nonce == expected_nonce {
                ask.spent = true;
                self.save_manifest_asks(&m)?;
            }
        }
        Ok(())
    }

    /// QURATOR-197 (F19) — record one FAILED DIAL against the ask `(npub, author, slug)`, so the
    /// fetch driver's redeem poll backs off instead of re-dialling the same ticket every 300 s
    /// forever. The companion gate lives in `fetch_driver::redeem_dial_ready`.
    ///
    /// **Counts only failures that actually reached a dial.** The caller cannot tell a post-claim
    /// failure (endpoint/fetch/accept — a real dial attempt) from a pre-claim refusal
    /// (Unsolicited/Spent/ClaimedByAnother — no dial happened), so the discriminator is read here,
    /// from the durable state `claim_manifest_ask` itself wrote, rather than re-derived at the
    /// call site: `claimed_by == request_id` can only ever have been written by a **Granted**
    /// claim of this exact ticket (the claim sets it after the nonce and spent checks passed), and
    /// a spent ask is terminal. A refusal therefore lands here as a silent no-op — a stale-nonce
    /// wrap replayed by the relay every poll must not burn the CURRENT ask's dial budget.
    ///
    /// Silently `Ok(())` when there is nothing to count (missing ask, unclaimed, someone else's
    /// ticket, spent): this is bookkeeping for a backoff, never a failure the caller should surface.
    pub fn note_failed_dial(
        &self,
        npub: &str,
        author: &str,
        slug: &str,
        request_id: &str,
    ) -> Result<()> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        let Some(ask) = m.get_mut(&manifest_ask_key(npub, author, slug)) else { return Ok(()) };
        if ask.spent || ask.claimed_by.as_deref() != Some(request_id) {
            return Ok(());
        }
        ask.dial_attempts += 1;
        ask.dial_last_fail_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.save_manifest_asks(&m)
    }

    /// QURATOR-250 — drop ask-trace entries past [`MANIFEST_ASK_RETENTION_SECS`], returning how
    /// many went. Runs once per fetch-driver poll, under [`MANIFEST_ASKS_LOCK`] like every
    /// load-modify-save of this map.
    ///
    /// **Eviction forgets bookkeeping; it refuses nothing.** A fresh ask records and claims
    /// identically with the dead entries gone (pinned by `eviction_never_refuses_a_fresh_ask`),
    /// and a ticket answering an expired ask meets the same `Unsolicited` refusal a spent or
    /// superseded-nonce ask already met — the redeem allow-list is a security boundary, and an
    /// entry the retention policy has declared dead no longer vouches for a dial.
    ///
    /// Saves only when something was actually evicted, so a quiet map is not rewritten every
    /// 300 s; a failed load surfaces as an error the caller logs, never a failed poll (the next
    /// tick retries).
    pub fn evict_expired_manifest_asks(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        let before = m.len();
        m.retain(|_, ask| !manifest_ask_is_expired(ask, now));
        let removed = before - m.len();
        if removed > 0 {
            self.save_manifest_asks(&m)?;
        }
        Ok(removed)
    }

    /// QURATOR-293 — LIVENESS eviction: drop this author's ask-trace entries whose slug the
    /// CURRENT listing (`listed`) no longer carries, returning how many went. The companion of
    /// [`evict_expired_manifest_asks`]: the 30-day window only bounds slugs the peer REMOVED (and
    /// ex-contacts), eventually — this makes the removal take effect on the next discovery poll,
    /// so a rotating malicious listing cannot hold a 30-day tail of dead `peer|author|slug` rows.
    ///
    /// ⚠ The caller must pass a listing that actually ARRIVED, never one a failed or undecryptable
    /// enumeration reconciled out of the cache — the pure core
    /// ([`manifest_ask_is_unlisted`]) cannot see that, and evicting on a hollow listing deletes
    /// LIVE asks. `discover_unheld` gates on `ListingsStatus::Fetched` for exactly that reason.
    ///
    /// Same forget-never-refuse contract as the retention sweep: eviction only shortens the trace;
    /// a fresh ask records and claims identically with the dead entries gone. Under
    /// [`MANIFEST_ASKS_LOCK`] like every load-modify-save of this map, and saves only when
    /// something was actually evicted.
    pub fn evict_unlisted_manifest_asks(
        &self,
        author: &str,
        listed: &std::collections::HashSet<String>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize> {
        let _guard = MANIFEST_ASKS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load_manifest_asks()?;
        let before = m.len();
        m.retain(|k, ask| !manifest_ask_is_unlisted(k, ask, author, listed, now));
        let removed = before - m.len();
        if removed > 0 {
            self.save_manifest_asks(&m)?;
        }
        Ok(removed)
    }

    // ── Issued transport tickets (M18 W4) — DELETED 2026-09-03, QURATOR-177 Option E (owner
    //    ruling: authorization is the standing grant checked at ASK time; the ticket is address
    //    delivery). This section was `issued_tickets_path`/`load_issued_tickets`/
    //    `save_issued_tickets`/`load_issued_ticket`/`record_issued_ticket`/`mark_ticket_consumed`
    //    — the issued-ticket ledger that answered "what did we mint for this request, and is it
    //    spent?" on the serve path. Durable replay protection and the audit trail were
    //    deliberately given up with it; a stale `issued_tickets.json` on an upgrading install is
    //    dead data (nothing reads it; `wipe()` already removes the whole directory).


    // ── standing grants: DELETED 2026-09-04, QURATOR-164 ────────────────────────────────────
    // `standing_grants_path` / `load_standing_grants` / `save_standing_grants` /
    // `record_standing_grant` / `standing_grant_for`, and the `standing_grants.json` map they held,
    // are gone. Owner ruling: *"There's no approval needed for public collections, thats why they
    // are called public."*
    //
    // The map recorded "a human approved serving this (peer, author, slug)" so a later ask could
    // skip the human. Every path it guarded serves PUBLIC bytes only — `build_slug_manifest`
    // refuses a private collection outright, and private listings go through `priv_listing.rs`'s
    // per-recipient CEK wrap, which never reaches here — so there was nothing to authorise.
    //
    // Its three non-authorisation jobs were rehomed rather than lost: the auto-approve loop's
    // per-pair rate key became `auto_approve::pace_key` (a rate-limiting concern, not a permission
    // one); the startup rebind now asks `transport_state::has_servable_content` ("do I hold
    // anything?") instead of "did I ever approve?"; and the chat card's `has_standing_grant`
    // display shim is deleted with the concept.
    //
    // ⚠ On-disk `standing_grants.json` files are simply ignored from now on — INV-8 makes deletion
    // deliberate, so nothing removes them; they are inert.

}

// `ISSUED_TICKETS_LOCK` — DELETED 2026-09-03, QURATOR-177 Option E, with the issued-ticket map it
// serialized. The M19 W9 lesson it embodied (hoist the RMW lock to ONE shared static, never one
// per function — `MANIFEST_ASKS_LOCK` and `STANDING_GRANTS_LOCK` below still enforce it) outlives
// the map.

/// Serializes every load-modify-save of the ask trace.
///
/// **One shared static, not one per function.** A `static` declared inside a function body is its own
/// distinct item, so the two locks this replaced never serialized against *each other* — a
/// `record_manifest_ask` and a `claim_manifest_ask` overlapping could interleave their
/// load→modify→save and lose the newer write entirely.
static MANIFEST_ASKS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());



// `IssuedTicketRecord` (ticket verbatim, `redeemer_npub`, `consumed_at`, `delivered_bytes`,
// `served_fingerprint`) — DELETED 2026-09-03, QURATOR-177 Option E, with the
// `issued_tickets.json` map that held it. Its two surviving consumers moved elsewhere at the same
// time: the grant keying `redeemer_npub` fed is now keyed inline by `fulfil.rs`
// (`record_standing_grant`), and the Carrier-4 branch `served_fingerprint` once discriminated is
// now the ticket's own `author_npub` (`manifest_source.rs`). The audit trail is deliberately gone.

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
    /// QURATOR-197 (F19) — failed redemption DIALS recorded against this ask. The fetch driver's
    /// 300 s poll re-reads the wrap from the relay every tick and, before these two fields, a
    /// permanently-failing ticket was re-dialled forever: `claim_manifest_ask` re-grants the same
    /// `request_id` and only a SUCCESS spends. Mirrors `peer_wave`'s per-candidate convention
    /// (attempts + last-attempt + exponential backoff + a small cap) — see
    /// `fetch_driver::redeem_dial_ready`.
    ///
    /// **Persisted, not in-memory like the driver's `AskState`, deliberately**: both halves of the
    /// loop survive a restart — this ask on disk, the wrap on the relay — so a process-lifetime
    /// counter would reset exactly when the storm does not. Persisted-with-a-bound, the
    /// `answered_asks` precedent: no new map entries (a field on a row the user's own ask
    /// created), the gate stops incrementing once the cap is hit, and a fresh ask
    /// (`record_manifest_ask`) resets both — so it can grow in neither entries nor value.
    #[serde(default)]
    pub dial_attempts: u32,
    /// Unix seconds of the most recent failed dial (`0` = never) — wall clock, not `Instant`,
    /// because the backoff must hold across a restart.
    #[serde(default)]
    pub dial_last_fail_unix: u64,
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

/// The on-disk key for an ask trace: `"{npub}|{author}|{slug}"` — who we asked, **which author's
/// collection we asked about** (Carrier 4: a re-serve ask names a third-party author; a self-ask is
/// spelled `author == npub`), and the slug. The pipe is unambiguous because npubs and slugs never
/// contain `|` (bech32 / URL-safe slug charset).
pub fn manifest_ask_key(npub: &str, author: &str, slug: &str) -> String {
    format!("{npub}|{author}|{slug}")
}

/// Widen a pre-Carrier-4 2-segment key `"{npub}|{slug}"` to the self-ask spelling
/// `"{npub}|{npub}|{slug}"` — all an authorless ask could ever have meant was the asked peer's own
/// collection. Any other segment count is left as-is (never happens for well-formed keys).
fn widen_legacy_ask_key(key: &str) -> Option<String> {
    let parts: Vec<&str> = key.split('|').collect();
    (parts.len() == 2).then(|| format!("{}|{}|{}", parts[0], parts[0], parts[1]))
}

/// QURATOR-250 — how long an ask-trace entry stays authoritative: 30 days.
///
/// Sized against what the entry still does for the machine. A ticket answering an ask lands in
/// the redeem poll (300 s cadence), and the dial machinery an entry authorises is terminal
/// within hours — the 3-dial cap's exponential backoff from a 600 s base exhausts itself in
/// roughly 70 minutes (QURATOR-197). A month is two orders of magnitude beyond that, so nothing
/// the loops still rely on is inside the cut, while the Browse UI's "Asked" trace keeps a
/// readable age. The other half of the sizing: every re-ask OVERWRITES the entry
/// (`record_manifest_ask` resets `sent_at`), and both driver tiers re-ask for as long as their
/// condition holds — so an entry the loops still want never ages past the window; only an ask
/// NOTHING has re-sent for a whole month expires.
///
/// ⚠ This is eviction, never a throughput cap (2026-09-02 owner ruling: prefetch takes no caps).
/// Forgetting an expired entry can refuse ONE very-late ticket (`Unsolicited`) and cost a
/// re-ask, both bounded and delay-shaped — it never skips an ask, drops a listing, or limits how
/// many asks a poll may process.
pub(crate) const MANIFEST_ASK_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

/// QURATOR-250 — pure core: is this ask-trace entry past retention at `now`?
///
/// An undateable `sent_at` is NOT expired, deliberately. The stamp is minted by THIS node at
/// record time (`chrono::Utc::now().to_rfc3339()` at every producer), never peer-supplied, so a
/// stamp that will not parse is disk corruption rather than an attack — and deleting state we
/// cannot date is the wrong side to fail on. Such an entry costs one row until the next re-ask
/// overwrites it with a fresh stamp.
pub(crate) fn manifest_ask_is_expired(
    ask: &ManifestAsk,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    match parse_watermark_ts(&ask.sent_at) {
        Some(sent) => {
            now.timestamp().saturating_sub(sent.timestamp()) >= MANIFEST_ASK_RETENTION_SECS as i64
        }
        None => false,
    }
}

/// QURATOR-293 — how long an ask-trace entry is immune to LIVENESS eviction after it was sent:
/// 1 hour.
///
/// An ask sent a moment ago may simply not have been answered yet — the peer answers in seconds
/// but our redeem poll only reads the inbox every 300 s — and a poll that races it must not
/// delete the authorization the reply will need (a manual ask loses its auto-dial otherwise).
/// One hour is two orders of magnitude past that ask→answer→redeem cycle, mirroring how the
/// 30-day window was sized against the dial machinery it authorises. The grace only DELAYS the
/// eviction of dead entries; it never rescues one, because a listed slug's entry keeps being
/// re-stamped by the re-ask and an unlisted one stops being asked the moment it drops out.
pub(crate) const MANIFEST_ASK_LIVENESS_GRACE_SECS: u64 = 60 * 60;

/// QURATOR-293 — pure core of liveness eviction: is this ask-trace entry dead because `author`'s
/// CURRENT listing no longer carries its slug?
///
/// Attribution is by the key's AUTHOR segment — `{npub}|{author}|{slug}`, so one author's poll
/// touches exactly that author's entries (self-asks `author == npub` AND Carrier-4 asks a third
/// peer was asked about this author's collection): a slug-name collision with another author
/// ("films") must never evict the other author's entry. A key that does not split into exactly
/// three segments is not ours to interpret and is kept (the undateable-stamp discipline).
///
/// Two in-flight protections, both DELAY-shaped:
/// - an UNSPENT entry a ticket has CLAIMED may still be inside the QURATOR-197 dial backoff
///   (terminal ~70 min after the claim); it is exempt here but NOT from the 30-day window, so
///   the exemption cannot make an entry immortal. A SPENT entry is terminal — it awaits nothing.
/// - an entry inside [`MANIFEST_ASK_LIVENESS_GRACE_SECS`] of its `sent_at` may still be awaiting
///   its first answer. An undateable `sent_at` is kept, for the same reason as in
///   [`manifest_ask_is_expired`].
///
/// Like the retention sweep this forgets bookkeeping and refuses nothing: a very-late ticket
/// answering a forgotten entry meets the same `Unsolicited` a spent or expired ask already met.
pub(crate) fn manifest_ask_is_unlisted(
    key: &str,
    ask: &ManifestAsk,
    author: &str,
    listed: &std::collections::HashSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let segments: Vec<&str> = key.split('|').collect();
    if segments.len() != 3 || segments[1] != author {
        return false;
    }
    let slug = segments[2];
    if listed.contains(slug) {
        return false;
    }
    if !ask.spent && ask.claimed_by.is_some() {
        return false;
    }
    match parse_watermark_ts(&ask.sent_at) {
        Some(sent) => {
            now.timestamp().saturating_sub(sent.timestamp()) >= MANIFEST_ASK_LIVENESS_GRACE_SECS as i64
        }
        None => false,
    }
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

    pub fn dead_topic_verdicts_path(&self) -> PathBuf {
        self.base.join("dead_topic_verdicts.json")
    }

    /// The persisted **known-dead public Topic verdicts** (QURATOR-192): topic_id → unix-secs the
    /// verdict was stamped. Only a CONFIDENT `alive_count == Some(0)` may ever land here — the gate
    /// is `commands/topics.rs`'s `dead_verdict_warranted` — never `None` (unknown: no key / private
    /// / relay error), which would bury a live Topic permanently. A verdict is honoured for one
    /// aliveness window (`hb_net::count::TOPIC_ALIVE_WINDOW_SECS`) so a revived Topic reappears.
    /// Device-local by design.
    pub fn load_dead_topic_verdicts(&self) -> Result<std::collections::HashMap<String, u64>> {
        Ok(
            read_json_lenient::<std::collections::HashMap<String, u64>>(&self.dead_topic_verdicts_path())
                .context("loading dead topic verdicts")?
                .unwrap_or_default(),
        )
    }

    pub fn save_dead_topic_verdicts(&self, verdicts: &std::collections::HashMap<String, u64>) -> Result<()> {
        write_json(&self.dead_topic_verdicts_path(), verdicts).context("saving dead topic verdicts")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    /// Persists a deliberately-unroutable relay set (QURATOR-179 slice 2) on an *existing* store.
    /// `relay_urls()` (see `net.rs`) falls back to the four REAL public `DEFAULT_RELAYS` whenever a
    /// store's configured set is empty, so any test that reaches relay I/O through a plain store
    /// silently targets the live internet. Call this on a store instead: its non-empty sentinel set
    /// means `relay_urls()` returns the sentinel, not the defaults, so a test that tries to dial out
    /// fails loudly against a host that cannot resolve rather than succeeding slowly against a real
    /// relay. `.invalid` is the RFC 2606 reserved TLD guaranteed to never resolve. Exposed for
    /// callers (e.g. `commands/settings.rs`'s `guard_app()`) that must build the store directly
    /// because they need its owning `TempDir` to outlive this function's return.
    pub(crate) fn pin_unroutable_relays(store: &DataStore) {
        store
            .save_settings(&Settings {
                relay_urls: vec!["wss://hoardbook-test-sentinel.invalid".to_string()],
                ..Default::default()
            })
            .unwrap();
    }

    /// [`test_store`], but with [`pin_unroutable_relays`] already applied.
    pub(crate) fn test_store_unroutable_relays() -> (TempDir, DataStore) {
        let (dir, store) = test_store();
        pin_unroutable_relays(&store);
        (dir, store)
    }

    /// QURATOR-252 mechanism pin — the private-audience read-modify-write runs under
    /// [`PRIVATE_AUDIENCE_LOCK`] (one shared module-scope static, the M19 W9 lesson
    /// [`MANIFEST_ASKS_LOCK`] documents; exactly one mutator path today — the
    /// `private_audience_set` command — but the lock guards the store method, so every current
    /// or future caller is covered by construction). This is the DETERMINISTIC half of the race
    /// proof: with the lock held by this thread, a `set_private_audience_member` — even on a
    /// different `DataStore`, the lock is module-scope and shared across instances — must not
    /// complete until the lock is released; then it must complete and land. The behavioural
    /// twin (real threads racing enrol vs revoke) is
    /// `private_audience_revocation_survives_a_concurrent_enrolment` in commands/groups.rs.
    /// Honest limit: the "blocked" half observes a 50 ms window — a worker thread not yet
    /// scheduled inside it could let the first assert pass vacuously; the release half
    /// (completes + lands) pins the method's correctness regardless.
    /// P-10: in `set_private_audience_member`, delete the guard line
    /// `let _guard = PRIVATE_AUDIENCE_LOCK.lock().unwrap_or_else(|p| p.into_inner());`
    /// (store.rs:1020 at the time of writing) — the `!completed` assert must go red because the
    /// write lands while the lock is held.
    #[test]
    fn set_private_audience_member_blocks_while_the_lock_is_held() {
        let (dir, store) = test_store();
        let _keep = dir;
        let store = std::sync::Arc::new(store);
        let worker_store = store.clone();
        let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_completed = completed.clone();

        let hold = PRIVATE_AUDIENCE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let worker = std::thread::spawn(move || {
            worker_store.set_private_audience_member("npub_x", true).unwrap();
            worker_completed.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !completed.load(std::sync::atomic::Ordering::SeqCst),
            "the audience write must not complete while PRIVATE_AUDIENCE_LOCK is held elsewhere"
        );
        drop(hold);
        worker.join().unwrap();
        assert!(
            completed.load(std::sync::atomic::Ordering::SeqCst),
            "the write completes once the lock is released"
        );
        assert_eq!(
            store.load_private_audience().unwrap(),
            vec!["npub_x".to_string()],
            "the blocked write landed after release"
        );
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
        // QURATOR-164: a pre-QURATOR-164 file with neither new key loads both as false — the
        // switch tier off, and the baseline startup notice still owed.
        // MUTATION (P-10): on the `pub swarm_caching: bool,` field in the `Settings` struct
        // above, change `#[serde(default)]` to `#[serde(default = "default_true")]` — this
        // assert reds (an old file would then load the switch as ON, silently opting every
        // existing user into the heavier tier).
        assert!(!s.swarm_caching, "swarm_caching defaults OFF on an old file (opt-in only)");
        assert!(
            !s.serving_notice_acknowledged,
            "serving_notice_acknowledged defaults OFF on an old file (notice still owed)"
        );
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
            swarm_caching: true,
            serving_notice_acknowledged: true,
            known_default_relays: vec!["wss://offered.example".into()],
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
        // MUTATION (P-10): on the `pub swarm_caching: bool,` field in the `Settings` struct
        // above, change `#[serde(default)]` to `#[serde(skip_serializing)]` — this assert reds
        // (the save would drop the field, and the reload would read the `false` default).
        assert!(r.swarm_caching, "swarm_caching preserved");
        assert!(r.serving_notice_acknowledged, "serving_notice_acknowledged preserved");
        // MUTATION (P-10): on the `pub known_default_relays: Vec<String>,` field in the
        // `Settings` struct above, change `#[serde(default)]` to `#[serde(skip_serializing)]` —
        // this assert reds (the save would drop the QURATOR-208 watermark, and the reload would
        // read the empty serde default, un-stranding nothing and resurrecting every upgrade).
        assert_eq!(
            r.known_default_relays,
            vec!["wss://offered.example".to_string()],
            "the QURATOR-208 known-defaults watermark round-trips (a dropped field would reset it)"
        );
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

    /// QURATOR-288 fixture: materialise every per-slug sidecar the delete sweeps cover, via the
    /// store's own save methods so the paths cannot drift from the real ones.
    fn materialise_every_sidecar(store: &DataStore) {
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
        store.save_published("films", "{}").unwrap();
        store.save_share_settings("films", &ShareSettings::default()).unwrap();
        store.save_scan_spec("films", &ScanSpec::default()).unwrap();
        store
            .save_snapshot_fingerprint("films", &hb_core::SnapshotFingerprint("fp".into()))
            .unwrap();
    }

    /// QURATOR-288: `delete_collection` itself had zero direct coverage — its sweep was only
    /// pinned obliquely through the command layer. Materialise all five sidecars and assert all
    /// five are gone.
    ///
    /// Mutation to redden (orchestrator applies on the settled tree; a Rust lane does not run
    /// it — a filtered `cargo test -p hb-app` still compiles the whole crate and collides on
    /// CARGO_TARGET_DIR): in `collection_sidecars` (the private `fn` immediately above
    /// `delete_collection`, whose body is the tuple of a four-entry `vec![` and
    /// `self.published_path(slug)`), delete the `self.share_settings_path(slug),` entry INSIDE
    /// that `vec![` — resolve the line number in that production region; the same text appears
    /// in this comment, so an unqualified text match would mutate the comment and report a good
    /// control as decorative. The `share_settings_path` assert below reds.
    #[test]
    fn delete_collection_removes_every_sidecar_including_the_published_marker() {
        let (_dir, store) = test_store();
        materialise_every_sidecar(&store);
        // Precondition: all five exist, so a silently failing save cannot vacuously green the
        // absence asserts below.
        for present in [
            store.collection_draft_path("films"),
            store.published_path("films"),
            store.share_settings_path("films"),
            store.scan_spec_path("films"),
            store.snapshot_fingerprint_path("films"),
        ] {
            assert!(present.exists(), "precondition: {} must exist", present.display());
        }

        store.delete_collection("films").unwrap();

        for gone in [
            store.collection_draft_path("films"),
            store.published_path("films"),
            store.share_settings_path("films"),
            store.scan_spec_path("films"),
            store.snapshot_fingerprint_path("films"),
        ] {
            assert!(
                !gone.exists(),
                "delete_collection must sweep {} — an orphaned sidecar keeps \
                 target_path_is_occupied (backup.rs) true and blocks future restores",
                gone.display()
            );
        }
    }

    /// QURATOR-288: the spare-the-marker variant the reserved-marker teardown in
    /// `commands/collection.rs` (QURATOR-249) calls instead of hand-rolling four paths. Same
    /// five-sidecar fixture; exactly the published marker must survive.
    ///
    /// Mutation to redden (orchestrator applies; not run here): in
    /// `delete_collection_sparing_published_marker` (the `pub fn` immediately after
    /// `delete_collection`), replace the whole body — the
    /// `let (sidecars, _published_marker) = self.collection_sidecars(slug);` destructure plus
    /// its `for` loop — with a plain delegation to `self.delete_collection(slug)`, making the
    /// sparing variant sweep the marker too. The marker-survival assert below reds while the
    /// sibling `delete_collection_removes_every_sidecar_including_the_published_marker` stays
    /// green (one mutation, one test — attributable).
    #[test]
    fn delete_collection_sparing_published_marker_removes_every_sidecar_but_the_marker() {
        let (_dir, store) = test_store();
        materialise_every_sidecar(&store);
        for present in [
            store.collection_draft_path("films"),
            store.published_path("films"),
            store.share_settings_path("films"),
            store.scan_spec_path("films"),
            store.snapshot_fingerprint_path("films"),
        ] {
            assert!(present.exists(), "precondition: {} must exist", present.display());
        }

        store.delete_collection_sparing_published_marker("films").unwrap();

        assert!(
            store.published_path("films").exists(),
            "INV-8: the sparing variant must spare the published marker — for a reserved \
             marker slug it is the live profile teaser's marker, not the collection's"
        );
        for gone in [
            store.collection_draft_path("films"),
            store.share_settings_path("films"),
            store.scan_spec_path("films"),
            store.snapshot_fingerprint_path("films"),
        ] {
            assert!(
                !gone.exists(),
                "the sparing variant must still sweep {} — an orphaned sidecar keeps \
                 target_path_is_occupied (backup.rs) true and blocks future restores",
                gone.display()
            );
        }
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

    // ── Known-dead public Topic verdicts (QURATOR-192) ──────────────────────────────────────────

    #[test]
    fn dead_topic_verdicts_default_empty_and_roundtrip() {
        // P-10 MUTATION (orchestrator): in `save_dead_topic_verdicts` (the containing fn), replace
        // the `verdicts` argument with `&std::collections::HashMap::new()` — the round-trip assert
        // below reds (an empty map comes back instead of the saved one). The sibling defaults
        // assert stays green either way, proving the round-trip is the pinned half.
        let (_dir, store) = test_store();
        assert!(
            store.load_dead_topic_verdicts().unwrap().is_empty(),
            "no verdicts yet defaults to empty, never an error"
        );

        let verdicts =
            std::collections::HashMap::from([("films".to_string(), 1_234_u64), ("games".to_string(), 5_678_u64)]);
        store.save_dead_topic_verdicts(&verdicts).unwrap();
        assert_eq!(
            store.load_dead_topic_verdicts().unwrap(),
            verdicts,
            "the stamped verdict map round-trips through dead_topic_verdicts.json"
        );
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

    // `consume_then_concurrent_record_does_not_revert_the_consumption` (the M19 W9 regression
    // test) — DELETED 2026-09-03, QURATOR-177 Option E: it pinned that `record_issued_ticket` and
    // `mark_ticket_consumed` serialized on ONE shared `ISSUED_TICKETS_LOCK`, and both functions
    // are deleted with the ledger. The lock-hoisting lesson it taught is still enforced by
    // `MANIFEST_ASKS_LOCK` and `STANDING_GRANTS_LOCK` (whose own tests remain).

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
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp-1", "2026-01-01T00:00:00Z", "nonce-1")
            .unwrap();
        // Same slug, different peer ⇒ distinct entry (don't clobber).
        store
            .record_manifest_ask("npub1b", "npub1b", "criterion", "fp-2", "2026-01-02T00:00:00Z", "nonce-x")
            .unwrap();
        // Same peer, different slug ⇒ distinct entry.
        store
            .record_manifest_ask("npub1a", "npub1a", "other", "fp-3", "2026-01-03T00:00:00Z", "nonce-x")
            .unwrap();
        let m = store.load_manifest_asks().unwrap();
        assert_eq!(m.len(), 3);
        let key_a = manifest_ask_key("npub1a", "npub1a", "criterion");
        assert_eq!(m[&key_a].fingerprint_seen, "fp-1");
        assert_eq!(m[&key_a].sent_at, "2026-01-01T00:00:00Z");
        assert_eq!(m[&manifest_ask_key("npub1b", "npub1b", "criterion")].sent_at, "2026-01-02T00:00:00Z");
        assert_eq!(m[&manifest_ask_key("npub1a", "npub1a", "other")].sent_at, "2026-01-03T00:00:00Z");
    }

    #[test]
    fn manifest_ask_overwrites_on_re_ask_for_same_pair() {
        // A re-ask is a re-ask: the newest send wins. One entry per (npub, slug), not a history.
        let (_dir, store) = test_store();
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp-old", "2026-01-01T00:00:00Z", "nonce-x")
            .unwrap();
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp-new", "2026-01-09T00:00:00Z", "nonce-x")
            .unwrap();
        let m = store.load_manifest_asks().unwrap();
        assert_eq!(m.len(), 1, "exactly one entry per (npub, slug)");
        let entry = &m[&manifest_ask_key("npub1a", "npub1a", "criterion")];
        assert_eq!(entry.fingerprint_seen, "fp-new");
        assert_eq!(entry.sent_at, "2026-01-09T00:00:00Z");
    }

    // ── QURATOR-250 — ask-trace retention (eviction, never a processing cap) ────────────────
    // The persisted set is attacker-growable (a malicious listing with N fake slugs mints N
    // `peer|author|slug` keys); before this ticket nothing ever removed one. The retention sweep
    // forgets dead entries; it must never refuse a fresh ask — the same forget-never-refuse
    // shape `auto_approve::remember_answered` established for the answered-ask memory.

    /// A `ManifestAsk` as `record_manifest_ask` would have written it, with the given `sent_at`.
    fn ask_sent_at(sent_at: &str) -> ManifestAsk {
        ManifestAsk {
            fingerprint_seen: "fp".into(),
            sent_at: sent_at.into(),
            nonce: "nonce".into(),
            claimed_by: None,
            spent: false,
            dial_attempts: 0,
            dial_last_fail_unix: 0,
        }
    }

    /// QURATOR-250 — the pinned clock every retention test drives time through. Never wall time:
    /// a wall-clock fixture dodged the very gate under test once already (2026-09-01 incident).
    fn retention_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// MUTATION (P-10) — production anchor: `manifest_ask_is_expired` in store.rs, the `Some(sent)`
    /// arm's comparison at store.rs:1521 (`now.timestamp().saturating_sub(sent.timestamp()) >=
    /// MANIFEST_ASK_RETENTION_SECS as i64`). Change `>=` to `>` → the exactly-30-day-old spent
    /// entry below flips to retained and this test reds on `removed == 2`. (Inverting the `!` in
    /// `evict_expired_manifest_asks`'s `retain` closure at store.rs:1351 also reds it: the LIVE
    /// entries would be the ones evicted.)
    #[test]
    fn expired_manifest_asks_are_evicted_and_live_ones_retained() {
        let (_dir, store) = test_store();
        let now = retention_now();
        let mut m = std::collections::HashMap::new();
        // 31 days old — past the 30-day window.
        m.insert(manifest_ask_key("npub1a", "npub1a", "ancient"), ask_sent_at("2026-05-01T00:00:00Z"));
        // 29 days old — inside it.
        m.insert(manifest_ask_key("npub1a", "npub1a", "recent"), ask_sent_at("2026-05-03T00:00:00Z"));
        // Asked "now" — live by any reading.
        m.insert(manifest_ask_key("npub1b", "npub1b", "fresh"), ask_sent_at("2026-06-01T00:00:00Z"));
        // A spent entry is still a trace the UI reads; it keeps the same window as the rest.
        let mut spent = ask_sent_at("2026-05-02T00:00:00Z");
        spent.spent = true;
        m.insert(manifest_ask_key("npub1c", "npub1c", "answered-old"), spent);
        store.save_manifest_asks(&m).unwrap();

        let removed = store.evict_expired_manifest_asks(now).unwrap();
        assert_eq!(removed, 2, "the 31-day and 30-day-old-spent entries are past retention");
        let left = store.load_manifest_asks().unwrap();
        assert_eq!(left.len(), 2, "only the entries inside the window survive");
        assert!(left.contains_key(&manifest_ask_key("npub1a", "npub1a", "recent")));
        assert!(left.contains_key(&manifest_ask_key("npub1b", "npub1b", "fresh")));
        assert!(
            !left.contains_key(&manifest_ask_key("npub1a", "npub1a", "ancient")),
            "an ask nothing has re-sent for a whole month is dead bookkeeping"
        );
    }

    /// MUTATION (P-10) — the REFUSE half is what this test pins: in `record_manifest_ask`
    /// (store.rs:1211, the `m.insert(` block), make the function return `Ok(())` before the
    /// insert → the fresh ask leaves no trace, `claim_manifest_ask` answers `Unsolicited`, and
    /// this test reds. The FORGET half (the eviction count) is pinned by the neighbouring
    /// `expired_manifest_asks_are_evicted_and_live_ones_retained` instead — a one-entry map
    /// cannot distinguish "evicted" from "cleared" here.
    #[test]
    fn eviction_never_refuses_a_fresh_ask() {
        let (_dir, store) = test_store();
        let now = retention_now();
        // A dead entry: past the window, evicted.
        store
            .record_manifest_ask("npub1a", "npub1a", "ancient", "fp", "2026-01-01T00:00:00Z", "n-old")
            .unwrap();
        assert_eq!(store.evict_expired_manifest_asks(now).unwrap(), 1);
        assert!(store.load_manifest_asks().unwrap().is_empty(), "the dead entry is forgotten");

        // A fresh ask after eviction must proceed exactly as before — the retention cut must
        // never REFUSE, only forget (`remember_answered`'s semantics, mirrored).
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp", "2026-06-01T00:00:00Z", "n-new")
            .unwrap();
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-new", "req-A").unwrap(),
            crate::store::AskClaim::Granted,
            "a fresh ask is always granted — the retention cut must never REFUSE, only forget"
        );
        store.spend_manifest_ask("npub1a", "npub1a", "criterion", "n-new").unwrap();
        assert_eq!(store.load_manifest_asks().unwrap().len(), 1, "the fresh ask's trace persists");
    }

    /// MUTATION (P-10) — production anchor: `manifest_ask_is_expired` in store.rs, the
    /// `None => false` arm at store.rs:1523. Change it to `None => true` → the undateable entry
    /// below is evicted and this test reds.
    #[test]
    fn an_undated_manifest_ask_is_retained_and_keeps_its_neighbours() {
        let (_dir, store) = test_store();
        let now = retention_now();
        let mut m = std::collections::HashMap::new();
        // A corrupt stamp (`sent_at` is minted locally, never peer-supplied, so this is disk
        // corruption, not an attack): kept, and the load path must neither panic nor discard
        // the rest of the set around it.
        m.insert(manifest_ask_key("npub1a", "npub1a", "corrupt"), ask_sent_at("not-a-timestamp"));
        m.insert(manifest_ask_key("npub1b", "npub1b", "live"), ask_sent_at("2026-05-30T00:00:00Z"));
        store.save_manifest_asks(&m).unwrap();

        assert_eq!(store.evict_expired_manifest_asks(now).unwrap(), 0, "nothing dateable expired");
        let left = store.load_manifest_asks().unwrap();
        assert_eq!(left.len(), 2, "undateable is retained; its live neighbour is untouched");
        assert!(left.contains_key(&manifest_ask_key("npub1a", "npub1a", "corrupt")));
        assert!(left.contains_key(&manifest_ask_key("npub1b", "npub1b", "live")));
    }

    /// MUTATION (P-10) — production anchor: `MANIFEST_ASK_RETENTION_SECS` in store.rs, the const
    /// at store.rs:1506. Change `30 * 24 * 60 * 60` to `365 * 24 * 60 * 60` → the "last month's
    /// rotation" entries below flip to retained and this test reds on `left.len() == 40` (the
    /// set would grow without bound across rotations).
    #[test]
    fn manifest_asks_stay_bounded_under_sustained_pressure() {
        let (_dir, store) = test_store();
        let now = retention_now();
        let mut m = std::collections::HashMap::new();
        // The malicious-listing shape: one peer whose listing keeps rotating fake slugs. Last
        // month's 200 entries are past the window; this month's 40 are inside it.
        for i in 0..200 {
            m.insert(
                manifest_ask_key("npub1evil", "npub1evil", &format!("rot-{i}")),
                ask_sent_at("2026-04-01T00:00:00Z"),
            );
        }
        for i in 0..40 {
            m.insert(
                manifest_ask_key("npub1evil", "npub1evil", &format!("cur-{i}")),
                ask_sent_at("2026-05-20T00:00:00Z"),
            );
        }
        store.save_manifest_asks(&m).unwrap();

        assert_eq!(store.evict_expired_manifest_asks(now).unwrap(), 200);
        let left = store.load_manifest_asks().unwrap();
        assert_eq!(
            left.len(),
            40,
            "the persisted set is bounded by the distinct (peer, author, slug) triples asked \
             within the retention window — a peer with fresh nonces cannot grow it forever"
        );
        assert!(left.keys().all(|k| k.contains("|cur-")), "only the within-window entries remain");
    }

    // ── QURATOR-293 — ask-trace LIVENESS eviction (composes with the 30-day window, never a cap) ──
    // The retention sweep above only bounds slugs the peer REMOVED, eventually: a rotating listing
    // holds a 30-day tail of dead rows. Eviction on the next discovery poll makes the removal take
    // effect immediately — and, like retention, it forgets entries without ever refusing an ask.

    /// QURATOR-293 — the core attribution: one author's poll evicts exactly that author's
    /// unlisted entries (self-ask AND Carrier-4 spellings), never another author's same-named
    /// slug, never a malformed key.
    ///
    /// MUTATION (P-10) — production anchor: `manifest_ask_is_unlisted` in store.rs, the
    /// `if listed.contains(slug) { return false; }` guard. Change `listed.contains(slug)` to
    /// `!listed.contains(slug)` → the LISTED entry is evicted and every unlisted one retained;
    /// this test reds on `removed == 1` (expected 2) and on `left.len() == 4` (expected 3).
    /// (Independent second mutation, run separately: delete the `|| segments[1] != author`
    /// conjunct from the 3-segment check → the foreign `npub1b` entry is evicted too and both
    /// asserts red the other way, `removed == 3` / `left.len() == 2`.)
    #[test]
    fn unlisted_ask_traces_are_evicted_and_listed_and_foreign_ones_retained() {
        let (_dir, store) = test_store();
        let now = retention_now();
        // 24 h old — past the 1 h liveness grace, so nothing here is protected by freshness; the
        // grace itself is pinned by the neighbouring test below.
        let aged = "2026-05-31T00:00:00Z";
        let mut m = std::collections::HashMap::new();
        // The author's CURRENT listing carries "films": its trace is live, kept.
        m.insert(manifest_ask_key("npub1a", "npub1a", "films"), ask_sent_at(aged));
        // The author removed "rotated-out": dead bookkeeping, evicted.
        m.insert(manifest_ask_key("npub1a", "npub1a", "rotated-out"), ask_sent_at(aged));
        // ANOTHER author's entry for the REMOVED slug name — only the author-segment check keeps
        // it alive when npub1a's poll sees "rotated-out" gone (slug collision, other direction).
        m.insert(manifest_ask_key("npub1b", "npub1b", "rotated-out"), ask_sent_at(aged));
        // A Carrier-4 ask ABOUT npub1a's removed slug, asked of a third peer: keyed by the AUTHOR
        // being polled, it dies with the listing too.
        m.insert(manifest_ask_key("npub1c", "npub1a", "rotated-out"), ask_sent_at(aged));
        // A malformed key (4 segments — the legacy widener only maps 2→3 and leaves this alone):
        // not attributable to any author, kept.
        m.insert("npub1d|npub1d|films|extra".to_string(), ask_sent_at(aged));
        store.save_manifest_asks(&m).unwrap();

        let listed: std::collections::HashSet<String> =
            ["films"].into_iter().map(String::from).collect();
        let removed = store.evict_unlisted_manifest_asks("npub1a", &listed, now).unwrap();
        assert_eq!(removed, 2, "the removed slug's self-ask AND carrier-ask traces die");
        let left = store.load_manifest_asks().unwrap();
        assert_eq!(left.len(), 3, "listed, foreign-author, and malformed keys survive");
        assert!(left.contains_key(&manifest_ask_key("npub1a", "npub1a", "films")));
        assert!(left.contains_key(&manifest_ask_key("npub1b", "npub1b", "rotated-out")));
        assert!(!left.contains_key(&manifest_ask_key("npub1a", "npub1a", "rotated-out")));
        assert!(!left.contains_key(&manifest_ask_key("npub1c", "npub1a", "rotated-out")));
    }

    /// QURATOR-293 — the two in-flight protections: an unspent CLAIMED entry may still be inside
    /// the QURATOR-197 dial backoff, and a fresh ask may still be awaiting its first answer. A
    /// SPENT entry is terminal and dies with the listing.
    ///
    /// MUTATION (P-10) — production anchor: `manifest_ask_is_unlisted` in store.rs, the
    /// `if !ask.spent && ask.claimed_by.is_some() { return false; }` guard. Change
    /// `ask.claimed_by.is_some()` to `false` → the claimed entry is evicted and this reds on
    /// `removed == 3`. (Independent second mutation, run separately: change
    /// `MANIFEST_ASK_LIVENESS_GRACE_SECS` from `60 * 60` to `0` → the awaiting-reply entry is
    /// evicted and the same assert reds.)
    #[test]
    fn liveness_eviction_spares_a_claimed_ask_and_one_inside_its_grace() {
        let (_dir, store) = test_store();
        let now = retention_now();
        let mut m = std::collections::HashMap::new();
        // Unlisted, but a ticket CLAIMED it — the dial backoff may still be running.
        let mut claimed = ask_sent_at("2026-05-01T00:00:00Z");
        claimed.claimed_by = Some("req-1".into());
        m.insert(manifest_ask_key("npub1a", "npub1a", "claimed"), claimed);
        // Unlisted, asked 30 s ago — the reply may simply not have arrived yet.
        m.insert(
            manifest_ask_key("npub1a", "npub1a", "awaiting-reply"),
            ask_sent_at("2026-05-31T23:59:30Z"),
        );
        // Unlisted, spent AND still carrying its claim — terminal: awaits nothing, dies.
        let mut spent = ask_sent_at("2026-05-01T00:00:00Z");
        spent.spent = true;
        spent.claimed_by = Some("req-2".into());
        m.insert(manifest_ask_key("npub1a", "npub1a", "answered"), spent);
        // Unlisted, old, unclaimed — the eviction's actual target.
        m.insert(manifest_ask_key("npub1a", "npub1a", "dead"), ask_sent_at("2026-05-01T00:00:00Z"));
        store.save_manifest_asks(&m).unwrap();

        let listed: std::collections::HashSet<String> = std::collections::HashSet::new();
        assert_eq!(store.evict_unlisted_manifest_asks("npub1a", &listed, now).unwrap(), 2);
        let left = store.load_manifest_asks().unwrap();
        assert!(left.contains_key(&manifest_ask_key("npub1a", "npub1a", "claimed")));
        assert!(left.contains_key(&manifest_ask_key("npub1a", "npub1a", "awaiting-reply")));
        assert!(!left.contains_key(&manifest_ask_key("npub1a", "npub1a", "answered")));
        assert!(!left.contains_key(&manifest_ask_key("npub1a", "npub1a", "dead")));
    }

    /// QURATOR-293 — forget-never-refuse, and composition: eviction shortens the trace only, and
    /// an author that produces NO listing at all (ex-contact, never polled) is still the 30-day
    /// window's alone — the two sweeps are independent, so liveness eviction cannot strand an
    /// entry the window would have taken.
    ///
    /// MUTATION (P-10) — production anchor: `evict_unlisted_manifest_asks`'s `m.retain` closure in
    /// store.rs. Change `!manifest_ask_is_unlisted(k, ask, author, listed, now)` to `false`
    /// (clear the whole map) → the LISTED "films" ask dies with the dead one, `removed` reports 2,
    /// and this test reds on the very next assert (the FORGET half is pinned by the two tests
    /// above; this one pins the refuse half and the composition).
    #[test]
    fn liveness_eviction_never_refuses_a_fresh_ask_and_composes_with_retention() {
        let (_dir, store) = test_store();
        let now = retention_now();
        store
            .record_manifest_ask("npub1a", "npub1a", "gone", "fp", "2026-05-01T00:00:00Z", "n-old")
            .unwrap();
        store
            .record_manifest_ask("npub1a", "npub1a", "films", "fp", "2026-06-01T00:00:00Z", "n-new")
            .unwrap();
        let listed: std::collections::HashSet<String> =
            ["films"].into_iter().map(String::from).collect();
        assert_eq!(
            store.evict_unlisted_manifest_asks("npub1a", &listed, now).unwrap(),
            1,
            "only the unlisted trace dies; the listed one is live"
        );

        // The ask loop that follows the sweep in the same discovery poll records and claims
        // identically with the dead entry gone — eviction must never refuse an ask.
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "films", "n-new", "req-A").unwrap(),
            crate::store::AskClaim::Granted,
            "a fresh ask is always granted — liveness eviction forgets, never refuses"
        );

        // An author no poll ever sees again: liveness eviction cannot touch it, the window must.
        store
            .record_manifest_ask("npub1z", "npub1z", "orphan", "fp", "2026-05-01T00:00:00Z", "n-z")
            .unwrap();
        assert_eq!(
            store.evict_expired_manifest_asks(now).unwrap(),
            1,
            "the ex-contact's orphan dies by retention, not liveness"
        );
        let left = store.load_manifest_asks().unwrap();
        assert!(left.contains_key(&manifest_ask_key("npub1a", "npub1a", "films")));
        assert!(!left.contains_key(&manifest_ask_key("npub1z", "npub1z", "orphan")));
    }

    /// **Carrier 4 lenient load (QURATOR-79)** — an ask map written by an older build carries
    /// 2-segment `{npub}|{slug}` keys. Every such ask was by construction a self-ask (the only kind
    /// that existed), so it must still CLAIM and SPEND correctly after the key widened to
    /// `{npub}|{author}|{slug}` — the migration is on load, not on a command the user must re-run.
    ///
    /// MUTATION (P-10) — resolved by containing function, not text: inside `load_manifest_asks`,
    /// MUTATION (P-10) — in `save_answered_asks`, write an empty set instead of `seen`
    /// (`write_json(&self.answered_asks_path(), &std::collections::HashSet::<String>::new())`) →
    /// this test reds on the reload assert.
    ///
    /// This is the whole point of QURATOR-184: without persistence every app start re-fetched the
    /// relay backlog and re-answered all of it, and strfry retains those asks so the replay grew
    /// over a node's lifetime.
    #[test]
    fn the_answered_ask_memory_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        assert!(
            store.load_answered_asks().unwrap().is_empty(),
            "a node that has never answered anything remembers nothing"
        );

        let mut seen = std::collections::HashSet::new();
        seen.insert("npubA|npubA|films|nonce-1".to_string());
        store.save_answered_asks(&seen).unwrap();

        // A fresh handle on the same directory — the restart this exists to survive.
        let reopened = DataStore::new(dir.path().to_path_buf());
        assert_eq!(
            reopened.load_answered_asks().unwrap(),
            seen,
            "the memory must outlive the process, or the backlog is re-answered on every start"
        );
    }

    /// drop the `widen_legacy_ask_key` rewrite (delete the `if m.keys().any(...)` block) → the
    /// 2-segment key stays 2-segment, `claim_manifest_ask` finds nothing, and the first assert
    /// below reds with `Unsolicited`.
    #[test]
    fn a_legacy_two_segment_ask_key_still_claims_and_spends() {
        let (_dir, store) = test_store();
        // Write the OLD shape by hand — this is what an older build left on disk. A nonce too: a
        // pre-ruling ask (no nonce) fails closed by construction and would pass vacuously.
        write_json(
            &store.manifest_asks_path(),
            &std::collections::HashMap::from([(
                "npub1a|criterion".to_string(),
                ManifestAsk {
                    fingerprint_seen: "fp-1".into(),
                    sent_at: "2026-01-01T00:00:00Z".into(),
                    nonce: "n-1".into(),
                    claimed_by: None,
                    spent: false,
                    dial_attempts: 0,
                    dial_last_fail_unix: 0,
                },
            )]),
        )
        .unwrap();

        // The widened key resolves the legacy entry: claim is Granted (not Unsolicited).
        let claim = store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", "req-A").unwrap();
        assert!(matches!(claim, AskClaim::Granted), "a legacy ask must still claim: {claim:?}");

        // And it spends — one ask, one auto-dial survives the widening.
        store.spend_manifest_ask("npub1a", "npub1a", "criterion", "n-1").unwrap();
        let claim = store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", "req-A").unwrap();
        assert!(matches!(claim, AskClaim::Spent), "a legacy ask must still spend: {claim:?}");

        // The claim's WRITE persisted the widened spelling — the migration converges on disk.
        let raw = std::fs::read_to_string(store.manifest_asks_path()).unwrap();
        assert!(raw.contains("\"npub1a|npub1a|criterion\""), "the widened key is what saved: {raw}");
    }

    /// **Carrier 4 (QURATOR-79)** — a self-ask (the peer's own collection) is spelled
    /// `author == npub`, i.e. `{npub}|{npub}|{slug}` — the exact spelling the TypeScript gate
    /// `ticketAnswersOurAsk` tries on the owner path. Round-trips through the widened key, and does
    /// NOT collide with a third-party-author ask for the same `(npub, slug)`.
    ///
    /// MUTATION (P-10): inside `manifest_ask_key`, emit `format!("{npub}|{slug}")` (drop the
    /// author) → the two entries below land on one key, `m.len() == 1`, and the length assert reds.
    #[test]
    fn a_self_ask_round_trips_through_the_widened_key() {
        let (_dir, store) = test_store();
        // Self-ask: we asked npub1a for npub1a's own collection.
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp-own", "2026-01-01T00:00:00Z", "n-own")
            .unwrap();
        // Re-serve ask: we asked npub1a for npub1b's collection. Same peer, same slug.
        store
            .record_manifest_ask("npub1a", "npub1b", "criterion", "fp-b", "2026-01-02T00:00:00Z", "n-b")
            .unwrap();

        let m = store.load_manifest_asks().unwrap();
        assert_eq!(m.len(), 2, "the author is part of the ask's identity — two distinct entries");
        assert_eq!(manifest_ask_key("npub1a", "npub1a", "criterion"), "npub1a|npub1a|criterion");
        assert_eq!(m[&manifest_ask_key("npub1a", "npub1a", "criterion")].nonce, "n-own");
        assert_eq!(m[&manifest_ask_key("npub1a", "npub1b", "criterion")].nonce, "n-b");

        // Each claims under its own key only — the cross-tenant boundary the re-serve spelling draws.
        assert!(matches!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-own", "req-own").unwrap(),
            AskClaim::Granted
        ));
        assert!(matches!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-b", "req-x").unwrap(),
            AskClaim::Unsolicited
        ));
    }

    #[test]
    fn manifest_asks_is_wiped_with_the_rest_of_the_profile() {
        let (_dir, store) = test_store();
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp-1", "2026-01-01T00:00:00Z", "nonce-1")
            .unwrap();
        assert!(store.manifest_asks_path().exists());

        store.wipe().unwrap();
        assert!(
            !store.manifest_asks_path().exists(),
            "manifest_asks.json must be removed by wipe()"
        );
    }

    // ── Standing grants (QURATOR-137 slice 2) — record-only: nothing reads these to decide
    //    anything yet; slice 3 wires the redeem-time consultation.





}
