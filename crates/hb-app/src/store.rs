//! Typed read/write helpers for the on-disk data directory.
//!
//! Layout (v0.9 Nostr model):
//! ```text
//! <app_data_dir>/
//!   identity/
//!     identity.json           StoredIdentity (nsec + iroh key + account browse-key)
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
        }
    }
}

// ---------------------------------------------------------------------------
// ShareSettings — per-collection P2P sharing config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShareSettings {
    pub enabled: bool,
    pub root_path: Option<String>,
    pub allowed_paths: Vec<String>,
    pub speed_cap_kbps: Option<u32>,
    pub download_limit: Option<u32>,
    pub require_follow: bool,
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
}

// ---------------------------------------------------------------------------
// StoredIdentity — the three keys, on disk (v0.9 Nostr model)
// ---------------------------------------------------------------------------

/// On-disk identity: the irreplaceable secp256k1 secret (`nsec`), the bound iroh transport
/// key (regenerable), and the account browse-key (the "club pass" carried in the `hbk` share
/// code). On Windows this whole struct is DPAPI-encrypted at rest; on Linux/macOS it is a
/// 0600 plaintext file until the Phase-2 keyring lands.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredIdentity {
    pub version: u8,
    /// secp256k1 secret key as bech32 `nsec…` — the one irreplaceable secret.
    pub nsec: String,
    /// Hex-encoded 32-byte account browse-key.
    pub browse_key_hex: String,
}

impl std::fmt::Debug for StoredIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredIdentity")
            .field("version", &self.version)
            .field("nsec", &"[REDACTED]")
            .field("browse_key_hex", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(path, json)?;
    Ok(())
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
fn read_json_lenient<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
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

#[derive(Clone)]
pub struct DataStore {
    pub(crate) base: PathBuf,
}

impl DataStore {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
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

        Ok(Some(serde_json::from_slice(&json_bytes).context("parsing identity")?))
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
    pub fn save_published(&self, key: &str, event_json: &str) -> Result<()> {
        let path = self.published_path(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, event_json).context("saving published event")
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
        let path = self.published_path(key);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn is_published(&self, key: &str) -> bool {
        self.published_path(key).exists()
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
    pub fn wipe(&self) -> Result<()> {
        for subdir in &["identity", "collections", "published", "contacts", "sharing"] {
            let path = self.base.join(subdir);
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
        }
        for file in &[self.settings_path(), self.groups_path(), self.watches_path()] {
            if file.exists() {
                std::fs::remove_file(file)?;
            }
        }
        Ok(())
    }

    // -- Contacts ------------------------------------------------------------

    pub fn load_contact(&self, npub_hash: &str) -> Result<Option<CachedPeer>> {
        read_json(&self.contact_path(npub_hash)).context("loading contact")
    }

    pub fn save_contact(&self, npub_hash: &str, peer: &CachedPeer) -> Result<()> {
        write_json(&self.contact_path(npub_hash), peer)
            .context("saving contact")
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
                if let Ok(Some(peer)) = read_json_lenient::<CachedPeer>(&path) {
                    results.push(peer);
                }
            }
        }
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// CachedPeer — one file per followed peer in contacts/
// ---------------------------------------------------------------------------

use hb_core::types::{Collection, Profile};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPeer {
    /// The peer's Nostr identity (bech32 `npub`) — the stable key the follower-gate keys on.
    pub npub: String,
    /// The peer's account browse-key (hex), captured from a full `hbk` share code — lets us
    /// decrypt their listings + unseal their presence address. `None` for a follow-only contact.
    #[serde(default)]
    pub browse_key_hex: Option<String>,
    /// Local impersonation-resistant petname (bound to `npub`, never shared).
    #[serde(default)]
    pub petname: Option<String>,
    pub profile: Option<Profile>,
    pub collections: Vec<Collection>,
    pub online: bool,
    pub last_fetched: chrono::DateTime<chrono::Utc>,
    /// User-defined tags for organizing contacts locally. Never shared.
    #[serde(default)]
    pub local_tags: Vec<String>,
}

impl CachedPeer {
    pub fn pubkey_hash(npub: &str) -> String {
        // Use first 16 hex chars of SHA256 of the npub as a stable filename.
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(npub.as_bytes());
        hex::encode(&hash[..8])
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
    /// Marks this group as **trusted** (M10): its members' `npub`s receive a per-recipient
    /// sealed copy of every Private collection. `#[serde(default)]` ⇒ a pre-M10 group loads as
    /// untrusted (false), so trust is never silently granted on upgrade. Local-only, never shared.
    #[serde(default)]
    pub trusted: bool,
}

impl DataStore {
    pub fn groups_path(&self) -> PathBuf {
        self.base.join("groups.json")
    }

    pub fn load_groups(&self) -> Result<Vec<Group>> {
        let mut groups = read_json_lenient::<Vec<Group>>(&self.groups_path())
            .context("loading groups")?
            .unwrap_or_default();
        groups.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
        Ok(groups)
    }

    pub fn save_groups(&self, groups: &[Group]) -> Result<()> {
        write_json(&self.groups_path(), groups).context("saving groups")
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
        }
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
        };
        store.save_scan_spec("films", &spec).unwrap();
        let loaded = store.load_scan_spec("films").unwrap().unwrap();
        assert_eq!(loaded.root, "/mnt/share/films");
        assert_eq!(loaded.include, vec!["criterion".to_string()]);
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
}
