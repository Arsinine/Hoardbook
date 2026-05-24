use hb_core::{HoardbookKeypair, hb_id_decode, types::StoredKeypair};
use serde::Serialize;
use tauri::State;

use crate::{
    SharedEndpoint, SharedIdentity, SharedRelay,
    error::{CmdResult, cmd_err},
    store::DataStore,
};

#[derive(Debug, Clone, Serialize)]
pub struct IdentityInfo {
    pub hb_id: String,
    pub hb_id_short: String,
}

impl IdentityInfo {
    fn from_keypair(kp: &HoardbookKeypair) -> Self {
        let hb_id = kp.hb_id();
        let hb_id_short = shorten(&hb_id);
        Self { hb_id, hb_id_short }
    }
}

fn shorten(id: &str) -> String {
    if id.len() <= 14 {
        return id.to_string();
    }
    format!("{}…{}", &id[..8], &id[id.len() - 4..])
}

/// Generate a fresh keypair and persist it.
/// Errors if a keypair already exists — the frontend must call `rotate_keypair` to replace it.
#[tauri::command]
pub async fn generate_keypair(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<IdentityInfo> {
    if store.load_keypair().map_err(cmd_err)?.is_some() {
        return Err("A keypair already exists. Use rotate_keypair to replace it.".into());
    }

    let kp = HoardbookKeypair::generate();
    let stored = StoredKeypair {
        version: 1,
        hb_id: kp.hb_id(),
        private_key_hex: hex::encode(kp.private_key_bytes()),
    };

    store.save_keypair(&stored).map_err(cmd_err)?;
    let info = IdentityInfo::from_keypair(&kp);

    // Start iroh P2P endpoint with this keypair's key.
    crate::start_iroh_endpoint(kp.private_key_bytes(), (*store).clone(), (*endpoint).clone())
        .await
        .map_err(cmd_err)?;

    *identity.write().await = Some(kp);
    Ok(info)
}

/// Import a keypair from a previously exported JSON file.
#[tauri::command]
pub async fn import_keypair(
    path: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<IdentityInfo> {
    if store.load_keypair().map_err(cmd_err)?.is_some() {
        return Err("A keypair already exists. Wipe data first to import a different keypair.".into());
    }

    let json = std::fs::read_to_string(&path)
        .map_err(|e| format!("Could not read file: {e}"))?;
    let stored: StoredKeypair = serde_json::from_str(&json)
        .map_err(|e| format!("Invalid keypair file: {e}"))?;

    let private_bytes: [u8; 32] = hex::decode(&stored.private_key_hex)
        .map_err(|e| format!("Invalid private key hex: {e}"))?
        .try_into()
        .map_err(|_| "Private key must be exactly 32 bytes".to_string())?;

    let kp = HoardbookKeypair::from_bytes(&private_bytes);
    if kp.hb_id() != stored.hb_id {
        return Err("Keypair file is corrupted: public key does not match the private key".into());
    }

    store.save_keypair(&stored).map_err(cmd_err)?;
    let info = IdentityInfo::from_keypair(&kp);

    crate::start_iroh_endpoint(&private_bytes, (*store).clone(), (*endpoint).clone())
        .await
        .map_err(cmd_err)?;

    *identity.write().await = Some(kp);
    Ok(info)
}

/// Load the current keypair from disk. Returns `None` if no keypair exists yet.
#[tauri::command]
pub async fn get_identity(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<Option<IdentityInfo>> {
    if let Some(ref kp) = *identity.read().await {
        return Ok(Some(IdentityInfo::from_keypair(kp)));
    }

    let stored = match store.load_keypair().map_err(cmd_err)? {
        Some(s) => s,
        None => return Ok(None),
    };

    let bytes: [u8; 32] = hex::decode(&stored.private_key_hex)
        .map_err(cmd_err)?
        .try_into()
        .map_err(|_| "keypair file has invalid length".to_string())?;

    let kp = HoardbookKeypair::from_bytes(&bytes);
    let info = IdentityInfo::from_keypair(&kp);
    *identity.write().await = Some(kp);
    Ok(Some(info))
}

/// Return the raw Hoardbook ID string for sharing.
#[tauri::command]
pub async fn get_hb_id(identity: State<'_, SharedIdentity>) -> CmdResult<String> {
    identity
        .read()
        .await
        .as_ref()
        .map(|kp| kp.hb_id())
        .ok_or_else(|| "No identity loaded.".into())
}

/// Validate a Hoardbook ID string (checksum check only, no network).
#[tauri::command]
pub async fn validate_hb_id(hb_id: String) -> CmdResult<bool> {
    Ok(hb_id_decode(&hb_id).is_ok())
}

/// Return the current iroh EndpointAddr as a JSON string, or None if not initialised.
#[tauri::command]
pub async fn get_node_addr(endpoint: State<'_, SharedEndpoint>) -> CmdResult<Option<String>> {
    let guard = endpoint.read().await;
    let addr = guard.as_ref().map(|ep| {
        serde_json::to_string(&ep.addr()).unwrap_or_default()
    });
    Ok(addr)
}

/// Export the stored keypair as a JSON string for the user to save to a file.
#[tauri::command]
pub async fn export_keypair(store: State<'_, DataStore>) -> CmdResult<String> {
    let stored = store
        .load_keypair()
        .map_err(cmd_err)?
        .ok_or("No keypair to export.")?;
    serde_json::to_string_pretty(&stored).map_err(cmd_err)
}

/// Write the exported keypair JSON to a user-chosen absolute path.
#[tauri::command]
pub async fn save_keypair_file(path: String, store: State<'_, DataStore>) -> CmdResult<()> {
    let stored = store
        .load_keypair()
        .map_err(cmd_err)?
        .ok_or("No keypair to export.")?;
    let json = serde_json::to_string_pretty(&stored).map_err(cmd_err)?;
    std::fs::write(&path, json).map_err(cmd_err)?;
    Ok(())
}

/// Wipe all local data and reset in-memory state. Irreversible.
/// Returns `true` if the relay was also notified (peer removed from directory),
/// `false` if the relay could not be reached (local wipe still completes either way).
#[tauri::command]
pub async fn wipe_data(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<bool> {
    // Relay no longer stores peer data (profile/collections served via iroh),
    // so there is nothing to deactivate on the relay side.
    store.wipe().map_err(cmd_err)?;
    *identity.write().await = None;
    relay.set_relay_urls(vec![]);

    // Close and clear the iroh endpoint.
    let mut ep_guard = endpoint.write().await;
    if let Some(ep) = ep_guard.take() {
        ep.close().await;
    }

    Ok(true)
}

// ---------------------------------------------------------------------------
// Tests — T12 acceptance criteria
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DataStore;
    use hb_core::{HoardbookKeypair, types::StoredKeypair};
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn keypair_generate_unique() {
        let kp1 = HoardbookKeypair::generate();
        let kp2 = HoardbookKeypair::generate();
        assert_ne!(kp1.hb_id(), kp2.hb_id(), "each generated keypair must be unique");
    }

    #[test]
    fn export_import_roundtrip() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        let stored = StoredKeypair {
            version: 1,
            hb_id: kp.hb_id(),
            private_key_hex: hex::encode(kp.private_key_bytes()),
        };
        store.save_keypair(&stored).unwrap();

        // Export: serialize to JSON string (portable, unencrypted — same as save_keypair_file output).
        let exported = serde_json::to_string_pretty(&stored).unwrap();

        // Simulate import: parse the plain JSON backup and save through the platform store.
        let reimported: StoredKeypair = serde_json::from_str(&exported).unwrap();
        assert_eq!(reimported.hb_id, stored.hb_id, "reimported hb_id must match");
        assert_eq!(
            reimported.private_key_hex, stored.private_key_hex,
            "reimported private key must match"
        );

        // Reload from disk via the platform-specific path.
        let loaded = store.load_keypair().unwrap().unwrap();
        assert_eq!(loaded.hb_id, stored.hb_id);
        assert_eq!(loaded.private_key_hex, stored.private_key_hex);
    }

    #[test]
    fn stored_keypair_debug_redacts() {
        let kp = HoardbookKeypair::generate();
        let stored = StoredKeypair {
            version: 1,
            hb_id: kp.hb_id(),
            private_key_hex: hex::encode(kp.private_key_bytes()),
        };
        let debug_str = format!("{stored:?}");
        assert!(
            !debug_str.contains(&stored.private_key_hex),
            "Debug output must not contain the actual private key"
        );
        assert!(
            debug_str.contains("[REDACTED]"),
            "Debug output must contain [REDACTED] placeholder"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn keypair_file_has_mode_600() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        let stored = StoredKeypair {
            version: 1,
            hb_id: kp.hb_id(),
            private_key_hex: hex::encode(kp.private_key_bytes()),
        };
        store.save_keypair(&stored).unwrap();

        let path = store.keypair_path();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        // Mode bits: 0o600 = owner r+w only.
        assert_eq!(mode & 0o777, 0o600, "keypair.json must have mode 600");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn keypair_file_is_readable_json() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        let stored = StoredKeypair {
            version: 1,
            hb_id: kp.hb_id(),
            private_key_hex: hex::encode(kp.private_key_bytes()),
        };
        store.save_keypair(&stored).unwrap();

        // On Linux the file is plain JSON (not encrypted).
        let raw = std::fs::read_to_string(store.keypair_path()).unwrap();
        assert!(raw.contains("hb_id"), "Linux keypair file must be plain JSON");
        assert!(!raw.is_empty());
    }
}
