//! Shared iroh connection lifecycle helpers.
//!
//! Recovered from the retired transfer plane (`d242e71^`) unchanged, because the bug it fixes is a
//! property of QUIC teardown and not of what was being carried — see the regression test below.

/// Hold a connection open (bounded) until the peer closes it, before the connection is
/// dropped. Dropping it immediately after writing the response can send a
/// CONNECTION_CLOSE ahead of the (small) response on fast links, which the peer sees as
/// a truncated read. Shared by the manifest connection handler and the harness.
pub(crate) async fn drain_connection(conn: &iroh::endpoint::Connection) {
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), conn.closed()).await;
}

// ---------------------------------------------------------------------------
// Regression test — connection-close truncation race (HANDOVER 2026-06-11 §2)
//
// The retired xfer handler dropped the iroh Connection immediately after writing a small response;
// on a fast link the CONNECTION_CLOSE frame can race ahead of the response and the peer sees a
// truncated read. The loopback repro was deterministic. M18 flagged this as a bug that WILL recur,
// so the test was recovered along with the drain — rewritten against the manifest plane's handler,
// since the old one no longer exists.
//
// It runs the REAL production handler over real QUIC on loopback (relays and address lookup
// disabled), so removing the drain from `serve_manifest_connection` turns it red. The refusal
// response is the smallest thing the manifest plane ever writes — a single status byte plus a short
// framed reason — which is exactly the shape that gets lost without the drain.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use hb_core::ticket::ContactStanding;

    use crate::transport::tests::{bind_local_endpoint, loopback_addr, real_payload_for, TestSource};
    use crate::transport::{fetch_manifest, ManifestPlane, MANIFEST_ALPN};
    use hb_core::ticket::TransportTicket;

    const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    #[tokio::test]
    async fn a_refusal_response_survives_connection_close() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let server =
                bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
            let addr = serde_json::to_string(&loopback_addr(&server)).unwrap();
            let ticket = TransportTicket::issue("req-1", "small", &addr, 1_700_000_000);
            let source = TestSource::new(ticket.clone(), real_payload_for("small", 10));
            // Blocked standing ⇒ every round takes the refusal path, the smallest response the
            // plane writes and the documented deterministic loss without the drain.
            *source.standing.lock().unwrap() = ContactStanding::Blocked;

            let accept_ep = server.clone();
            let plane = ManifestPlane::new(source.clone());
            tokio::spawn(async move {
                while let Some(incoming) = accept_ep.accept().await {
                    let Ok(accepting) = incoming.accept() else { continue };
                    let Ok(conn) = accepting.await else { continue };
                    let plane = plane.clone();
                    tokio::spawn(async move { let _ = plane.serve(conn).await; });
                }
            });

            let client = bind_local_endpoint(&rand::random(), vec![]).await;
            for round in 0..3 {
                let err = fetch_manifest(&client, &ticket)
                    .await
                    .expect_err("a blocked redeemer must be refused");
                let msg = err.to_string();
                assert!(
                    msg.contains("no longer an approved contact"),
                    "round {round}: the refusal must arrive intact, got: {msg}"
                );
            }

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }
}
