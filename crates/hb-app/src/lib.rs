#![forbid(unsafe_code)]

mod backup;
mod commands;
mod error;
mod identity_state;
mod net;
mod presence;
mod single_instance;
mod store;
mod update_logic;

use std::sync::Arc;
use store::DataStore;
use tauri::{
    Emitter, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::RwLock;

pub use identity_state::{AppIdentity, SharedIdentity};

/// Managed state types — Arc-wrapped so they can be cloned into background tasks.
/// Sender half of the presence-loop cancel channel. Send `true` to stop the task.
pub type SharedCancelPresence = Arc<tokio::sync::watch::Sender<bool>>;

// ---------------------------------------------------------------------------
// Setup helpers — each owns one concern from the setup closure
// ---------------------------------------------------------------------------

/// Create the app data directory with restrictive permissions (mode 0700 on Unix).
fn create_app_data_dir(path: &std::path::Path) {
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .expect("could not create app data dir");
    }
    #[cfg(target_os = "windows")]
    std::fs::create_dir_all(path).expect("could not create app data dir");
}

/// Build the system tray with "Open Hoardbook" and "Quit" items.
fn build_system_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let show_item = MenuItemBuilder::with_id("show", "Open Hoardbook").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let tray_menu = MenuBuilder::new(app).items(&[&show_item, &quit_item]).build()?;
    TrayIconBuilder::with_id("hb_tray")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&tray_menu)
        .tooltip("Hoardbook")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "quit" => app.exit(0),
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// If an identity is on disk, populate `identity` for the session. (No transport to start —
/// Hoardbook moves no files; file transfer lives in the Mascara companion.)
fn restore_identity(store: DataStore, identity: SharedIdentity) {
    let stored = match store.load_identity() {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => {
            tracing::error!("Failed to load identity on startup: {e:#}");
            return;
        }
    };
    let app_id = match AppIdentity::from_stored(&stored) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Stored identity is corrupted: {e:#}");
            return;
        }
    };
    // Populate synchronously so the presence task has an identity at its first fire.
    *identity.blocking_write() = Some(app_id);
}

/// Spawn the long-running background tasks: the presence-publish loop + the update check.
fn spawn_background_tasks(
    identity: SharedIdentity,
    presence_cancel_rx: tokio::sync::watch::Receiver<bool>,
    store: DataStore,
    app: tauri::AppHandle,
) {
    tauri::async_runtime::spawn(presence::run_presence_loop(
        identity,
        store,
        presence_cancel_rx,
    ));

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        match app.updater_builder().build() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    tracing::info!("Update available: v{}", update.version);
                    let _ = app.emit("update-available", &update.version);
                }
                Ok(None) => tracing::debug!("App is up to date"),
                Err(e) => tracing::debug!("Background update check failed: {e}"),
            },
            Err(e) => tracing::debug!("Updater not configured: {e}"),
        }
    });
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

pub fn run() {
    let mut builder = tauri::Builder::default();

    // Single-instance enforcement (M8): a second launch focuses the existing window instead of
    // spawning a duplicate (a duplicate opens its own relay connections + presence loop, so it would
    // double-publish presence under one npub — collapsing to one process collapses that to one
    // publisher). The v2 docs require this be the FIRST plugin so the second-instance argv is
    // captured before other setup. Desktop-only; mobile has no single-instance concern.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // TODO(M-later): route second-instance argv as an "open this share code" deep-link.
            use single_instance::{FocusAction, FocusableWindow, focus_existing};
            // Absorbing call-site expression (F10): a stale/destroyed handle's method errors are
            // swallowed inside `focus_existing`, never panicking the surviving instance.
            let win = app.get_webview_window("main");
            if focus_existing(win.as_ref().map(|w| w as &dyn FocusableWindow)) == FocusAction::NoWindow
            {
                tracing::warn!("single-instance: second launch but no main window to focus");
            }
        }));
    }

    builder
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("could not resolve app data dir");

            create_app_data_dir(&data_dir);

            let store = DataStore::new(data_dir);

            let identity: SharedIdentity = Arc::new(RwLock::new(None));
            let staged_update: commands::update::SharedStagedUpdate = Arc::default();

            let (presence_cancel_tx, presence_cancel_rx) = tokio::sync::watch::channel(false);
            let presence_cancel: SharedCancelPresence = Arc::new(presence_cancel_tx);

            app.manage(store.clone());
            app.manage(Arc::clone(&identity));
            app.manage(Arc::clone(&presence_cancel));
            app.manage(Arc::clone(&staged_update));

            build_system_tray(app)?;

            let app_handle = app.handle().clone();

            restore_identity(store.clone(), Arc::clone(&identity));

            spawn_background_tasks(
                Arc::clone(&identity),
                presence_cancel_rx,
                store,
                app_handle,
            );

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides to tray; "Quit" from the tray calls app.exit(0) instead.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::identity::generate_keypair,
            commands::identity::import_nsec,
            commands::identity::get_identity,
            commands::identity::get_share_code,
            commands::identity::validate_share_code,
            commands::identity::backup_data,
            commands::identity::peek_backup,
            commands::identity::restore_data,
            commands::identity::wipe_data,
            commands::profile::save_profile,
            commands::profile::get_profile,
            commands::profile::publish_profile,
            commands::profile::unpublish_profile,
            commands::profile::has_published_profile,
            commands::collection::scan_directory,
            commands::collection::list_subdirs,
            commands::collection::get_collections,
            commands::collection::delete_collection,
            commands::collection::publish_collection,
            commands::collection::update_collection_meta,
            commands::collection::export_collection,
            commands::browse::paste_key,
            commands::browse::follow,
            commands::browse::get_contacts,
            commands::browse::unfollow_contact,
            commands::browse::refresh_contact,
            commands::browse::set_contact_tags,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::check_relay,
            commands::settings::acknowledge_privacy_notice,
            commands::chat::send_message,
            commands::chat::get_messages,
            commands::sharing::get_share_settings,
            commands::sharing::save_share_settings,
            commands::groups::groups_get,
            commands::groups::groups_create,
            commands::groups::groups_rename,
            commands::groups::groups_delete,
            commands::groups::groups_assign,
            commands::groups::groups_unassign,
            commands::groups::contact_update_groups,
            commands::watches::watches_get,
            commands::watches::watches_create,
            commands::watches::watches_delete,
            commands::watches::watches_evaluate,
            commands::update::check_update,
            commands::update::download_update,
            commands::update::apply_staged_update,
            commands::update::take_update_notice,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Hoardbook")
        // Obsidian deferred-install: apply a staged update as the app quits (Auto mode), so the
        // running-exe lock never bites and the user saw no mid-session interruption.
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                commands::update::apply_staged_on_exit(app);
            }
        });
}
