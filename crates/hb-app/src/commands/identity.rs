//! Identity commands on the v0.9 Nostr model: a secp256k1 `npub`, a bound iroh transport key, and
//! the account browse-key (the `hbk` share code). Replaces the legacy Ed25519 keypair identity.

use serde::Serialize;
use tauri::State;
use zeroize::Zeroizing;

use crate::{
    backup::backup_inner,
    identity_state::{AppIdentity, SharedIdentity},
    error::{CmdResult, cmd_err},
    store::DataStore,
};
use hb_core::BackupMode;

#[derive(Debug, Clone, Serialize)]
pub struct IdentityInfo {
    /// The bech32 `npub` — the identity everywhere.
    pub npub: String,
    pub npub_short: String,
    /// The full `hbk…` share code (npub + account browse-key) — the "club pass" to hand out.
    pub share_code: String,
    /// How the private key is protected at rest: "os-encrypted" (Windows DPAPI) or "plain-file".
    pub key_storage: &'static str,
}

/// Spec: Linux/macOS keep the key as a 0600 plaintext file until the Phase-2 keyring lands; the
/// UI shows the storage warning when this is "plain-file".
const KEY_STORAGE: &str = if cfg!(target_os = "windows") { "os-encrypted" } else { "plain-file" };

impl IdentityInfo {
    fn from_identity(id: &AppIdentity) -> anyhow::Result<Self> {
        let npub = id.npub();
        Ok(Self {
            npub_short: shorten(&npub),
            share_code: id.share_code()?,
            npub,
            key_storage: KEY_STORAGE,
        })
    }
}

fn shorten(id: &str) -> String {
    if id.len() <= 14 {
        return id.to_string();
    }
    format!("{}…{}", &id[..8], &id[id.len() - 4..])
}

