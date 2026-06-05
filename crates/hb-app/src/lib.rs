#![forbid(unsafe_code)]

mod commands;
mod dht_service;
mod error;
mod heartbeat;
mod node;
mod relay;
mod store;
mod transfer;

use std::sync::Arc;
use hb_core::HoardbookKeypair;
use relay::RelayClient;
use store::DataStore;
use tauri::{
    Emitter, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::RwLock;

/// Managed state types — Arc-wrapped so they can be cloned into background tasks.
pub type SharedIdentity           = Arc<RwLock<Option<HoardbookKeypair>>>;
pub type SharedRelay              = Arc<RelayClient>;
pub type SharedEndpoint           = Arc<RwLock<Option<iroh::Endpoint>>>;
pub type SharedDownloadRegistry   = Arc<transfer::DownloadRegistry>;
pub type SharedDmQueue            = node::SharedDmQueue;
/// Sender half of the heartbeat-cancel channel. Send `true` to stop the task.
pub type SharedCancelHeartbeat = Arc<tokio::sync::watch::Sender<bool>>;
/// Sender for the DHT service cancel/trigger channel.
/// Send `false` to wake the announce loop immediately; `true` to shut it down.
pub type SharedDhtCancel = Arc<tokio::sync::watch::Sender<bool>>;

// ---------------------------------------------------------------------------
// iroh endpoint lifecycle helper
// ---------------------------------------------------------------------------

/// Create (or replace) the iroh P2P endpoint from the given private key bytes,
/// persist it in `endpoint_state`, and spawn the unified accept loop.
pub(crate) async fn start_iroh_endpoint(
    private_bytes: &[u8; 32],
    store: DataStore,
    endpoint_state: SharedEndpoint,
    dm_queue: SharedDmQueue,
    own_hb_id: String,
    app: tauri::AppHandle,
    download_registry: SharedDownloadRegistry,
) -> anyhow::Result<()> {
    let secret_key = iroh::SecretKey::from_bytes(private_bytes);

    let new_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .alpns(vec![transfer::XFER_ALPN.to_vec(), node::NODE_ALPN.to_vec()])
        .bind()
        .await?;

    let mut guard = endpoint_state.write().await;

    // Gracefully close any previous endpoint (its accept loop will exit naturally).
    if let Some(old) = guard.take() {
        old.close().await;
    }

    // Spawn the unified protocol accept loop.
    let server_ep = new_ep.clone();
    tauri::async_runtime::spawn(run_accept_loop(server_ep, store, own_hb_id, dm_queue, app, download_registry));

    *guard = Some(new_ep);
    Ok(())
}

const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// Dispatch incoming iroh connections by ALPN to the appropriate protocol handler.
async fn run_accept_loop(
    endpoint: iroh::Endpoint,
    store: DataStore,
    own_hb_id: String,
    dm_queue: SharedDmQueue,
    app: tauri::AppHandle,
    download_registry: SharedDownloadRegistry,
) {
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        let incoming = match endpoint.accept().await {
            Some(inc) => inc,
            None => {
                tracing::debug!("iroh endpoint closed — accept loop exiting");
                break;
            }
        };

        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => { tracing::warn!("connection semaphore closed"); break; }
        };

        let store_clone = store.clone();
        let queue_clone = dm_queue.clone();
        let hb_id_clone = own_hb_id.clone();
        let app_clone = app.clone();
        let registry_clone = download_registry.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let accepting = match incoming.accept() {
                Ok(a) => a,
                Err(e) => { tracing::debug!("iroh incoming reject: {e}"); return; }
            };
            let conn = match accepting.await {
                Ok(c) => c,
                Err(e) => { tracing::debug!("iroh handshake error: {e}"); return; }
            };

            // Clone ALPN before moving conn into the handler.
            let alpn = conn.alpn().to_vec();
            if alpn == transfer::XFER_ALPN {
                if let Err(e) = transfer::handle_xfer_connection(conn, store_clone, registry_clone).await {
                    tracing::warn!("xfer session error: {e}");
                }
            } else if alpn == node::NODE_ALPN {
                if let Err(e) =
                    node::handle_node_connection(conn, store_clone, &hb_id_clone, queue_clone, app_clone)
                        .await
                {
                    tracing::warn!("node session error: {e}");
                }
            } else {
                tracing::debug!("unknown ALPN on incoming connection, dropping");
            }
        });
    }
}

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

