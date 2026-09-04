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
    let (update, bytes) = take_staged_update(&staged)?;
    update.install(&bytes).map_err(cmd_err)?;
    app.restart();
}

/// The guard of [`apply_staged_update`]: atomically take the staged update, or refuse with the
/// user-visible message if nothing is staged. Extracted (QURATOR-182, 2026-09-04) after the
/// `portable_update.rs` precedent (`fb84b993`) so the refusal path is unit-drivable without an
/// `AppHandle`. ⚠ The 2026-08-31 note below is a prior session's engineering reasoning, NOT an
/// owner ruling — the 2026-08-31 owner rulings do not mention this command. It declined this arm
/// on ONE stated cost: "a signature change on the path that ships binaries to users". This
/// extraction pays no such cost — the command's signature is unchanged and the helper is private.
/// Behaviour-preserving: the condition (`inner.take()` yields `None`) and the refusal message are
/// byte-identical to the inline `let-else` that stood in the command. The install/relaunch tail
/// stays in the command — that half is the I/O boundary and is never driven from a test.
fn take_staged_update(staged: &SharedStagedUpdate) -> Result<(Update, Vec<u8>), String> {
    staged
        .lock()
        .unwrap()
        .inner
        .take()
        .ok_or_else(|| "No update is staged.".into())
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
pub async fn take_update_notice<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
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


// QURATOR-161 slice 6 -- the update commands. Owner ruling 2026-08-31 after an adversarial pass
// over what each of these would actually prove. The previous note here claimed all three were
// OWED; two of those three were wrong, in opposite directions.
//
//   check_update          -- has NO guard. Four lines: `updater_builder().build()?`, `check()?`,
//                            and a struct field copy. Every failure is a plugin error propagated
//                            by `cmd_err`. There is nothing here to pin; it should never have been
//                            counted as an untested guard.
//
//   download_update       -- genuinely unreachable. Its only branch (the already-up-to-date
//                            `Ok(None)`) sits downstream of `app.updater_builder().build()`, which
//                            reads the updater endpoint + minisign pubkey from the app config.
//                            `mock_app()` carries a dummy config with no updater keys, so the build
//                            refuses before any branch can be taken, and `tauri_plugin_updater::Update`
//                            has no public constructor, so a real one cannot be supplied either.
//
//   apply_staged_update   -- REVISED 2026-09-04 (QURATOR-182): the refusal arm IS now tested.
//                            ⚠ The note above is a prior SESSION's reasoning, not an owner ruling.
//                            It declined the arm on one stated cost, a signature change on the
//                            shipping path; that cost is not paid here. The
//                            `take_staged_update` extraction (same seam shape `fb84b993` gave
//                            `apply_portable_update_inner`) removed that cost. The install/relaunch
//                            tail stays in the command and remains untested -- it needs a real
//                            `Update`, which has no public constructor.
//
//   take_update_notice    -- the one with real, uncovered risk, and the only one made generic over
//                            `R: Runtime` (Tauri's documented mechanism for mock-runtime testing;
//                            `Wry` stays the concrete runtime in `lib.rs`). `should_show_update_notice`
//                            is CI-tested in `crate::update_logic`, but the WIRING around it is not:
//                            that the new version is persisted, and that a second call therefore
//                            goes quiet. Break the persistence and the "now running vX.Y" notice
//                            fires on every launch forever while the pure function's tests stay
//                            green. That is the bug class the test below exists for.
#[cfg(test)]
mod command_guards {
    use super::*;
    use tauri::Manager;

    /// The notice fires exactly ONCE after a version change: the first call reports it and persists
    /// the running version, the second call sees no change and goes quiet.
    ///
    /// This pins the command's WIRING, not its decision -- `should_show_update_notice` is already
    /// covered in `crate::update_logic`. What is uncovered until now is that `take_update_notice`
    /// actually writes the new version back, which is the whole once-only guarantee.
    ///
    /// MUTATION (reds this test): in `take_update_notice`, delete the `store.save_settings(&settings)?`
    /// call inside the `if settings.last_seen_version != current` block (keeping the in-memory
    /// assignment). The first call still reports the notice, but nothing is persisted, so the second
    /// call reports it again -- the `second.is_none()` assertion fails.
    #[tokio::test]
    async fn the_update_notice_fires_once_then_persists_and_goes_quiet() {
        let app = tauri::test::mock_app();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());

        // Seed a prior version that cannot equal the running one. Not the `""` default: that is the
        // fresh-install case, where `version_changed` decides on its own terms and would conflate
        // "no notice because nothing changed" with "no notice because we never persisted".
        let mut settings = store.load_settings().unwrap().unwrap_or_default();
        settings.last_seen_version = "0.0.0-before".into();
        store.save_settings(&settings).unwrap();
        app.manage(store);

        let current = app.package_info().version.to_string();

        let first = take_update_notice(app.handle().clone(), app.state::<DataStore>())
            .await
            .unwrap();
        assert_eq!(
            first.map(|n| n.version),
            Some(current.clone()),
            "a changed version must report the notice once, naming the running version"
        );

        let persisted = app
            .state::<DataStore>()
            .load_settings()
            .unwrap()
            .unwrap()
            .last_seen_version;
        assert_eq!(
            persisted, current,
            "the running version must be written back, or the notice can never go quiet"
        );

        let second = take_update_notice(app.handle().clone(), app.state::<DataStore>())
            .await
            .unwrap();
        assert!(
            second.is_none(),
            "the second call must go quiet -- this is the once-only guarantee"
        );
    }

    /// A fresh install (empty `last_seen_version`) shows NO "now running" notice --
    /// `version_changed` treats an empty `last_seen` as "nothing to have updated *from*" -- and
    /// still performs the first-write normalization: `last_seen_version` becomes the running
    /// version, so the next launch after a real upgrade compares against a genuine prior version.
    /// Without the write, a brand-new install that later upgrades would compare `""` vs the new
    /// version forever.
    ///
    /// MUTATION (reds this test): in `take_update_notice`, replace the line
    /// `let show = crate::update_logic::should_show_update_notice(&settings.last_seen_version, &current);`
    /// with `let show = settings.last_seen_version != current;` (inlining the comparison and
    /// dropping the empty-`last_seen` guard). The fresh install then reports a notice it must not
    /// show. Only THIS test reds -- the once-only test above seeds a non-empty prior version, so
    /// both its calls behave identically under the mutation. (Deleting the
    /// `store.save_settings(&settings)` call also reds this test, via the persisted-version assert;
    /// that edit reds the once-only test too. The `should_show_update_notice` call line occurs
    /// exactly once in this file, in `take_update_notice`.)
    #[tokio::test]
    async fn fresh_install_shows_no_notice_but_normalizes_last_seen() {
        let app = tauri::test::mock_app();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // Seed a settings file whose `last_seen_version` is the `""` default -- the fresh-install
        // state as the command sees it -- with an unroutable relay pinned (test-hazard rule).
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["ws://127.0.0.1:9".into()],
                ..Default::default()
            })
            .unwrap();
        app.manage(store);

        let current = app.package_info().version.to_string();

        let notice = take_update_notice(app.handle().clone(), app.state::<DataStore>())
            .await
            .unwrap();
        assert!(
            notice.is_none(),
            "a first-ever launch is not an update -- no 'now running vX.Y' notice"
        );

        let persisted = app
            .state::<DataStore>()
            .load_settings()
            .unwrap()
            .unwrap()
            .last_seen_version;
        assert_eq!(
            persisted, current,
            "the first write must normalize last_seen to the running version"
        );
    }

    /// The apply command's guard, via the extracted `take_staged_update` seam (the `fb84b993`
    /// shape given `apply_portable_update_inner`): with nothing staged, applying REFUSES with the
    /// user-visible message instead of proceeding toward install/relaunch. The command's Ok arm
    /// needs a real `Update` (no public constructor), so the refusal is pinned at the guard. The
    /// regression the 2026-08-31 note feared -- someone rewriting the take as `inner.unwrap()` --
    /// is caught here as a panic (red), not as a silent install.
    ///
    /// MUTATION (reds this test): in `take_staged_update`, edit the refusal message string
    /// `"No update is staged."` in any way (e.g. drop the trailing period). The string's only
    /// PRODUCTION occurrence is inside `take_staged_update`; the only other occurrences in this
    /// file are this test's own expected value and this comment block.
    #[test]
    fn applying_with_nothing_staged_refuses_with_the_no_update_staged_message() {
        let staged: SharedStagedUpdate = Default::default();
        assert_eq!(
            take_staged_update(&staged).err(),
            Some("No update is staged.".to_string()),
            "an empty stage must refuse, never fall through toward install"
        );
    }
}
