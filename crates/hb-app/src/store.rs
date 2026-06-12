//! Typed read/write helpers for the on-disk data directory.
//!
//! Layout mirrors the spec:
//! ```text
//! <app_data_dir>/
//!   identity/
//!     keypair.json            StoredKeypair
//!     profile.signed.json     SignedEnvelope (profile)
//!   collections/
//!     <slug>.signed.json      SignedEnvelope (collection)
//!     <slug>.draft.json       Collection (unsigned draft)
//!   contacts/
//!     <pubkey_hash>.json      CachedPeer
//!   settings.json
//! ```

use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use hb_core::{StoredKeypair, SignedEnvelope};

// ---------------------------------------------------------------------------
// Settings — persisted user preferences
// ---------------------------------------------------------------------------

fn default_true() -> bool { true }
fn default_dht_port() -> u16 { 6882 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub relay_urls: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_dms: bool,
    #[serde(default)]
    pub dht_announce_enabled: bool,
    #[serde(default)]
    pub dht_announce_tags: Vec<String>,
    #[serde(default)]
    pub dht_announce_content_types: Vec<String>,
    #[serde(default = "default_dht_port")]
    pub dht_identity_port: u16,
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

    pub fn keypair_path(&self) -> PathBuf {
        // .bin on Windows (DPAPI-encrypted opaque blob), .json on Linux (plain chmod 600).
        #[cfg(target_os = "windows")]
        let filename = "keypair.bin";
        #[cfg(not(target_os = "windows"))]
        let filename = "keypair.json";
        self.base.join("identity").join(filename)
    }

    pub fn profile_signed_path(&self) -> PathBuf {
        self.base.join("identity").join("profile.signed.json")
    }

    pub fn profile_draft_path(&self) -> PathBuf {
        self.base.join("identity").join("profile.draft.json")
    }

    pub fn collection_signed_path(&self, slug: &str) -> PathBuf {
        self.base.join("collections").join(format!("{slug}.signed.json"))
    }

    pub fn collection_draft_path(&self, slug: &str) -> PathBuf {
        self.base.join("collections").join(format!("{slug}.draft.json"))
    }

    pub fn contact_path(&self, pubkey_hash: &str) -> PathBuf {
        self.base.join("contacts").join(format!("{pubkey_hash}.json"))
    }

    pub fn settings_path(&self) -> PathBuf {
        self.base.join("settings.json")
    }

    // -- Identity ------------------------------------------------------------

    pub fn save_keypair(&self, kp: &StoredKeypair) -> Result<()> {
        let path = self.keypair_path();
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

        let json = serde_json::to_string_pretty(kp)?;

        #[cfg(target_os = "windows")]
        {
            let encrypted = hb_dpapi::encrypt(json.as_bytes())
                .context("DPAPI encryption failed")?;
            std::fs::write(&path, encrypted)?;
        }

        #[cfg(not(target_os = "windows"))]
        {
            let first_write = !path.exists();
            std::fs::write(&path, json.as_bytes())?;
            // Restrict to owner read/write only.
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            if first_write {
                tracing::warn!(
                    "Private key stored as a plain file at {:?}. Keep your home directory secure.",
                    path
                );
            }
        }

        Ok(())
    }

    pub fn load_keypair(&self) -> Result<Option<StoredKeypair>> {
        let path = self.keypair_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).context("reading keypair file")?;

        // A 0-byte keypair file is a failed/partial write (e.g. the DPAPI
        // CRED_SYNC bug wrote an empty blob). Treat it as "absent" so the app
        // regenerates an identity instead of dead-ending forever on the
        // "Identity file unreadable" recovery screen.
        if bytes.is_empty() {
            return Ok(None);
        }

        #[cfg(target_os = "windows")]
        let json_bytes = hb_dpapi::decrypt(&bytes).context("DPAPI decryption failed")?;

        #[cfg(not(target_os = "windows"))]
        let json_bytes = bytes;

        Ok(Some(serde_json::from_slice(&json_bytes).context("parsing keypair")?))
    }

    // -- Profile -------------------------------------------------------------

    pub fn save_profile_draft(&self, profile: &Profile) -> Result<()> {
        write_json(&self.profile_draft_path(), profile)
            .context("saving profile draft")
    }

    pub fn load_profile_draft(&self) -> Result<Option<Profile>> {
        read_json_lenient(&self.profile_draft_path()).context("loading profile draft")
    }

    pub fn save_profile_signed(&self, env: &SignedEnvelope) -> Result<()> {
        write_json(&self.profile_signed_path(), env)
            .context("saving signed profile")
    }

    pub fn load_profile_signed(&self) -> Result<Option<SignedEnvelope>> {
        read_json(&self.profile_signed_path()).context("loading signed profile")
    }

    // -- Collections ---------------------------------------------------------

    pub fn save_collection_draft(&self, collection: &Collection) -> Result<()> {
        write_json(&self.collection_draft_path(&collection.slug), collection)
            .context("saving collection draft")
    }

    pub fn save_collection_signed(&self, slug: &str, env: &SignedEnvelope) -> Result<()> {
        write_json(&self.collection_signed_path(slug), env)
            .context("saving signed collection")
    }

    /// List all signed collection envelopes.
    pub fn list_collections(&self) -> Result<Vec<SignedEnvelope>> {
        let dir = self.base.join("collections");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut results = vec![];
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".signed"))
                    .unwrap_or(false)
            {
                if let Ok(Some(env)) = read_json::<SignedEnvelope>(&path) {
                    results.push(env);
                }
            }
        }
        Ok(results)
    }

    /// List slug names that have draft files but no signed file.
    pub fn list_draft_only_slugs(&self) -> Result<Vec<String>> {
        let dir = self.base.join("collections");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut drafts = vec![];
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && stem.ends_with(".draft")
            {
                let slug = stem.trim_end_matches(".draft").to_string();
                // Only include if no signed version exists.
                let signed = self.collection_signed_path(&slug);
                if !signed.exists() {
                    drafts.push(slug);
                }
            }
        }
        Ok(drafts)
    }

    /// Load a draft collection by slug.
    pub fn load_collection_draft(&self, slug: &str) -> Result<Option<Collection>> {
        read_json(&self.collection_draft_path(slug)).context("loading collection draft")
    }

    pub fn share_settings_path(&self, slug: &str) -> PathBuf {
        self.base.join("sharing").join(format!("{slug}.json"))
    }

    pub fn delete_collection(&self, slug: &str) -> Result<()> {
        for path in &[
            self.collection_draft_path(slug),
            self.collection_signed_path(slug),
            self.share_settings_path(slug),
        ] {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
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
        for subdir in &["identity", "collections", "contacts", "sharing"] {
            let path = self.base.join(subdir);
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
        }
        let settings = self.settings_path();
        if settings.exists() {
            std::fs::remove_file(&settings)?;
        }
        // The PEX address cache is network-learned data — clear it with everything else.
        let peers = self.peers_path();
        if peers.exists() {
            std::fs::remove_file(&peers)?;
        }
        Ok(())
    }

    // -- Contacts ------------------------------------------------------------

    pub fn load_contact(&self, pubkey_hash: &str) -> Result<Option<CachedPeer>> {
        read_json(&self.contact_path(pubkey_hash)).context("loading contact")
    }

    pub fn save_contact(&self, pubkey_hash: &str, peer: &CachedPeer) -> Result<()> {
        write_json(&self.contact_path(pubkey_hash), peer)
            .context("saving contact")
    }

    pub fn delete_contact(&self, pubkey_hash: &str) -> Result<()> {
        let path = self.contact_path(pubkey_hash);
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
    pub hb_id: String,
    pub profile: Option<Profile>,
    pub collections: Vec<Collection>,
    pub online: bool,
    pub node_addr: Option<String>,
    pub last_fetched: chrono::DateTime<chrono::Utc>,
    /// Last time the relay received a heartbeat from this peer (relay-side timestamp).
    /// More accurate than last_fetched for "last seen" display.
    #[serde(default)]
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    /// User-defined tags for organizing contacts locally. Never shared.
    #[serde(default)]
    pub local_tags: Vec<String>,
}

impl CachedPeer {
    pub fn pubkey_hash(hb_id: &str) -> String {
        // Use first 16 hex chars of SHA256 of the hb_id as a stable filename.
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(hb_id.as_bytes());
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
    pub pubkeys: Vec<String>,
    /// Last modification time — used to order groups most-recently-modified first.
    #[serde(default = "default_group_modified_at")]
    pub modified_at: chrono::DateTime<chrono::Utc>,
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
// Peer address cache (PEX) — peers.json, spec data model
// ---------------------------------------------------------------------------

impl DataStore {
    pub fn peers_path(&self) -> PathBuf {
        self.base.join("peers.json")
    }

    /// Load the PEX peer address cache: `{ hb_id → entry }`. Lenient like other
    /// local config — a version mismatch falls back to an empty cache.
    pub fn load_peer_cache(
        &self,
    ) -> Result<std::collections::HashMap<String, crate::pex::PeerAddrEntry>> {
        Ok(read_json_lenient(&self.peers_path())
            .context("loading peer cache")?
            .unwrap_or_default())
    }

    pub fn save_peer_cache(
        &self,
        entries: &std::collections::HashMap<String, crate::pex::PeerAddrEntry>,
    ) -> Result<()> {
        write_json(&self.peers_path(), entries).context("saving peer cache")
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
    /// hb_ids already notified — prevents re-firing for the same peer.
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

    // HANDOVER scenario 2: a 0-byte keypair file (the on-disk symptom of the
    // DPAPI CRED_SYNC bug) must be treated as "absent" so the app regenerates,
    // not as an unreadable identity that dead-ends the recovery screen forever.
    // Cross-platform: an empty file fails decrypt (Windows) / JSON parse (Linux)
    // identically, so this asserts Ok(None) on every OS.
    #[test]
    fn empty_keypair_file_treated_as_absent() {
        let (_dir, store) = test_store();
        let path = store.keypair_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap(); // 0-byte keypair.bin / keypair.json

        let loaded = store.load_keypair().expect("empty keypair file must not error");
        assert!(
            loaded.is_none(),
            "a 0-byte keypair file must load as None (absent), got {loaded:?}"
        );
    }

    // HANDOVER scenario 3: end-to-end identity wiring through DataStore on
    // Windows — generate -> save_keypair (DPAPI-encrypt + write) -> load_keypair
    // (read + DPAPI-decrypt) -> reconstruct the keypair. This covers the save/load
    // glue, not just the in-crate DPAPI roundtrip. With the CRED_SYNC (0x8) bug,
    // save_keypair wrote an empty blob, so the asserts on the on-disk bytes and
    // the recovered hb_id both fail.
    #[cfg(target_os = "windows")]
    #[test]
    fn keypair_save_load_roundtrip_windows() {
        use hb_core::HoardbookKeypair;
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        let stored = StoredKeypair {
            version: 1,
            hb_id: kp.hb_id(),
            private_key_hex: hex::encode(kp.private_key_bytes()),
        };
        let plaintext_json = serde_json::to_string_pretty(&stored).unwrap();

        store.save_keypair(&stored).expect("save_keypair must succeed");

        // The on-disk blob must be a real DPAPI ciphertext: non-empty and not the
        // plaintext JSON. CRED_SYNC produced a 0-byte file here.
        let on_disk = std::fs::read(store.keypair_path()).unwrap();
        assert!(!on_disk.is_empty(), "keypair.bin must not be 0 bytes");
        assert_ne!(
            on_disk.as_slice(),
            plaintext_json.as_bytes(),
            "keypair.bin must be encrypted, not stored as plaintext JSON"
        );

        // Reload through the platform path and reconstruct the live keypair.
        let loaded = store
            .load_keypair()
            .expect("load_keypair must succeed")
            .expect("keypair must be present after save");
        assert_eq!(loaded.hb_id, stored.hb_id, "reloaded hb_id must match");

        let bytes: [u8; 32] = hex::decode(&loaded.private_key_hex)
            .unwrap()
            .try_into()
            .unwrap();
        let recovered = HoardbookKeypair::from_bytes(&bytes);
        assert_eq!(
            recovered.hb_id(),
            kp.hb_id(),
            "keypair reconstructed from the reloaded identity must match the original"
        );
    }
}