/// If a keypair is on disk, populate `identity` and spawn the iroh endpoint.
/// Non-fatal: a failed endpoint logs a warning and the user can retry by restarting.
fn restore_identity_and_start_endpoint(
    store: DataStore,
    identity: SharedIdentity,
    endpoint_state: SharedEndpoint,
    dm_queue: SharedDmQueue,
    download_registry: SharedDownloadRegistry,
    app: tauri::AppHandle,
) {
    let keypair_result = store.load_keypair();
    if let Err(ref e) = keypair_result {
        tracing::error!("Failed to load keypair on startup: {e:#}");
    }
    let Ok(Some(stored)) = keypair_result else { return };

    let private_arr: Option<[u8; 32]> = hex::decode(&stored.private_key_hex)
        .ok()
        .and_then(|b| b.try_into().ok());

    let Some(arr) = private_arr else { return };

    // Populate synchronously so the heartbeat task has a keypair at its first fire (15 s).
    *identity.blocking_write() = Some(HoardbookKeypair::from_bytes(&arr));

    let hb_id = stored.hb_id.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            start_iroh_endpoint(&arr, store, endpoint_state, dm_queue, hb_id, app, download_registry)
                .await
        {
            tracing::warn!("iroh endpoint startup failed: {e}");
        }
    });
}

/// Spawn all long-running background tasks (heartbeat, DHT, update check).
fn spawn_background_tasks(
    relay: SharedRelay,
    identity: SharedIdentity,
    endpoint_state: SharedEndpoint,
    hb_cancel_rx: tokio::sync::watch::Receiver<bool>,
    dht_identity_port: u16,
    dht_cancel: SharedDhtCancel,
    store: DataStore,
    app: tauri::AppHandle,
) {
    tauri::async_runtime::spawn(heartbeat::run_heartbeat_loop(
        Arc::clone(&relay),
        Arc::clone(&identity),
        Arc::clone(&endpoint_state),
        hb_cancel_rx,
    ));

    tauri::async_runtime::spawn(dht_service::run_identity_server(
        dht_identity_port,
        Arc::clone(&identity),
        Arc::clone(&relay),
        dht_cancel.subscribe(),
    ));

    tauri::async_runtime::spawn(dht_service::run_dht_announce_loop(
        Arc::clone(&identity),
        Arc::clone(&relay),
        store,
        dht_cancel.subscribe(),
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
    tauri::Builder::default()
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
            let settings = store.load_settings().ok().flatten().unwrap_or_default();

            let identity: SharedIdentity = Arc::new(RwLock::new(None));
            let endpoint_state: SharedEndpoint = Arc::new(RwLock::new(None));
            let dm_queue: SharedDmQueue = Arc::new(tokio::sync::Mutex::new(vec![]));
            let relay: SharedRelay = Arc::new(RelayClient::new(settings.relay_urls));
            let download_registry: SharedDownloadRegistry =
                Arc::new(transfer::DownloadRegistry::new());

            let (hb_cancel_tx, hb_cancel_rx) = tokio::sync::watch::channel(false);
            let hb_cancel: SharedCancelHeartbeat = Arc::new(hb_cancel_tx);
            let (dht_cancel_tx, _) = tokio::sync::watch::channel(false);
            let dht_cancel: SharedDhtCancel = Arc::new(dht_cancel_tx);

            app.manage(store.clone());
            app.manage(Arc::clone(&identity));
            app.manage(Arc::clone(&relay));
            app.manage(Arc::clone(&endpoint_state));
            app.manage(Arc::clone(&download_registry));
            app.manage(Arc::clone(&dm_queue));
            app.manage(Arc::clone(&hb_cancel));
            app.manage(Arc::clone(&dht_cancel));

            build_system_tray(app)?;

            let app_handle = app.handle().clone();

            restore_identity_and_start_endpoint(
                store.clone(),
                Arc::clone(&identity),
                Arc::clone(&endpoint_state),
                Arc::clone(&dm_queue),
                Arc::clone(&download_registry),
                app_handle.clone(),
            );

            spawn_background_tasks(
                Arc::clone(&relay),
                Arc::clone(&identity),
                Arc::clone(&endpoint_state),
                hb_cancel_rx,
                settings.dht_identity_port,
                Arc::clone(&dht_cancel),
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
            commands::identity::import_keypair,
            commands::identity::get_identity,
            commands::identity::get_hb_id,
            commands::identity::validate_hb_id,
            commands::identity::get_node_addr,
            node::fetch_direct_dm_inbox,
            commands::identity::export_keypair,
            commands::identity::save_keypair_file,
            commands::identity::wipe_data,
            commands::profile::save_profile,
            commands::profile::get_profile,
            commands::profile::publish_profile,
            commands::profile::unpublish_profile,
            commands::profile::has_published_profile,
            commands::collection::scan_directory,
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
            commands::chat::send_message,
            commands::chat::get_messages,
            commands::sharing::get_share_settings,
            commands::sharing::save_share_settings,
            commands::sharing::request_download,
            commands::sharing::cancel_download,
            commands::dht::dht_search,
            commands::dht::dht_start_announce,
            commands::dht::dht_stop_announce,
            commands::groups::groups_get,
            commands::groups::groups_create,
            commands::groups::groups_rename,
            commands::groups::groups_delete,
            commands::groups::groups_assign,
            commands::groups::groups_unassign,
            commands::watches::watches_get,
            commands::watches::watches_create,
            commands::watches::watches_delete,
            commands::watches::watches_evaluate,
            commands::update::check_update,
            commands::update::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hoardbook");
}
