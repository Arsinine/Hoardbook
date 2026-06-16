//! Shared iroh connection lifecycle helpers.

/// Hold a connection open (bounded) until the peer closes it, before the connection is
/// dropped. Dropping it immediately after writing the response can send a
/// CONNECTION_CLOSE ahead of the (small) response on fast links, which the peer sees as
/// a truncated read. Shared by the node + xfer connection handlers and the harness.
pub(crate) async fn drain_connection(conn: &iroh::endpoint::Connection) {
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), conn.closed()).await;
}

// ---------------------------------------------------------------------------
// Regression tests — connection-close truncation race (HANDOVER 2026-06-11 §2)
//
// Both handlers used to drop the iroh Connection immediately after writing a small
// response; on a fast link the CONNECTION_CLOSE frame can race ahead of the response
// and the peer sees a truncated read ("read resp len" / "read status"). The loopback
// repro was deterministic. These tests run the REAL handlers over real QUIC on
// loopback (relay + discovery disabled), so removing the drain turns them red.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::Arc;

    use hb_core::{DocType, HoardbookKeypair, SignedEnvelope};
    use tokio::sync::Mutex;

    use crate::node::{self, SharedDmQueue};
    use crate::store::{DataStore, ShareSettings};
    use crate::transfer::{self, DownloadRegistry};

    const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Bind a loopback-only endpoint: crypto provider set, relays and address lookup
    /// disabled, so the test never touches the network beyond 127.0.0.1/::1.
    async fn bind_local_endpoint(secret: &[u8; 32], alpns: Vec<Vec<u8>>) -> iroh::Endpoint {
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(secret))
            .alpns(alpns)
            .bind()
            .await
            .expect("bind loopback endpoint")
    }

    /// The server's EndpointAddr rewritten to loopback IPs (bound sockets are
    /// wildcard addresses), serialised the way the relay heartbeat would carry it.
    fn loopback_addr_json(server: &iroh::Endpoint) -> String {
        let mut addrs: BTreeSet<iroh::TransportAddr> = BTreeSet::new();
        for sock in server.bound_sockets() {
            let ip: IpAddr = if sock.is_ipv4() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            };
            addrs.insert(iroh::TransportAddr::Ip(SocketAddr::new(ip, sock.port())));
        }
        let addr = iroh::EndpointAddr { id: server.id(), addrs };
        serde_json::to_string(&addr).unwrap()
    }

    fn seeded_store(kp: &HoardbookKeypair, dir: &std::path::Path) -> DataStore {
        let store = DataStore::new(dir.to_path_buf());
        let prof = hb_core::Profile {
            display_name: "race-test".to_string(),
            bio: None, tags: vec![], since: None, est_size: None, languages: vec![],
            contact_hint: None, email: None, location: None, social_links: vec![],
            willing_to: vec![], content_types: vec![], updated: chrono::Utc::now(),
        };
        store.save_profile_draft(&prof).unwrap();
        let env = SignedEnvelope::create(kp, DocType::Profile, &prof).unwrap();
        store.save_profile_signed(&env).unwrap();
        store
    }

    /// Node leg: every get_profile response is small, so without the drain the
    /// server's close raced ahead of it and ALL node responses failed on loopback.
    #[tokio::test]
    async fn node_response_survives_connection_close() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let server_kp = HoardbookKeypair::generate();
            let dir = tempfile::tempdir().unwrap();
            let store = seeded_store(&server_kp, dir.path());
            let hb_id = server_kp.hb_id();

            let server =
                bind_local_endpoint(server_kp.private_key_bytes(), vec![node::NODE_ALPN.to_vec()])
                    .await;
            let addr_json = loopback_addr_json(&server);

            let store_srv = store.clone();
            let hb_id_srv = hb_id.clone();
            let server_ep = server.clone();
            tokio::spawn(async move {
                while let Some(incoming) = server_ep.accept().await {
                    let Ok(accepting) = incoming.accept() else { continue };
                    let Ok(conn) = accepting.await else { continue };
                    let dm_queue: SharedDmQueue = Arc::new(Mutex::new(vec![]));
                    // The production path (minus the Tauri notification closure),
                    // including the drain under test.
                    let _ = node::handle_node_connection_core(
                        &conn, &store_srv, &hb_id_srv, &dm_queue, None, |_| {},
                    )
                    .await;
                }
            });

            let client_kp = HoardbookKeypair::generate();
            let client =
                bind_local_endpoint(client_kp.private_key_bytes(), vec![]).await;

            // Loopback made the race deterministic, but give it a few rounds anyway.
            for round in 0..3 {
                let (profile, _collections) =
                    node::fetch_profile_via_iroh(&client, &addr_json, &hb_id, None)
                        .await
                        .unwrap_or_else(|e| panic!("round {round}: response truncated: {e:#}"));
                assert!(profile.is_some(), "round {round}: profile must arrive intact");
            }

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// Xfer leg: the tiny require_follow denial is the smallest response on the
    /// transfer path — the documented deterministic loss without the drain.
    #[tokio::test]
    async fn xfer_error_response_survives_connection_close() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let server_kp = HoardbookKeypair::generate();
            let dir = tempfile::tempdir().unwrap();
            let store = DataStore::new(dir.path().to_path_buf());
            store
                .save_share_settings("col", &ShareSettings {
                    enabled: true,
                    require_follow: true,
                    root_path: Some(dir.path().to_str().unwrap().to_string()),
                    ..Default::default()
                })
                .unwrap();

            let server = bind_local_endpoint(
                server_kp.private_key_bytes(),
                vec![transfer::XFER_ALPN.to_vec()],
            )
            .await;
            let addr_json = loopback_addr_json(&server);

            let store_srv = store.clone();
            let server_ep = server.clone();
            tokio::spawn(async move {
                while let Some(incoming) = server_ep.accept().await {
                    let Ok(accepting) = incoming.accept() else { continue };
                    let Ok(conn) = accepting.await else { continue };
                    let registry = Arc::new(DownloadRegistry::new());
                    // Full production handler — owns the bi-stream accept and the drain.
                    let _ = transfer::handle_xfer_connection(conn, store_srv.clone(), registry)
                        .await;
                }
            });

            let client_kp = HoardbookKeypair::generate();
            let client = bind_local_endpoint(client_kp.private_key_bytes(), vec![]).await;

            for round in 0..3 {
                let registry = Arc::new(DownloadRegistry::new());
                let save = dir.path().join(format!("out-{round}.bin"));
                let err = transfer::download_file_inner(
                    &client,
                    &addr_json,
                    &server_kp.hb_id(),
                    "col",
                    "f.txt",
                    save.to_str().unwrap(),
                    None,
                    registry.next_id(),
                    registry.clone(),
                    |_ev| {},
                )
                .await
                .expect_err("stranger must be denied");
                let msg = err.to_string();
                assert!(
                    msg.contains("restricted to followers"),
                    "round {round}: expected the follower-gate denial to arrive intact, got: {msg}"
                );
            }

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }
}
