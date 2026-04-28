use hb_core::{DocType, SignedEnvelope, types::Profile};
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::DataStore,
    SharedIdentity, SharedRelay,
};

/// Returns true if a signed (published) profile exists on disk.
#[tauri::command]
pub async fn has_published_profile(store: State<'_, DataStore>) -> CmdResult<bool> {
    Ok(store.load_profile_signed().map_err(cmd_err)?.is_some())
}

#[tauri::command]
pub async fn save_profile(
    profile: Profile,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    store.save_profile_draft(&profile).map_err(cmd_err)
}

#[tauri::command]
pub async fn get_profile(store: State<'_, DataStore>) -> CmdResult<Option<Profile>> {
    if let Some(draft) = store.load_profile_draft().map_err(cmd_err)? {
        return Ok(Some(draft));
    }
    if let Some(env) = store.load_profile_signed().map_err(cmd_err)? {
        return Ok(Some(env.parse_payload().map_err(cmd_err)?));
    }
    Ok(None)
}

/// Check if a display name is available on the relay (anti-spoofing).
/// Returns `(available, taken_by_pubkey)`.
#[tauri::command]
pub async fn check_name_available(
    display_name: String,
    relay: State<'_, SharedRelay>,
) -> CmdResult<(bool, Option<String>)> {
    let result = relay.check_name(&display_name).await.map_err(cmd_err)?;
    Ok((result.available, result.taken_by))
}

#[tauri::command]
pub async fn publish_profile(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let guard = identity.read().await;
    let kp = guard
        .as_ref()
        .ok_or("No identity loaded. Generate a keypair first.")?;

    let my_pubkey = kp.hb_id();

    let profile = store
        .load_profile_draft()
        .map_err(cmd_err)?
        .ok_or("No profile draft found. Save a profile first.")?;

    // Anti-spoofing: warn if the name is already used by another key.
    // We pass the caller's own pubkey so the relay doesn't count it as taken.
    if let Ok(check) = relay.check_name(&profile.display_name).await {
        if !check.available {
            if let Some(ref owner) = check.taken_by {
                if *owner != my_pubkey {
                    return Err(format!(
                        "The display name '{}' is already in use by another identity on this relay.",
                        profile.display_name
                    ));
                }
            }
        }
    }
    // Note: if the relay is unreachable the check above returns Ok(available=true),
    // so publishing is never blocked solely by relay downtime.

    let envelope = SignedEnvelope::create(kp, DocType::Profile, &profile).map_err(cmd_err)?;
    store.save_profile_signed(&envelope).map_err(cmd_err)?;
    relay.publish("profile", &envelope).await.map_err(cmd_err)
}

#[tauri::command]
pub async fn unpublish_profile(store: State<'_, DataStore>) -> CmdResult<()> {
    let path = store.profile_signed_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(cmd_err)?;
    }
    Ok(())
}
