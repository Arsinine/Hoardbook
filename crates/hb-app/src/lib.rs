#![forbid(unsafe_code)]

mod commands;
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
pub type SharedCancelHeartbeat    = Arc<tokio::sync::watch::Sender<bool>>;

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
    tauri::async_runtime::spawn(run_accept_loop(server_ep, store, own_hb_id, dm_queue, app));

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
                if let Err(e) = transfer::handle_xfer_connection(conn, store_clone).await {
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

            // Mode 0700 on Linux — data dir accessible only to the owning user.
            #[cfg(not(target_os = "windows"))]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&data_dir)
                    .expect("could not create app data dir");
            }
            #[cfg(target_os = "windows")]
            std::fs::create_dir_all(&data_dir).expect("could not create app data dir");

            let identity: SharedIdentity = Arc::new(RwLock::new(None));
            let endpoint_state: SharedEndpoint = Arc::new(RwLock::new(None));
            let dm_queue: SharedDmQueue = Arc::new(tokio::sync::Mutex::new(vec![]));

            // Load saved relay URLs from settings, if any.
            let store_tmp = DataStore::new(data_dir.clone());
            let saved_relays = store_tmp
                .load_settings()
                .ok()
                .flatten()
                .map(|s| s.relay_urls)
                .unwrap_or_default();
            let relay: SharedRelay = Arc::new(RelayClient::new(saved_relays));

            let download_registry: SharedDownloadRegistry =
                Arc::new(transfer::DownloadRegistry::new());

            let (hb_cancel_tx, hb_cancel_rx) = tokio::sync::watch::channel(false);
            let hb_cancel: SharedCancelHeartbeat = Arc::new(hb_cancel_tx);

            app.manage(DataStore::new(data_dir.clone()));
            app.manage(Arc::clone(&identity));
            app.manage(Arc::clone(&relay));
            app.manage(Arc::clone(&endpoint_state));
            app.manage(Arc::clone(&download_registry));
            app.manage(Arc::clone(&dm_queue));
            app.manage(Arc::clone(&hb_cancel));

            // System tray — "Open Hoardbook" / "Quit".
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

            let app_handle = app.handle().clone();

            // If a keypair is already on disk, populate identity and start the iroh endpoint.
            let keypair_load_result = store_tmp.load_keypair();
            if let Err(ref e) = keypair_load_result {
                tracing::error!("Failed to load keypair on startup: {e:#}");
            }
            if let Ok(Some(stored)) = keypair_load_result {
                // Populate SharedIdentity synchronously so the heartbeat task always
                // has a keypair when it fires its first beat 15 s after launch.
                if let Ok(bytes) = hex::decode(&stored.private_key_hex) {
                    let arr: Result<[u8; 32], _> = bytes.try_into();
                    if let Ok(arr) = arr {
                        *identity.blocking_write() = Some(HoardbookKeypair::from_bytes(&arr));
                    }
                }

                let ep_arc = Arc::clone(&endpoint_state);
                let q_arc = Arc::clone(&dm_queue);
                let store_for_ep = DataStore::new(data_dir.clone());
                let hb_id = stored.hb_id.clone();
                let app_for_ep = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(bytes) = hex::decode(&stored.private_key_hex) {
                        if let Ok(arr) = bytes.try_into() {
                            if let Err(e) =
                                start_iroh_endpoint(&arr, store_for_ep, ep_arc, q_arc, hb_id, app_for_ep)
                                    .await
                            {
                                tracing::warn!("iroh endpoint startup failed: {e}");
                            }
                        }
                    }
                });
            }

            // Background heartbeat: first fires within 30 s, then every 5 minutes.
            tauri::async_runtime::spawn(heartbeat::run_heartbeat_loop(
                Arc::clone(&relay),
                Arc::clone(&identity),
                Arc::clone(&endpoint_state),
                hb_cancel_rx,
            ));

            // Background update check: silent unless a newer version is found.
            let app_for_update = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                match app_for_update.updater_builder().build() {
                    Ok(updater) => match updater.check().await {
                        Ok(Some(update)) => {
                            tracing::info!("Update available: v{}", update.version);
                            let _ = app_for_update.emit("update-available", &update.version);
                        }
                        Ok(None) => tracing::debug!("App is up to date"),
                        Err(e) => tracing::debug!("Background update check failed: {e}"),
                    },
                    Err(e) => tracing::debug!("Updater not configured: {e}"),
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it to the system tray rather than quitting.
            // "Quit" from the tray menu calls app.exit(0) which bypasses this.
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
