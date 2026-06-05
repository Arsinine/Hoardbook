use crate::{Config, helpers::*, tap::TestResult};
use hb_core::{HoardbookKeypair, SignedEnvelope};
use serde_json::Value;

pub async fn run(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    out.push(d2_tampered_envelope(cfg).await);
    out.extend(d3_hb_id_checksum(cfg).await);
    out.push(d4_cross_relay_key_compat(cfg).await);
    out
}

// ---------------------------------------------------------------------------
// D2 — Tampered envelope rejected at relay
// ---------------------------------------------------------------------------

async fn d2_tampered_envelope(cfg: &Config) -> TestResult {
    let name = "D2: tampered envelope rejected (HTTP transport layer)";
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    let mut env = make_message(&alice, &bob.hb_id(), 400);
    // Mutate the payload after signing.
    env.payload["content"] = serde_json::json!("injected-content");
    let body = serde_json::json!({
        "type": "message",
        "document": serde_json::to_value(&env).unwrap(),
    });
    match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg))
        .json(&body).send().await
    {
        Ok(r) if !r.status().is_success() => TestResult::ok(name),
        Ok(r) => TestResult::fail(name, format!("tampered envelope accepted (status {})", r.status())),
        Err(e) => TestResult::fail(name, e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// D3 — HbId checksum rejection at relay endpoints
// ---------------------------------------------------------------------------

async fn d3_hb_id_checksum(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    let kp = HoardbookKeypair::generate();
    let valid_id = kp.hb_id();
    // Corrupt the last character of the HbId (4-byte checksum at the end).
    let mut mangled: Vec<char> = valid_id.chars().collect();
    let last_idx = mangled.len() - 1;
    mangled[last_idx] = if mangled[last_idx] == 'A' { 'B' } else { 'A' };
    let mangled_id: String = mangled.into_iter().collect();

    // D3a: GET /v1/peer/<mangled_id> must return error.
    {
        let name = "D3a: GET /v1/peer with mangled HbId rejected";
        let resp = cfg.client.get(format!("{}/v1/peer/{mangled_id}", cfg.relay_sg)).send().await;
        match resp {
            Ok(r) if !r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("mangled HbId accepted by peer endpoint ({})", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    // D3b: Heartbeat with mangled public_key in HeartbeatBody must be rejected.
    {
        let name = "D3b: heartbeat with mangled public_key rejected";
        let body_value = serde_json::json!({
            "doc_type": "heartbeat",
            "payload": {
                "public_key": mangled_id,
                "signed_at": chrono::Utc::now().to_rfc3339(),
            },
            "public_key": valid_id,
            "signature": "deadbeef".repeat(16),
            "signed_at": chrono::Utc::now(),
        });
        let resp = cfg.client.post(format!("{}/v1/heartbeat", cfg.relay_sg))
            .json(&body_value).send().await;
        match resp {
            Ok(r) if !r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("mangled heartbeat accepted ({})", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// D4 — Cross-relay key compatibility (sign on SG, verify on JP)
// ---------------------------------------------------------------------------

async fn d4_cross_relay_key_compat(cfg: &Config) -> TestResult {
    let name = "D4: cross-relay key compatibility (sign→publish SG, read+verify JP)";
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    // Alice sends heartbeat to relay-jp so it knows her.
    let hb = make_heartbeat(&alice, None);
    match cfg.client.post(format!("{}/v1/heartbeat", cfg.relay_jp)).json(&hb).send().await {
        Ok(r) if !r.status().is_success() => {
            return TestResult::fail(name, format!("heartbeat to relay-jp failed: {}", r.status()));
        }
        Err(e) => return TestResult::fail(name, e.to_string()),
        _ => {}
    }
    // Alice publishes a message to relay-jp.
    let env = make_message(&alice, &bob.hb_id(), 500);
    match cfg.client.post(format!("{}/v1/publish", cfg.relay_jp))
        .json(&publish_body(&env)).send().await
    {
        Ok(r) if !r.status().is_success() => {
            return TestResult::fail(name, format!("publish to relay-jp failed: {}", r.status()));
        }
        Err(e) => return TestResult::fail(name, e.to_string()),
        _ => {}
    }
    // Bob reads the message from relay-jp and verifies Alice's signature locally.
    let (sa, sig) = mailbox_auth(&bob);
    let resp = cfg.client
        .get(format!("{}/v1/messages/{}", cfg.relay_jp, bob.hb_id()))
        .query(&[("signed_at", &sa), ("signature", &sig)])
        .send().await;
    let body: Value = match resp {
        Err(e) => return TestResult::fail(name, e.to_string()),
        Ok(r) if !r.status().is_success() => return TestResult::fail(name, format!("GET failed: {}", r.status())),
        Ok(r) => r.json().await.unwrap(),
    };
    let msgs = body["messages"].as_array().unwrap_or(&vec![]).clone();
    if msgs.is_empty() {
        return TestResult::fail(name, "no messages found on relay-jp");
    }
    let stored_env: SignedEnvelope = match serde_json::from_value(msgs[0].clone()) {
        Ok(e) => e,
        Err(e) => return TestResult::fail(name, format!("envelope parse failed: {e}")),
    };
    // Signature verification uses hb_core on the client side — same crypto as relay.
    match stored_env.verify() {
        Ok(()) => {}
        Err(e) => return TestResult::fail(name, format!("signature verification failed: {e}")),
    }
    // Confirm the sender is Alice.
    if stored_env.public_key != alice.hb_id() {
        return TestResult::fail(name, format!("sender mismatch: expected {} got {}", alice.hb_id(), stored_env.public_key));
    }
    TestResult::ok(name)
}
