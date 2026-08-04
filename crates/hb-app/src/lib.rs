#![forbid(unsafe_code)]

mod backup;
mod commands;
// M18 W1 — the manifest plane's connection helpers. W4 wired the verb, so the `dead_code` allow that
// marked the plane as built-but-uncalled is gone: `commands::fulfil` calls it.
mod conn;
mod dm_cache_store;
mod dm_quarantine;
mod error;
mod identity_state;
// v0.12.10 diagnostic build — file-based tracing subscriber for the beacon-loop wedge.
mod logging;
mod manifest_cache;
mod manifest_source;
mod net;
mod portable_update_logic;
mod presence;
mod single_instance;
mod store;
// M18 W1 — the manifest plane (INV-4′: a transport exists, structurally limited to manifests).
mod transport;
// M18 W4 — the plane's session lifecycle: one endpoint, one bounded accept loop, bound lazily.
mod transport_state;
mod update_logic;
mod watch;
// M20 W6 — the headless WAN integration harness (Suite WAN-P). In-crate so it can reach
// `pub(crate)` production internals (presence.rs, hb-net::fetch_online_presence). Never starts the
// Tauri/webkit runtime — the thin `bin/hb-wan-it.rs` wrapper calls `run_wan_it`.
mod wan_it;

use std::path::PathBuf;
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
/// Sender half of the snapshot-watch-loop cancel channel.
pub type SharedCancelWatch = Arc<tokio::sync::watch::Sender<bool>>;

/// Keeps the OS filesystem watcher alive for the process lifetime (dropping it stops watching).
/// Managed as state purely so it isn't dropped at the end of `setup`.
struct WatcherHandle(#[allow(dead_code)] std::sync::Mutex<notify::RecommendedWatcher>);

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

/// If an identity is on disk, populate `identity` for the session.
///
/// **No transport is started here, and that is a choice rather than an absence** (M18): the manifest
/// plane binds lazily.
///
/// **Binding happens in three places, not one** — an earlier version of this comment named only the
/// first and was therefore misleading about when a QUIC endpoint and its accept loop come up:
///   1. `transport_state::rebind_if_tickets_outstanding` at startup, when an unspent ticket is on
///      the books (a ticket outlives the session that issued it);
///   2. `send_full_list`, when the owner approves a request;
///   3. **`redeem_manifest_ticket`** — the ASKER binds too, in order to dial. Since owner ruling ③
///      that binding is **dial-only** (`Role::DialOnly`): no advertised ALPN and no accept loop, so
///      redeeming does not leave this node answering anyone who holds its node id.
///
/// So the honest statement is: a user who never sends *and never receives* a full list never opens
/// a QUIC endpoint.
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
    relay: net::SharedRelay,
    presence_cancel_rx: tokio::sync::watch::Receiver<bool>,
    store: DataStore,
    app: tauri::AppHandle,
    beacon: presence::SharedBeaconState,
) {
    // The wakeup counter is the L4 idle-guard hook; in prod it is written-and-ignored.
    tauri::async_runtime::spawn(presence::run_presence_loop(
        identity,
        store,
        relay,
        presence_cancel_rx,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        beacon,
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

/// Spawn the snapshot-watch task (M9): the bounded launch reconcile + the debounced fs-watch that
/// re-publishes a changed listing. Returns the OS watcher to keep alive. Respects
/// `snapshot_auto_update`: off ⇒ manual-only (no watcher, no launch reconcile) — the pre-M9
/// behaviour. The setting is read at startup; a runtime toggle takes effect on the next launch.
fn spawn_watch_task(
    store: DataStore,
    identity: SharedIdentity,
    relay: net::SharedRelay,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
    app: tauri::AppHandle,
) -> Option<notify::RecommendedWatcher> {
    let auto = store
        .load_settings()
        .ok()
        .flatten()
        .map(|s| s.snapshot_auto_update)
        .unwrap_or(true);
    if !auto {
        tracing::info!("snapshot auto-update is off — watch loop not started (manual-only)");
        return None;
    }

    let watched = watch::watched_from_store(&store);
    let roots: Vec<PathBuf> = watched.iter().map(|w| w.root.clone()).collect();
    let cfg = watch::WatchCfg { watched, ..Default::default() };

    // The OS watcher feeds fs events into the loop; kept alive by the caller (managed state).
    let (fs_tx, fs_rx) = tokio::sync::mpsc::channel(256);
    let watcher = match watch::spawn_fs_watcher(&roots, fs_tx) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("could not start filesystem watcher: {e}");
            None
        }
    };

    // Forward launch-reconcile progress to the UI as a Tauri event ("Checking snapshots… N of M").
    let (prog_tx, mut prog_rx) = tokio::sync::mpsc::unbounded_channel::<watch::SnapshotProgress>();
    let app_for_prog = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(p) = prog_rx.recv().await {
            let _ = app_for_prog
                .emit("snapshot-progress", serde_json::json!({ "checked": p.checked, "total": p.total }));
        }
    });

    let sink = Arc::new(watch::RelayPublishSink { store: store.clone(), identity, relay });
    tauri::async_runtime::spawn(watch::run_watch_loop(
        store,
        cfg,
        sink,
        fs_rx,
        cancel_rx,
        watch::WATCH_TICK,
        Some(prog_tx),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
    ));
    watcher
}