/// Generate a fresh identity (npub + iroh key + account browse-key) and persist it.
/// Errors if an identity already exists — identities are fixed in Phase 1 (the only way to replace
/// one is Settings → Wipe data).
#[tauri::command]
pub async fn generate_keypair(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<IdentityInfo> {
    match store.load_identity() {
        Ok(Some(_)) => return Err("An identity already exists. Wipe data first to generate a new one.".into()),
        Ok(None) => {}
        Err(e) => {
            if store.identity_path().exists() {
                return Err(format!(
                    "Existing identity data cannot be read ({e}). \
                     Go to Settings → Wipe data to clear all local data and start over."
                ));
            }
            return Err(cmd_err(e));
        }
    }

    let app_id = AppIdentity::generate();
    let stored = app_id.to_stored().map_err(cmd_err)?;
    store.save_identity(&stored).map_err(cmd_err)?;
    let info = IdentityInfo::from_identity(&app_id).map_err(cmd_err)?;

    *identity.write().await = Some(app_id);
    Ok(info)
}

/// Import an existing Nostr secret key (`nsec`/hex): validate it, derive the matching `npub`, and
/// mint a fresh iroh key + browse-key. Refuses if an identity already exists (wipe-first). The
/// `nsec` is held in a zeroize-on-drop buffer for the call.
///
/// The UI must surface the de-pseudonymization implication of linking a public/Qurator `npub`
/// **before** invoking this — there is no offline oracle to detect a "public" key, so the UI
/// always warns (no hardcoded list, no relay lookup).
#[tauri::command]
pub async fn import_nsec(
    nsec: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<IdentityInfo> {
    let nsec = Zeroizing::new(nsec);
    let app_id = import_nsec_inner(&store, &nsec).map_err(cmd_err)?;
    let info = IdentityInfo::from_identity(&app_id).map_err(cmd_err)?;

    *identity.write().await = Some(app_id);
    Ok(info)
}

/// Tauri-free import seam: validate the nsec, mint fresh transport/browse keys, persist (re-wrap at
/// rest). Refuses when an identity already exists. Drives the L1 tests.
pub fn import_nsec_inner(store: &DataStore, nsec: &str) -> anyhow::Result<AppIdentity> {
    if store.load_identity()?.is_some() {
        anyhow::bail!("An identity already exists. Wipe data first to import a different key.");
    }
    let app_id = AppIdentity::from_nsec(nsec)?;
    store.save_identity(&app_id.to_stored()?)?;
    Ok(app_id)
}

/// Load the current identity from disk. Returns `None` if no identity exists yet.
#[tauri::command]
pub async fn get_identity(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<Option<IdentityInfo>> {
    if let Some(ref id) = *identity.read().await {
        return Ok(Some(IdentityInfo::from_identity(id).map_err(cmd_err)?));
    }

    let stored = match store.load_identity().map_err(cmd_err)? {
        Some(s) => s,
        None => return Ok(None),
    };
    let app_id = AppIdentity::from_stored(&stored)
        .map_err(|e| format!("Stored identity is corrupted: {e}"))?;
    let info = IdentityInfo::from_identity(&app_id).map_err(cmd_err)?;
    *identity.write().await = Some(app_id);
    Ok(Some(info))
}

/// Return the full `hbk…` share code to hand out.
#[tauri::command]
pub async fn get_share_code(identity: State<'_, SharedIdentity>) -> CmdResult<String> {
    identity
        .read()
        .await
        .as_ref()
        .ok_or_else(|| "No identity loaded.".to_string())?
        .share_code()
        .map_err(cmd_err)
}

/// Validate a pasted share code (npub or hbk) — codec/checksum only, no network.
#[tauri::command]
pub async fn validate_share_code(code: String) -> CmdResult<bool> {
    Ok(hb_core::ShareCode::parse(&code).is_ok())
}

/// Export a **portable, whole-`~/.hoardbook` backup** to `path`. `passphrase = Some` →
/// Argon2id → XChaCha20-Poly1305 (the portable default); `passphrase = None` → the plaintext
/// export (behind the UI's blunt "this file *is* your identity" warning). Replaces the legacy
/// key-only plaintext export.
#[tauri::command]
pub async fn backup_data(
    passphrase: Option<String>,
    path: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let pass = passphrase.map(Zeroizing::new);
    let mode = match &pass {
        Some(p) => BackupMode::Passphrase(p.as_str()),
        None => BackupMode::Plaintext,
    };
    let archive = backup_inner(store.inner(), mode).map_err(cmd_err)?;
    write_backup_file(&path, &archive).map_err(|e| format!("Could not write backup file: {e}"))?;
    Ok(())
}

/// Write the archive to `path`, creating it with mode 0600 *before* the secret bytes land. The
/// plaintext export carries the same nsec/browse-key/transport-secret that is deliberately 0600 at
/// rest — the archive must never be created world-readable (default 0644), and opening with the
/// mode up front leaves no chmod-after window where the secret sits on disk world-readable.
/// Windows has no Unix mode bits (and its at-rest secrets are DPAPI ciphertext), so the default
/// write is unchanged there.
#[cfg(unix)]
fn write_backup_file(path: &str, archive: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `.mode()` only governs the create path; an existing inode is reused with its old
    // permissions. Set 0600 explicitly so the overwrite path can never leave a pre-existing 0644
    // world-readable while the plaintext nsec/browse-key lands in it.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(archive)
}

#[cfg(not(unix))]
fn write_backup_file(path: &str, archive: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, archive)
}

/// Does the backup at `path` need a passphrase? Lets the UI decide whether to prompt (cheap — no
/// KDF). Returns an error for a non-backup / unknown-version file.
#[tauri::command]
pub async fn peek_backup(path: String) -> CmdResult<bool> {
    let archive = std::fs::read(&path).map_err(|e| format!("Could not read backup file: {e}"))?;
    hb_core::is_encrypted_backup(&archive).map_err(cmd_err)
}

/// Restore a whole-directory backup, re-wrapping the secrets under the local at-rest scheme. The
/// archive header is self-describing, so `passphrase` is optional (an encrypted archive + `None`
/// is a reasoned error). Refuses a non-empty profile — the UI wipes first, then re-calls.
#[tauri::command]
pub async fn restore_data(
    passphrase: Option<String>,
    path: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<IdentityInfo> {
    let archive = std::fs::read(&path).map_err(|e| format!("Could not read backup file: {e}"))?;
    let pass = passphrase.map(Zeroizing::new);
    crate::backup::restore_inner(store.inner(), &archive, pass.as_ref().map(|p| p.as_str()))
        .map_err(cmd_err)?;

    let stored = store
        .load_identity()
        .map_err(cmd_err)?
        .ok_or("Backup restored, but it contained no identity.")?;
    let app_id = AppIdentity::from_stored(&stored).map_err(cmd_err)?;
    let info = IdentityInfo::from_identity(&app_id).map_err(cmd_err)?;

    *identity.write().await = Some(app_id);
    Ok(info)
}

/// Wipe all local data and reset in-memory state. Irreversible.
#[tauri::command]
pub async fn wipe_data(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, crate::transport_state::SharedEndpoint>,
) -> CmdResult<bool> {
    store.wipe().map_err(cmd_err)?;
    *identity.write().await = None;
    // M18 W4: the manifest plane must not outlive the identity it serves from. Its accept loop holds
    // a snapshot of the signing key and browse-key (`ManifestSource` is synchronous and cannot read a
    // live handle), so without this it would keep answering redemptions with manifests signed by the
    // key the user just wiped.
    crate::transport_state::close_plane(&endpoint).await;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::import_nsec_inner;
    use crate::identity_state::AppIdentity;
    use crate::store::DataStore;
    use nostr::prelude::ToBech32;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn nsec_of(id: &AppIdentity) -> String {
        id.identity.keys().secret_key().to_bech32().unwrap()
    }

    #[test]
    fn identity_generate_unique() {
        let a = AppIdentity::generate();
        let b = AppIdentity::generate();
        assert_ne!(a.npub(), b.npub(), "each generated identity is unique");
    }

    #[test]
    fn identity_info_exposes_npub_and_share_code() {
        let id = AppIdentity::generate();
        let info = super::IdentityInfo::from_identity(&id).unwrap();
        assert!(info.npub.starts_with("npub1"));
        assert!(info.share_code.starts_with("hbk1"));
        // The browse-key is NOT exposed as raw bytes (only via the hbk share code).
        assert!(!info.share_code.contains(&hex::encode(id.browse_key.bytes())),
            "raw browse-key bytes must never appear in the surfaced info");
    }

    #[test]
    fn key_storage_reports_plain_file_off_windows() {
        let id = AppIdentity::generate();
        let info = super::IdentityInfo::from_identity(&id).unwrap();
        #[cfg(target_os = "windows")]
        assert_eq!(info.key_storage, "os-encrypted");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(info.key_storage, "plain-file", "drives the Linux/macOS 0600 storage warning");
    }

    #[test]
    fn import_valid_nsec_yields_matching_npub() {
        let (_dir, store) = test_store();
        let source = AppIdentity::generate();
        let nsec = nsec_of(&source);
        let imported = import_nsec_inner(&store, &nsec).unwrap();
        assert_eq!(imported.npub(), source.npub(), "the imported npub matches the source key");
        // Persisted and reloadable.
        let reloaded = AppIdentity::from_stored(&store.load_identity().unwrap().unwrap()).unwrap();
        assert_eq!(reloaded.npub(), source.npub());
    }

    #[test]
    fn import_malformed_nsec_rejected_with_reason() {
        let (_dir, store) = test_store();
        // AppIdentity holds secrets and is intentionally not Debug, so inspect the Err side directly.
        let err = import_nsec_inner(&store, "not-a-valid-nsec").err().expect("malformed key is refused");
        assert!(!err.to_string().is_empty(), "rejection carries a reason");
        assert!(store.load_identity().unwrap().is_none(), "nothing persisted on a bad key");
    }

    #[test]
    fn import_when_identity_exists_refused() {
        let (_dir, store) = test_store();
        store.save_identity(&AppIdentity::generate().to_stored().unwrap()).unwrap();
        let nsec = nsec_of(&AppIdentity::generate());
        let err = import_nsec_inner(&store, &nsec).err().expect("import into an occupied profile is refused");
        assert!(err.to_string().contains("already exists"), "got {err}");
    }

    #[test]
    fn imported_identity_mints_fresh_browse_key() {
        // The imported npub is reused; the browse-key is freshly minted, not carried in.
        let source = AppIdentity::generate();
        let nsec = nsec_of(&source);
        let imported = AppIdentity::from_nsec(&nsec).unwrap();
        assert_eq!(imported.npub(), source.npub());
        assert_ne!(imported.browse_key.bytes(), source.browse_key.bytes(), "fresh browse-key");
    }

    #[test]
    #[cfg(unix)]
    fn backup_file_created_with_0600_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.hb");
        super::write_backup_file(path.to_str().unwrap(), b"secret archive bytes").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "backup export must not be group/world readable (mode {mode:#o})");
    }

    #[test]
    #[cfg(unix)]
    fn backup_file_overwrite_is_forced_to_0600_not_left_world_readable() {
        // Re-exporting over an existing filename must not inherit the old inode's 0644 — the
        // plaintext nsec/browse-key would land world-readable. Pre-create the path as 0644, then
        // export over it, then assert the result is 0600.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.hb");
        std::fs::write(&path, b"old export").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::write_backup_file(path.to_str().unwrap(), b"secret archive bytes").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "overwrite must force 0600, not leave the pre-existing 0644 (mode {mode:#o})");
    }
}
