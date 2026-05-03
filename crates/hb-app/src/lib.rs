#![forbid(unsafe_code)]

mod commands;
mod error;
mod relay;
mod store;
mod transfer;

use std::sync::Arc;
use hb_core::HoardbookKeypair;
use relay::RelayClient;
use store::DataStore;
use tauri::Manager;
use tokio::sync::RwLock;

/// Managed state types — Arc-wrapped so they can be cloned into background tasks.
pub type SharedIdentity = Arc<RwLock<Option<HoardbookKeypair>>>;
pub type SharedRelay    = Arc<RelayClient>;
pub type SharedEndpoint = Arc<RwLock<Option<iroh::Endpoint>>>;

// ---------------------------------------------------------------------------
// iroh endpoint lifecycle helper
// ---------------------------------------------------------------------------

/// Create (or replace) the iroh P2P endpoint from the given private key bytes,
/// persist it in `endpoint_state`, and spawn the transfer accept loop.
pub(crate) async fn start_iroh_endpoint(
    private_bytes: &[u8; 32],
    store: DataStore,
    endpoint_state: SharedEndpoint,
) -> anyhow::Result<()> {
    let secret_key = iroh::SecretKey::from_bytes(private_bytes);

    let new_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .alpns(vec![transfer::XFER_ALPN.to_vec()])
        .bind()
        .await?;

    let mut guard = endpoint_state.write().await;

    // Gracefully close any previous endpoint (its accept loop will exit naturally).
    if let Some(old) = guard.take() {
        old.close().await;
    }

    // Spawn the file-transfer accept loop.
    let server_ep = new_ep.clone();
    tauri::async_runtime::spawn(transfer::run_server(server_ep, store));

    *guard = Some(new_ep);
    Ok(())
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

            std::fs::create_dir_all(&data_dir).expect("could not create app data dir");

            let identity: SharedIdentity = Arc::new(RwLock::new(None));
            let endpoint_state: SharedEndpoint = Arc::new(RwLock::new(None));

            // Load saved relay URLs from settings, if any.
            let store_tmp = DataStore::new(data_dir.clone());
            let saved_relays = store_tmp
                .load_settings()
                .ok()
                .flatten()
                .map(|s| s.relay_urls)
                .unwrap_or_default();
            let relay: SharedRelay = Arc::new(RelayClient::new(saved_relays));

            app.manage(DataStore::new(data_dir.clone()));
            app.manage(Arc::clone(&identity));
            app.manage(Arc::clone(&relay));
            app.manage(Arc::clone(&endpoint_state));

            // If a keypair is already on disk, start the iroh endpoint immediately.
            if let Ok(Some(stored)) = store_tmp.load_keypair() {
                let ep_arc = Arc::clone(&endpoint_state);
                let store_for_ep = DataStore::new(data_dir.clone());
                tauri::async_runtime::spawn(async move {
                    if let Ok(bytes) = hex::decode(&stored.private_key_hex) {
                        if let Ok(arr) = bytes.try_into() {
                            if let Err(e) = start_iroh_endpoint(&arr, store_for_ep, ep_arc).await {
                                tracing::warn!("iroh endpoint startup failed: {e}");
                            }
                        }
                    }
                });
            }

            // Background heartbeat every 5 minutes — includes iroh node_addr when available.
            let identity2    = Arc::clone(&identity);
            let relay2       = Arc::clone(&relay);
            let endpoint2    = Arc::clone(&endpoint_state);
            let store2       = DataStore::new(data_dir.clone());
            tauri::async_runtime::spawn(async move {
                let mut interval =
                    tokio::time::interval(tokio::time::Duration::from_secs(300));
                loop {
                    interval.tick().await;
                    let guard = identity2.read().await;
                    if let Some(ref kp) = *guard {
                        let node_addr = {
                            let ep_guard = endpoint2.read().await;
                            ep_guard.as_ref().and_then(|ep| {
                                serde_json::to_string(&ep.addr()).ok()
                            })
                        };
                        let listed = store2
                            .load_settings()
                            .ok()
                            .flatten()
                            .map(|s| s.recommended)
                            .unwrap_or(false);
                        if let Err(e) = relay2.send_heartbeat(kp, node_addr, listed).await {
                            tracing::debug!("heartbeat failed: {e}");
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::identity::generate_keypair,
            commands::identity::import_keypair,
            commands::identity::get_identity,
            commands::identity::get_hb_id,
            commands::identity::validate_hb_id,
            commands::identity::get_node_addr,
            commands::identity::export_keypair,
            commands::identity::save_keypair_file,
            commands::identity::wipe_data,
            commands::profile::save_profile,
            commands::profile::get_profile,
            commands::profile::publish_profile,
            commands::profile::unpublish_profile,
            commands::profile::has_published_profile,
            commands::profile::check_name_available,
            commands::collection::scan_directory,
            commands::collection::get_collections,
            commands::collection::delete_collection,
            commands::collection::publish_collection,
            commands::collection::update_collection_meta,
            commands::browse::paste_key,
            commands::browse::follow,
            commands::browse::get_contacts,
            commands::browse::unfollow_contact,
            commands::browse::refresh_contact,
            commands::browse::set_contact_tags,
            commands::browse::get_directory,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::check_relay,
            commands::chat::send_message,
            commands::chat::get_messages,
            commands::chat::get_channel_messages,
            commands::chat::post_channel_message,
            commands::sharing::get_share_settings,
            commands::sharing::save_share_settings,
            commands::sharing::request_download,
            commands::update::check_update,
            commands::update::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hoardbook");
}