// ---------------------------------------------------------------------------
// WAN harness entry point
// ---------------------------------------------------------------------------

/// Entry point for the `hb-wan-it` headless WAN integration harness binary (M20 W6). Thin wrapper
/// around `wan_it::run`; the binary lives at `src/bin/hb-wan-it.rs` and calls this so the harness
/// reaches `pub(crate)` production code without starting the Tauri/webkit runtime.
pub async fn run_wan_it() -> std::process::ExitCode {
    wan_it::run().await
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

            // v0.12.10 diagnostic build: install the file-based tracing subscriber BEFORE the
            // background tasks spawn, so the presence loop's debug! lines land in the log. Non-fatal —
            // see `logging::install`. `data_dir` is the resolved app-data dir (same one `DataStore`
            // uses), and it now exists.
            logging::install(&data_dir);

            // M21 W1 — install the process-level rustls CryptoProvider before any network task
            // can run. The lock resolves rustls with BOTH providers compiled in (`ring` via
            // nostr-sdk/tungstenite, `aws-lc-rs` via iroh/quinn), so rustls cannot auto-pick a
            // default and any provider-less TLS use panics at
            // `rustls-0.23.38/src/crypto/mod.rs:249` (the `.expect()` inside
            // `get_default_or_install_from_crate_features`, which fires when neither
            // auto-selection nor a pre-installed default yields exactly one provider). The
            // v0.12.11 log named this as the launch storm: 12 panics in 32 s, then a race winner
            // installed a default and every subsequent connect succeeded. Installing `ring`
            // deterministically removes that race.
            //
            // `ring` (not `aws-lc-rs`) because iroh configures its TLS explicitly via its
            // `tls-ring` feature and is unaffected by the process default; nostr-sdk/tungstenite
            // is the path that consumes it. This runs before `spawn_background_tasks` and before
            // the ticket-outstanding iroh bind below, so no task in this process can TLS-handshake
            // without a provider installed. `install_default` returns `Err` when a provider is
            // already installed — not a failure for us, hence `let _ =`. Same idiom as
            // `wan_it::run`.
            let _ = rustls::crypto::ring::default_provider().install_default();

            let store = DataStore::new(data_dir);

            let identity: SharedIdentity = Arc::new(RwLock::new(None));
            // M12 W1: the one persistent shared relay client, lazily built on first network use.
            let relay: net::SharedRelay = net::new_shared();
            let staged_update: commands::update::SharedStagedUpdate = Arc::default();
            let online_cache: commands::online::SharedOnlineCache = Arc::default();
            let beacon: presence::SharedBeaconState = Arc::default();
            // M18 W4: the manifest plane's endpoint. `None` until something needs it — a user who
            // never sends a full list never binds a QUIC endpoint.
            let manifest_endpoint: transport_state::SharedEndpoint =
                transport_state::new_shared_endpoint();

            let (presence_cancel_tx, presence_cancel_rx) = tokio::sync::watch::channel(false);
            let presence_cancel: SharedCancelPresence = Arc::new(presence_cancel_tx);

            let (watch_cancel_tx, watch_cancel_rx) = tokio::sync::watch::channel(false);
            let watch_cancel: SharedCancelWatch = Arc::new(watch_cancel_tx);

            app.manage(store.clone());
            app.manage(Arc::clone(&identity));
            app.manage(Arc::clone(&relay));
            app.manage(Arc::clone(&presence_cancel));
            app.manage(Arc::clone(&watch_cancel));
            app.manage(Arc::clone(&staged_update));
            app.manage(Arc::clone(&online_cache));
            app.manage(Arc::clone(&beacon));
            app.manage(Arc::clone(&manifest_endpoint));
            // M13 Part A: serializes the announce cooldown's check-and-record step.
            app.manage(commands::topics::AnnounceGate(std::sync::Mutex::new(())));

            build_system_tray(app)?;

            let app_handle = app.handle().clone();

            restore_identity(store.clone(), Arc::clone(&identity));

            // M18 W4: a ticket is valid until redeemed, so an approval given last session must still
            // be dialable this one. `restore_identity` populates synchronously above, so the transport
            // key is available here. Binds ONLY if an unspent ticket is on the books — see
            // `transport_state::rebind_if_tickets_outstanding` for why that asymmetry is deliberate.
            {
                let endpoint = Arc::clone(&manifest_endpoint);
                let identity = Arc::clone(&identity);
                let store = store.clone();
                tauri::async_runtime::spawn(async move {
                    let keys = {
                        let guard = identity.read().await;
                        guard.as_ref().map(|id| {
                            (
                                id.identity.clone(),
                                id.browse_key.clone(),
                                id.transport_key.clone(),
                                id.npub(),
                            )
                        })
                    };
                    let Some((id, browse_key, transport_key, npub)) = keys else { return };
                    let source: Arc<dyn transport::ManifestSource> =
                        manifest_source::StoreManifestSource::new(store.clone(), id, browse_key);
                    transport_state::rebind_if_tickets_outstanding(
                        &endpoint,
                        &npub,
                        &identity,
                        &transport_key,
                        source,
                        &store,
                    )
                    .await;
                });
            }

            spawn_background_tasks(
                Arc::clone(&identity),
                Arc::clone(&relay),
                presence_cancel_rx,
                store.clone(),
                app_handle.clone(),
                Arc::clone(&beacon),
            );

            // M9: the snapshot-watch sibling task (single watcher per app — single-instance, M8).
            if let Some(watcher) =
                spawn_watch_task(store, Arc::clone(&identity), Arc::clone(&relay), watch_cancel_rx, app_handle)
            {
                app.manage(WatcherHandle(std::sync::Mutex::new(watcher)));
            }

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
            commands::collection::collection_source_accessible,
            commands::collection::delete_collection,
            commands::collection::publish_collection,
            commands::collection::unpublish_collection,
            commands::collection::update_collection_meta,
            commands::collection::update_collection_visibility,
            commands::collection::export_collection,
            commands::collection::export_manifest,
            commands::browse::import_manifest,
            // M18 W4 — the fulfil verb, both halves.
            commands::fulfil::send_full_list,
            commands::fulfil::redeem_manifest_ticket,
            commands::private::browse_private_collections,
            commands::browse::paste_key,
            commands::browse::share_code_info,
            commands::browse::follow,
            commands::browse::get_contacts,
            commands::browse::unfollow_contact,
            commands::browse::refresh_contact,
            commands::browse::set_contact_tags,
            commands::browse::set_contact_petname,
            commands::browse::search_peers,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::check_relay,
            commands::settings::relay_status,
            commands::settings::beacon_status,
            commands::settings::acknowledge_privacy_notice,
            commands::online::online_count,
            commands::chat::send_message,
            commands::chat::request_manifest,
            commands::chat::get_messages,
            commands::chat::dm_requests,
            commands::chat::dm_request_accept,
            commands::chat::dm_request_decline,
            commands::chat::dm_block,
            commands::chat::dm_unblock,
            commands::chat::dm_blocked_list,
            commands::chat::get_read_state,
            commands::chat::advance_read_watermark,
            commands::chat::get_manifest_asks,
            commands::sharing::get_share_settings,
            commands::groups::groups_get,
            commands::groups::groups_create,
            commands::groups::groups_rename,
            commands::groups::groups_delete,
            commands::groups::groups_assign,
            commands::groups::groups_unassign,
            commands::groups::contact_update_groups,
            commands::groups::private_audience_list,
            commands::groups::private_audience_set,
            commands::watches::watches_get,
            commands::watches::watches_create,
            commands::watches::watches_delete,
            commands::watches::watches_evaluate,
            commands::topics::topic_list,
            commands::topics::topic_create,
            commands::topics::topic_update_meta,
            commands::topics::topic_discover,
            commands::topics::topic_lookup,
            commands::topics::topic_join_public,
            commands::topics::topic_redeem_invite,
            commands::topics::topic_preview_invite,
            commands::topics::topic_request_join,
            commands::topics::topic_invite,
            commands::topics::topic_leave,
            commands::topics::topic_roster,
            commands::topics::topic_channel,
            commands::topics::topic_post,
            commands::topics::topic_announce,
            commands::topics::topic_announce_status,
            commands::topics::topic_announcements,
            commands::topics::topic_announce_seen,
            commands::topics::topic_announce_mark_seen,
            commands::update::check_update,
            commands::update::download_update,
            commands::update::apply_staged_update,
            commands::update::take_update_notice,
            commands::portable_update::updater_is_portable,
            commands::portable_update::check_portable_update,
            commands::portable_update::apply_portable_update,
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
