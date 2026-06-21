//! Auto-updater commands — the Obsidian **deferred-install** pattern (spec §Auto-updater threat
//! model). `download()` (background, minisign-verified by the plugin) is separated from `install()`
//! (deferred to quit / next launch); there is **no immediate `app.restart()`** after a background
//! download. A staged update applies on app-quit (Auto), or via an explicit user "restart & apply"
//! (Confirm). The "now running vX.Y" notice fires once after a version change (visible-after).
//!
//! The pure decision logic (`crate::update_logic`) is CI-tested; the actual download/verify/apply
//! over a signed release is the **I/O boundary** and is not runnable in the offline dev env
//! (decision #7/#8).

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::error::{cmd_err, CmdResult};
use crate::store::DataStore;

/// A downloaded-but-not-yet-applied update: the verified `Update` (its install config) + the
/// downloaded installer bytes. Stored in managed state between `download_update` and the deferred
/// `install` (on quit / restart). `Update` is a Tauri `Resource` (Send + Sync), so it can live here.
#[derive(Default)]
pub struct StagedUpdate {
    inner: Option<(Update, Vec<u8>)>,
}

pub type SharedStagedUpdate = Arc<Mutex<StagedUpdate>>;

#[derive(Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub body: Option<String>,
}

/// The one-time "now running vX.Y — what's new" notice (visible-after).
#[derive(Serialize)]
pub struct UpdateNotice {
    pub version: String,
}

/// Check whether a newer release is available. Returns None if up to date, or an error if the
/// updater is not configured (pubkey not set in tauri.conf.json).
#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> CmdResult<Option<UpdateInfo>> {
    let updater = app.updater_builder().build().map_err(cmd_err)?;
    let update = updater.check().await.map_err(cmd_err)?;
    Ok(update.map(|u| UpdateInfo { version: u.version, body: u.body }))
}

/// Download + minisign-verify the latest release in the background and **stage** it. Does NOT
/// install or restart — the install is deferred to app-quit / next-launch. Returns the staged
/// version, or None if already up to date.
#[tauri::command]
pub async fn download_update(
    app: tauri::AppHandle,
    staged: State<'_, SharedStagedUpdate>,
) -> CmdResult<Option<String>> {
    let updater = app.updater_builder().build().map_err(cmd_err)?;
    let Some(update) = updater.check().await.map_err(cmd_err)? else {
        return Ok(None);
    };
    let version = update.version.clone();
    // The plugin verifies the minisign signature during download — unconditional.
    let bytes = update.download(|_, _| {}, || {}).await.map_err(cmd_err)?;
    staged.lock().unwrap().inner = Some((update, bytes));
    Ok(Some(version))
}

/// Apply a staged update **now** and relaunch — the explicit user action (the Obsidian default
/// instead applies silently on quit). Re-acquires the `Update` for its install config and installs
/// the already-downloaded bytes. Errors if nothing is staged.
#[tauri::command]
pub async fn apply_staged_update(
    app: tauri::AppHandle,
    staged: State<'_, SharedStagedUpdate>,
) -> CmdResult<()> {
    let inner = staged.lock().unwrap().inner.take();
    let Some((update, bytes)) = inner else {
        return Err("No update is staged.".into());
    };
    update.install(&bytes).map_err(cmd_err)?;
    app.restart();
}

/// Deferred Obsidian apply, called from the app's `ExitRequested` hook: if an update is staged,
/// install it as the app quits (so the running-exe lock never bites and the user saw no mid-session
/// interruption). Best-effort — logged, never panics. **I/O boundary: not exercised in the offline
/// dev env.**
pub fn apply_staged_on_exit(app: &tauri::AppHandle) {
    let staged = app.state::<SharedStagedUpdate>();
    let inner = staged.lock().unwrap().inner.take();
    if let Some((update, bytes)) = inner {
        match update.install(&bytes) {
            Ok(()) => tracing::info!("staged update applied on exit"),
            Err(e) => tracing::warn!("deferred update install failed on exit: {e}"),
        }
    }
}

/// The once-per-version "now running vX.Y" notice (spec's visible-after guardrail). Compares the
/// persisted `last_seen_version` against the running app version (exact string), persists the
/// running version, and returns the notice exactly once after a version change.
#[tauri::command]
pub async fn take_update_notice(
    app: tauri::AppHandle,
    store: State<'_, DataStore>,
) -> CmdResult<Option<UpdateNotice>> {
    let current = app.package_info().version.to_string();
    let mut settings = store.load_settings().map_err(cmd_err)?.unwrap_or_default();
    let show = crate::update_logic::should_show_update_notice(&settings.last_seen_version, &current);
    if settings.last_seen_version != current {
        settings.last_seen_version = current.clone();
        store.save_settings(&settings).map_err(cmd_err)?;
    }
    Ok(show.then_some(UpdateNotice { version: current }))
}
