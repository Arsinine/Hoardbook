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

/// Fully validate a backup WITHOUT applying it (QURATOR-126 #15): derive the key (Argon2id),
/// decrypt the AEAD, and parse every tar entry to completion — the exact production restore path,
/// into a throwaway directory. `Ok(())` means a subsequent restore with the same passphrase into a
/// wiped profile will succeed. Writes nothing to the datastore, so the UI can gate the
/// irreversible wipe on this instead of the 72-byte header sniff `peek_backup` does.
#[tauri::command]
pub async fn validate_backup(passphrase: Option<String>, path: String) -> CmdResult<()> {
    let archive = std::fs::read(&path).map_err(|e| format!("Could not read backup file: {e}"))?;
    crate::backup::validate_inner(&archive, passphrase.as_deref()).map_err(cmd_err)
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

    // ── QURATOR-161 slice 3 — this file's commands, driven through the command itself ──────────
    //
    // Same technique as slices 1-2 (`e4d5cf6`, `33a6c00`): `tauri::test::mock_app()` mints genuine
    // `State<'_, T>` handles through the same StateManager the command macro uses at invoke time,
    // so every test below calls the REAL `#[tauri::command]` fn at its real signature — no mirror,
    // no `*_inner` shim, no restructuring. The pure halves that already had tests above stay put;
    // what this block adds is the guards' placement inside the command fns themselves.
    mod command_guards {
        use super::super::{
            backup_data, generate_keypair, get_identity, get_share_code, import_nsec, peek_backup,
            restore_data, validate_backup, validate_share_code, wipe_data,
        };
        use super::nsec_of;
        use crate::error::CmdResult;
        use crate::identity_state::{AppIdentity, SharedIdentity};
        use crate::store::DataStore;
        use crate::transport_state::{new_shared_endpoint, SharedEndpoint};
        use std::sync::Arc;
        use tauri::Manager;

        fn guard_app(identity_on_disk: bool) -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            if identity_on_disk {
                store.save_identity(&AppIdentity::generate().to_stored().unwrap()).unwrap();
            }
            let identity: SharedIdentity =
                Arc::new(tokio::sync::RwLock::new(None));
            app.manage(identity);
            app.manage(store);
            app.manage(new_shared_endpoint());
            app
        }

        async fn generate(app: &tauri::App<tauri::test::MockRuntime>) -> CmdResult<super::super::IdentityInfo> {
            generate_keypair(app.state::<DataStore>(), app.state::<SharedIdentity>()).await
        }

        // -- generate_keypair: three guards, one match ---------------------------------------------

        /// A populated profile refuses with its exact message AND the file on disk is untouched.
        #[tokio::test]
        async fn generate_refuses_when_an_identity_already_exists() {
            let app = guard_app(true);
            let before = std::fs::read(app.state::<DataStore>().identity_path()).unwrap();

            let err = generate(&app).await.unwrap_err();
            assert_eq!(
                err,
                "An identity already exists. Wipe data first to generate a new one."
            );

            let after = std::fs::read(app.state::<DataStore>().identity_path()).unwrap();
            assert_eq!(after, before, "a refused generate must not rewrite the identity file");
            assert!(
                app.state::<SharedIdentity>().read().await.is_none(),
                "the in-memory identity stays unset — a refused generate mints nothing"
            );
        }

        /// An identity file that EXISTS but cannot be read is the wipe-me recovery message, not a
        /// bare `cmd_err` — this is the branch that must not dead-end a user on the unreadable
        /// recovery screen. Pinned in two flavors: a file that parses as neither JSON nor an
        /// identity record, and a DIRECTORY sitting at the identity path (`exists()` is true, the
        /// read is what fails).
        #[tokio::test]
        async fn generate_flags_unreadable_existing_identity_data() {
            let app = guard_app(false);
            let store = app.state::<DataStore>();
            let path = store.identity_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Valid JSON, but not an identity record — `load_identity` fails at the parse, and the
            // path exists, so the guard (not `cmd_err`) owns the refusal.
            std::fs::write(&path, b"{\"nsec\": 3}").unwrap();

            let err = generate(&app).await.unwrap_err();
            assert!(
                err.starts_with("Existing identity data cannot be read"),
                "the unreadable-data guard must fire, got {err}"
            );
            assert!(
                err.contains("Wipe data"),
                "the recovery pointer is part of the guard's contract, got {err}"
            );

            // Flavor two: a directory at the identity path. `exists()` is still TRUE, so this is
            // the same arm — proving the guard keys on path existence, not on file-ness.
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir_all(&path).unwrap();
            let err = generate(&app).await.unwrap_err();
            assert!(
                err.starts_with("Existing identity data cannot be read"),
                "a directory-shaped identity path takes the same recovery arm, got {err}"
            );
            assert!(
                app.state::<SharedIdentity>().read().await.is_none(),
                "neither refusal populates the shared identity"
            );
        }

        /// The pass side: an EMPTY profile sails through all three guards and mints a full
        /// identity — persisted, returned, and loaded into the shared state.
        #[tokio::test]
        async fn generate_mints_an_identity_on_an_empty_profile() {
            let app = guard_app(false);
            let info = generate(&app).await.expect("an empty profile must generate");
            assert!(info.npub.starts_with("npub1"));
            assert!(info.share_code.starts_with("hbk1"));
            assert!(
                app.state::<DataStore>().identity_path().exists(),
                "the new identity is persisted"
            );
            assert!(
                app.state::<SharedIdentity>().read().await.is_some(),
                "the shared identity is populated by the command, not just written to disk"
            );
            let reloaded = AppIdentity::from_stored(
                &app.state::<DataStore>().load_identity().unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(reloaded.npub(), info.npub);
        }

        // -- import_nsec: the identity-exists refusal, through the command ------------------------

        /// The `import_nsec_inner` mirror-free refusal, now via the command: an occupied profile
        /// refuses an import with its exact message and does not clobber the incumbent identity.
        #[tokio::test]
        async fn import_nsec_command_refuses_an_occupied_profile() {
            let app = guard_app(true);
            let incumbent = app.state::<DataStore>().load_identity().unwrap().unwrap();
            let err = import_nsec(
                nsec_of(&AppIdentity::generate()),
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
            )
            .await
            .unwrap_err();
            assert_eq!(
                err,
                "An identity already exists. Wipe data first to import a different key."
            );
            let after = app.state::<DataStore>().load_identity().unwrap().unwrap();
            assert_eq!(
                after.nsec, incumbent.nsec,
                "a refused import must not overwrite the incumbent identity"
            );
            assert_eq!(
                after.browse_key_hex, incumbent.browse_key_hex,
                "nor its browse-key"
            );
        }

        // -- get_identity: the corrupted-stored-identity refusal -----------------------------------

        /// A persisted identity that parses as JSON but fails `AppIdentity::from_stored` is the
        /// "corrupted" refusal — this is the message a user with a tampered/garbled identity file
        /// actually sees.
        #[tokio::test]
        async fn get_identity_flags_a_corrupted_stored_identity() {
            let app = guard_app(false);
            let store = app.state::<DataStore>();
            let path = store.identity_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Valid JSON, every field present, but the nsec is garbage — parse succeeds, key fails.
            std::fs::write(
                &path,
                br#"{"version":1,"nsec":"garbage","browse_key_hex":"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff","transport_secret_hex":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            )
            .unwrap();

            let err = get_identity(app.state::<DataStore>(), app.state::<SharedIdentity>())
                .await
                .unwrap_err();
            assert!(
                err.starts_with("Stored identity is corrupted:"),
                "the corrupted-stored guard must fire, got {err}"
            );
        }

        /// The other side: a HEALTHY stored identity with a cold in-memory state is loaded, cached
        /// into the shared state, and returns real info.
        #[tokio::test]
        async fn get_identity_loads_and_caches_a_healthy_stored_identity() {
            let app = guard_app(true);
            let expected = app.state::<DataStore>().load_identity().unwrap().unwrap();
            let info = get_identity(app.state::<DataStore>(), app.state::<SharedIdentity>())
                .await
                .expect("a healthy on-disk identity must load")
                .expect("Some(info) — the file exists");
            let expected_npub = AppIdentity::from_stored(&expected).unwrap().npub();
            assert_eq!(info.npub, expected_npub);
            assert!(
                app.state::<SharedIdentity>().read().await.is_some(),
                "a successful get_identity populates the shared state (the cache the fast path reads)"
            );
        }

        /// The empty-profile side of the load: no in-memory state and no file → `Ok(None)`, the
        /// signal the UI uses to show onboarding.
        #[tokio::test]
        async fn get_identity_returns_none_on_an_empty_profile() {
            let app = guard_app(false);
            let out = get_identity(app.state::<DataStore>(), app.state::<SharedIdentity>())
                .await
                .expect("an empty profile is Ok(None), never an error");
            assert!(out.is_none());
        }

        // -- get_share_code: the no-identity-loaded refusal ----------------------------------------

        /// With nothing loaded, `get_share_code` refuses — the command-level partner of the
        /// in-memory fast path in `get_identity`.
        #[tokio::test]
        async fn get_share_code_refuses_when_no_identity_is_loaded() {
            let app = guard_app(false);
            let err = get_share_code(app.state::<SharedIdentity>()).await.unwrap_err();
            assert_eq!(err, "No identity loaded.");
        }

        /// The pass side: a loaded identity yields its `hbk…` code (the UI's copyable string).
        #[tokio::test]
        async fn get_share_code_returns_the_loaded_identitys_code() {
            let app = guard_app(false);
            let id = AppIdentity::generate();
            *app.state::<SharedIdentity>().write().await = Some(id);
            let code = get_share_code(app.state::<SharedIdentity>())
                .await
                .expect("a loaded identity has a share code");
            assert!(code.starts_with("hbk1"));
        }

        // -- validate_share_code: the codec gate ----------------------------------------------------

        /// The parse guard through the command: garbage is `false`, a real code is `true`.
        #[tokio::test]
        async fn validate_share_code_command_pins_the_codec_gate() {
            assert!(!validate_share_code("not a share code".into()).await.unwrap());
            let id = AppIdentity::generate();
            let real = id.share_code().unwrap();
            assert!(validate_share_code(real).await.unwrap());
        }

        // -- backup_data: the write-failure refusal + the 0600 contract through the command ------

        /// The whole command end-to-end: a plaintext export produces the on-disk archive (with the
        /// HBK magic), and writing it to an unwritable path is the "Could not write backup file"
        /// refusal — the command's own guard, not `backup_inner`'s.
        #[tokio::test]
        async fn backup_data_command_writes_the_archive_and_refuses_an_unwritable_path() {
            let app = guard_app(true);
            let dir = tempfile::tempdir().unwrap();
            let good = dir.path().join("export.hb");
            backup_data(
                None,
                good.to_str().unwrap().to_string(),
                app.state::<DataStore>(),
            )
            .await
            .expect("a populated profile exports");
            let bytes = std::fs::read(&good).unwrap();
            assert_eq!(&bytes[..4], b"HBK1", "the archive is a versioned HBK backup");
            assert!(
                bytes.len() > 72,
                "header plus a non-empty body (the identity entry is in it)"
            );

            // The guard: the archive built fine, but the destination cannot be written — a
            // DIRECTORY as the target makes `write_backup_file` fail at the open.
            let err = backup_data(
                None,
                dir.path().to_str().unwrap().to_string(),
                app.state::<DataStore>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.starts_with("Could not write backup file:"),
                "the write-failure guard must fire after the archive is built, got {err}"
            );
        }

        /// The mode is honoured through the command: a passphrase export is recognised as encrypted
        /// by `is_encrypted_backup` (the UI's "needs a passphrase" oracle).
        #[tokio::test]
        async fn backup_data_command_seals_a_passphrase_export() {
            let app = guard_app(true);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("locked.hb");
            backup_data(
                Some("a-real-passphrase-12chars".into()),
                path.to_str().unwrap().to_string(),
                app.state::<DataStore>(),
            )
            .await
            .expect("a populated profile exports with a passphrase");
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"HBK1");
            assert!(
                hb_core::is_encrypted_backup(&bytes).unwrap(),
                "a passphrase export must read as encrypted (mode byte 1)"
            );
        }

        // -- peek_backup / validate_backup / restore_data: the read-failure guard -----------------

        /// The shared first guard of all three backup-consuming commands: a missing file is the
        /// "Could not read backup file" refusal, verbatim, in each command.
        #[tokio::test]
        async fn backup_consumers_refuse_a_missing_backup_file() {
            let app = guard_app(false);
            let missing = "/nonexistent-hb-test/no-such-backup.hb".to_string();

            let err = peek_backup(missing.clone()).await.unwrap_err();
            assert!(err.starts_with("Could not read backup file:"), "peek_backup: {err}");

            let err = validate_backup(None, missing.clone()).await.unwrap_err();
            assert!(err.starts_with("Could not read backup file:"), "validate_backup: {err}");

            let err = restore_data(
                None,
                missing,
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
            )
            .await
            .unwrap_err();
            assert!(err.starts_with("Could not read backup file:"), "restore_data: {err}");
        }

        /// The pass side of `peek_backup`'s second guard: a file that IS readable but is NOT an
        /// HBK archive is the reasoned `cmd_err`, never a panic or a bare bool.
        #[tokio::test]
        async fn peek_backup_flags_a_non_backup_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("not-a-backup.hb");
            std::fs::write(&path, b"just some bytes, no magic").unwrap();
            let err = peek_backup(path.to_str().unwrap().to_string())
                .await
                .unwrap_err();
            assert!(
                err.starts_with("not a Hoardbook backup archive:"),
                "the header-sniff guard must fire, got {err}"
            );
        }

        /// The pass side for `validate_backup`: a REAL plaintext archive (made by `backup_data`
        /// itself, so the fixture is the production bytes) validates clean, and a TAMPERED body is
        /// refused.
        #[tokio::test]
        async fn validate_backup_accepts_a_real_archive_and_refuses_a_tampered_one() {
            let app = guard_app(true);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("export.hb");
            backup_data(
                None,
                path.to_str().unwrap().to_string(),
                app.state::<DataStore>(),
            )
            .await
            .expect("export for the fixture");
            validate_backup(None, path.to_str().unwrap().to_string())
                .await
                .expect("the production archive validates clean");

            // Tamper by REPLACING the tar body with garbage: a plaintext archive has no AEAD, so
            // this destroys the tar structure outright and `validate_inner` must refuse it —
            // deterministic, never dependent on which byte a flip happens to hit.
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.truncate(72);
            bytes.extend_from_slice(b"this is not a tar archive at all, just noise");
            let tampered = dir.path().join("tampered.hb");
            std::fs::write(&tampered, &bytes).unwrap();
            let err = validate_backup(None, tampered.to_str().unwrap().to_string())
                .await
                .unwrap_err();
            assert!(
                !err.is_empty(),
                "a tampered archive is refused with a reason, got {err}"
            );
        }

        /// The pass side for `restore_data`: restoring a real archive into an EMPTY profile
        /// succeeds end-to-end and loads the identity into the shared state.
        #[tokio::test]
        async fn restore_data_restores_a_real_archive_into_an_empty_profile() {
            let source = guard_app(true);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("export.hb");
            backup_data(
                None,
                path.to_str().unwrap().to_string(),
                source.state::<DataStore>(),
            )
            .await
            .expect("export for the fixture");
            let expected =
                AppIdentity::from_stored(&source.state::<DataStore>().load_identity().unwrap().unwrap())
                    .unwrap()
                    .npub();

            let target = guard_app(false);
            let info = restore_data(
                None,
                path.to_str().unwrap().to_string(),
                target.state::<DataStore>(),
                target.state::<SharedIdentity>(),
            )
            .await
            .expect("a real archive restores into an empty profile");
            assert_eq!(info.npub, expected, "the restored npub matches the source");
            assert!(
                target.state::<SharedIdentity>().read().await.is_some(),
                "restore loads the identity into the shared state"
            );
        }

        // -- wipe_data: the reset contract ----------------------------------------------------------

        /// Wipe clears the persisted identity AND the in-memory one AND resets the endpoint
        /// generation (the manifest-plane rule) — the full reset, observable end-to-end.
        #[tokio::test]
        async fn wipe_data_clears_disk_state_and_the_shared_identity() {
            let app = guard_app(true);
            // Load the session the way a running app would: a LIVE in-memory identity plus a bound
            // plane. Without this the reset assertions are vacuous — the state starts empty.
            *app.state::<SharedIdentity>().write().await = Some(AppIdentity::generate());
            let endpoint = app.state::<SharedEndpoint>();
            let generation_before = endpoint.read().await.generation;

            let ok = wipe_data(
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
                endpoint.clone(),
            )
            .await
            .expect("a populated profile wipes");
            assert!(ok);

            assert!(
                !app.state::<DataStore>().identity_path().exists(),
                "the identity file is gone"
            );
            assert!(
                app.state::<DataStore>().load_identity().unwrap().is_none(),
                "the store reads as empty afterwards"
            );
            assert!(
                app.state::<SharedIdentity>().read().await.is_none(),
                "the in-memory identity is cleared by the COMMAND, not left to the caller"
            );
            let generation_after = app.state::<SharedEndpoint>().read().await.generation;
            assert!(
                generation_after != generation_before,
                "close_plane bumps the endpoint generation — the manifest plane must not outlive the identity"
            );

            // Wiping an ALREADY-empty profile is Ok(true), not an error (the wipe-after-wipe path).
            assert!(
                wipe_data(
                    app.state::<DataStore>(),
                    app.state::<SharedIdentity>(),
                    app.state::<SharedEndpoint>(),
                )
                .await
                .expect("wipe is idempotent from empty")
            );
        }

        // The `Ok(None)` / `cmd_err` arms of `generate_keypair`'s third guard cannot be driven
        // without restructuring: reaching `Err(e)` with `identity_path()` NOT existing requires a
        // failure inside `load_identity` while its first statement (`!path.exists() → Ok(None)`)
        // is false — i.e. an unreadable filesystem the test cannot manufacture through the public
        // `DataStore` API. This is recorded here rather than hacked around.
    }
}
