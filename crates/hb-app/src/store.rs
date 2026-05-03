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
use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use hb_core::{StoredKeypair, SignedEnvelope};

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
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
    /// Tracks concurrent active downloads for enforcing download_limit.
    pub(crate) active_downloads: Arc<AtomicU32>,
}

impl DataStore {
    pub fn new(base: PathBuf) -> Self {
        Self { base, active_downloads: Arc::new(AtomicU32::new(0)) }
    }

    pub fn active_download_count(&self) -> u32 {
        self.active_downloads.load(Ordering::Relaxed)
    }

    pub fn acquire_download_slot(&self) -> u32 {
        self.active_downloads.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn release_download_slot(&self) {
        self.active_downloads.fetch_sub(1, Ordering::Relaxed);
    }

    // -- Paths ---------------------------------------------------------------

    pub fn keypair_path(&self) -> PathBuf {
        self.base.join("identity").join("keypair.json")
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
        write_json(&self.keypair_path(), kp)
            .context("saving keypair")
    }

    pub fn load_keypair(&self) -> Result<Option<StoredKeypair>> {
        read_json(&self.keypair_path()).context("loading keypair")
    }

    // -- Profile -------------------------------------------------------------

    pub fn save_profile_draft(&self, profile: &Profile) -> Result<()> {
        write_json(&self.profile_draft_path(), profile)
            .context("saving profile draft")
    }

    pub fn load_profile_draft(&self) -> Result<Option<Profile>> {
        read_json(&self.profile_draft_path()).context("loading profile draft")
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

    pub fn save_settings(&self, settings: &crate::commands::settings::Settings) -> Result<()> {
        write_json(&self.settings_path(), settings).context("saving settings")
    }

    pub fn load_settings(&self) -> Result<Option<crate::commands::settings::Settings>> {
        read_json_lenient(&self.settings_path()).context("loading settings")
    }

    // -- Share settings ------------------------------------------------------

    pub fn save_share_settings(
        &self,
        slug: &str,
        settings: &crate::commands::sharing::ShareSettings,
    ) -> Result<()> {
        write_json(&self.share_settings_path(slug), settings).context("saving share settings")
    }

    pub fn load_share_settings(
        &self,
        slug: &str,
    ) -> Result<Option<crate::commands::sharing::ShareSettings>> {
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

use serde::Deserialize;
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
