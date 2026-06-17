//! Identity commands on the v0.9 Nostr model: a secp256k1 `npub`, a bound iroh transport key, and
//! the account browse-key (the `hbk` share code). Replaces the legacy Ed25519 keypair identity.

use serde::Serialize;
use tauri::State;

use crate::{
    identity_state::{AppIdentity, SharedIdentity},
    error::{CmdResult, cmd_err},
    store::DataStore,
    SharedDownloadRegistry, SharedEndpoint,
};

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
    app: tauri::AppHandle,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, SharedEndpoint>,
    registry: State<'_, SharedDownloadRegistry>,
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
    let iroh_secret = app_id.iroh_secret;

    // iroh endpoint startup is non-fatal: identity is committed, endpoint retried on next launch.
    if let Err(e) = crate::start_iroh_endpoint(
        &iroh_secret, (*store).clone(), (*endpoint).clone(), app, (*registry).clone(),
    ).await {
        tracing::warn!("iroh endpoint startup failed after identity generate: {e}");
    }

    *identity.write().await = Some(app_id);
    Ok(info)
}

/// Import an identity from a previously exported JSON file (a `StoredIdentity`).
#[tauri::command]
pub async fn import_keypair(
    app: tauri::AppHandle,
    path: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, SharedEndpoint>,
    registry: State<'_, SharedDownloadRegistry>,
) -> CmdResult<IdentityInfo> {
    if store.load_identity().map_err(cmd_err)?.is_some() {
        return Err("An identity already exists. Wipe data first to import a different one.".into());
    }

    let json = std::fs::read_to_string(&path)
        .map_err(|e| format!("Could not read file: {e}"))?;
    let stored: crate::store::StoredIdentity = serde_json::from_str(&json)
        .map_err(|e| format!("Invalid identity file: {e}"))?;

    let app_id = AppIdentity::from_stored(&stored)
        .map_err(|e| format!("Identity file is corrupted: {e}"))?;

    store.save_identity(&stored).map_err(cmd_err)?;
    let info = IdentityInfo::from_identity(&app_id).map_err(cmd_err)?;
    let iroh_secret = app_id.iroh_secret;

    if let Err(e) = crate::start_iroh_endpoint(
        &iroh_secret, (*store).clone(), (*endpoint).clone(), app, (*registry).clone(),
    ).await {
        tracing::warn!("iroh endpoint startup failed after identity import: {e}");
    }

    *identity.write().await = Some(app_id);
    Ok(info)
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

/// Return the current iroh EndpointAddr as a JSON string, or None if not initialised.
#[tauri::command]
pub async fn get_node_addr(endpoint: State<'_, SharedEndpoint>) -> CmdResult<Option<String>> {
    let guard = endpoint.read().await;
    let addr = guard.as_ref().map(|ep| serde_json::to_string(&ep.addr()).unwrap_or_default());
    Ok(addr)
}

/// Export the stored identity as a JSON string for the user to save to a file.
#[tauri::command]
pub async fn export_keypair(identity: State<'_, SharedIdentity>) -> CmdResult<String> {
    let guard = identity.read().await;
    let id = guard.as_ref().ok_or("No identity loaded.")?;
    let stored = id.to_stored().map_err(cmd_err)?;
    serde_json::to_string_pretty(&stored).map_err(cmd_err)
}

/// Write the exported identity JSON to a user-chosen absolute path.
#[tauri::command]
pub async fn save_keypair_file(
    path: String,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<()> {
    let guard = identity.read().await;
    let id = guard.as_ref().ok_or("No identity loaded.")?;
    let stored = id.to_stored().map_err(cmd_err)?;
    let json = serde_json::to_string_pretty(&stored).map_err(cmd_err)?;
    std::fs::write(&path, json).map_err(cmd_err)?;
    Ok(())
}

/// Wipe all local data and reset in-memory state. Irreversible.
#[tauri::command]
pub async fn wipe_data(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<bool> {
    store.wipe().map_err(cmd_err)?;
    *identity.write().await = None;

    let mut ep_guard = endpoint.write().await;
    if let Some(ep) = ep_guard.take() {
        ep.close().await;
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::identity_state::AppIdentity;
    use crate::store::DataStore;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn identity_generate_unique() {
        let a = AppIdentity::generate();
        let b = AppIdentity::generate();
        assert_ne!(a.npub(), b.npub(), "each generated identity is unique");
    }

    #[test]
    fn export_import_roundtrip_via_store() {
        let (_dir, store) = test_store();
        let app_id = AppIdentity::generate();
        let npub = app_id.npub();
        let stored = app_id.to_stored().unwrap();
        store.save_identity(&stored).unwrap();

        // Export = the JSON of StoredIdentity.
        let exported = serde_json::to_string_pretty(&stored).unwrap();
        let reimported: crate::store::StoredIdentity = serde_json::from_str(&exported).unwrap();
        let back = AppIdentity::from_stored(&reimported).unwrap();
        assert_eq!(back.npub(), npub, "reimported npub must match");

        let loaded = store.load_identity().unwrap().unwrap();
        let loaded_id = AppIdentity::from_stored(&loaded).unwrap();
        assert_eq!(loaded_id.npub(), npub);
    }
}
