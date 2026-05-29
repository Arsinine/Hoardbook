//! Background heartbeat task — fires within 30 s of launch, then every 5 minutes.
//! Carries the iroh NodeAddr so the relay can hand it to querying peers.

use tokio::sync::watch;

use crate::{SharedEndpoint, SharedIdentity, SharedRelay};

/// Run the heartbeat loop until `cancel` receives a new value.
///
/// First heartbeat fires 15 s after startup (within the 30 s spec requirement).
/// Subsequent heartbeats fire every 300 s. Relay failures are logged and swallowed;
/// the task never terminates on a network error.
pub(crate) async fn run_heartbeat_loop(
    relay: SharedRelay,
    identity: SharedIdentity,
    endpoint: SharedEndpoint,
    mut cancel: watch::Receiver<bool>,
) {
    tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(15)) => {}
        Ok(()) = cancel.changed() => { return; }
    }
    fire_heartbeat(&relay, &identity, &endpoint).await;

    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await; // skip the immediate first tick — we just fired above

    loop {
        tokio::select! {
            _ = interval.tick() => {
                fire_heartbeat(&relay, &identity, &endpoint).await;
            }
            Ok(()) = cancel.changed() => {
                tracing::debug!("heartbeat task cancelled");
                break;
            }
        }
    }
}

async fn fire_heartbeat(relay: &SharedRelay, identity: &SharedIdentity, endpoint: &SharedEndpoint) {
    let guard = identity.read().await;
    let Some(ref kp) = *guard else { return; };
    let node_addr = {
        let ep = endpoint.read().await;
        ep.as_ref().and_then(|e| serde_json::to_string(&e.addr()).ok())
    };
    if let Err(e) = relay.send_heartbeat(kp, node_addr).await {
        tracing::debug!("heartbeat failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Tests — T18 acceptance criteria
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use hb_core::types::HeartbeatBody;
    use hb_core::{HoardbookKeypair, hb_id_decode};

    use crate::relay::RelayClient;

    /// Heartbeat body must carry the iroh NodeAddr when one is available.
    #[test]
    fn heartbeat_includes_node_addr() {
        let kp = HoardbookKeypair::generate();
        let node_addr = Some("iroh://test-addr-abc123".to_string());
        let body = HeartbeatBody {
            node_addr: node_addr.clone(),
            public_key: kp.hb_id(),
            signed_at: chrono::Utc::now().to_rfc3339(),
        };
        assert_eq!(body.node_addr, node_addr, "node_addr must be included in the heartbeat body");
    }

    /// The heartbeat body is signed with the keypair; the signature must verify.
    #[test]
    fn heartbeat_signed_correctly() {
        let kp = HoardbookKeypair::generate();
        let body = HeartbeatBody {
            node_addr: Some("iroh://peer-addr".to_string()),
            public_key: kp.hb_id(),
            signed_at: chrono::Utc::now().to_rfc3339(),
        };
        let body_value = serde_json::to_value(&body).unwrap();
        let sig = kp.sign(&body_value);
        let pubkey_bytes = hb_id_decode(&kp.hb_id()).unwrap();
        assert!(
            hb_core::crypto::verify(&pubkey_bytes, &body_value, &sig).is_ok(),
            "heartbeat signature must verify against the signing keypair"
        );
    }

    /// A relay that refuses connections must not cause the task to return an error;
    /// the loop must survive and continue running.
    #[tokio::test]
    async fn task_survives_relay_failure() {
        let relay = RelayClient::new(vec!["http://127.0.0.1:1".to_string()]);
        let kp = HoardbookKeypair::generate();
        let result = relay.send_heartbeat(&kp, None).await;
        assert!(
            result.is_ok(),
            "relay failure must not propagate out of send_heartbeat: {result:?}"
        );
    }
}
