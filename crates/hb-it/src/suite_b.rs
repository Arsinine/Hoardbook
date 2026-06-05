use crate::{Config, helpers::*, tap::TestResult};
use hb_core::{HoardbookKeypair, hb_id_decode};
use serde_json::Value;

pub async fn run(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    out.push(b1_happy_path(cfg).await);
    out.push(b2_cross_relay_isolation(cfg).await);
    out.extend(b3_mailbox_auth(cfg).await);
    out.push(b4_deduplication(cfg).await);
    out.extend(b5_stale_timestamp(cfg).await);
    out.push(b6_encrypted_message(cfg).await);
    out
}

// ---------------------------------------------------------------------------
// B1 — Happy path: sign → publish → auth-fetch → verify
// ---------------------------------------------------------------------------

async fn b1_happy_path(cfg: &Config) -> TestResult {
    let name = "B1: full DM happy path (sign→publish→auth-fetch→verify)";
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    // Alice sends a message to Bob.
    let env = make_message(&alice, &bob.hb_id(), 0);
    let body = publish_body(&env);
    match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg)).json(&body).send().await {
        Ok(r) if !r.status().is_success() => {
            return TestResult::fail(name, format!("publish failed: {}", r.status()));
        }
        Err(e) => return TestResult::fail(name, format!("publish error: {e}")),
        _ => {}
    }
    // Bob reads his mailbox with signed auth.
    let (signed_at, signature) = mailbox_auth(&bob);
    let resp = cfg.client
        .get(format!("{}/v1/messages/{}", cfg.relay_sg, bob.hb_id()))
        .query(&[("signed_at", &signed_at), ("signature", &signature)])
        .send().await;
    let body: Value = match resp {
        Err(e) => return TestResult::fail(name, format!("mailbox GET error: {e}")),
        Ok(r) if !r.status().is_success() => {
            return TestResult::fail(name, format!("mailbox GET failed: {}", r.status()));
        }
        Ok(r) => r.json().await.unwrap(),
    };
    let msgs = body["messages"].as_array().unwrap_or(&vec![]).clone();
    if msgs.is_empty() {
        return TestResult::fail(name, "mailbox empty — message was not delivered");
    }
    // Verify the envelope signature locally.
    let stored_env: hb_core::SignedEnvelope = match serde_json::from_value(msgs[0].clone()) {
        Ok(e) => e,
        Err(e) => return TestResult::fail(name, format!("envelope deserialization failed: {e}")),
    };
    if let Err(e) = stored_env.verify() {
        return TestResult::fail(name, format!("envelope signature invalid after roundtrip: {e}"));
    }
    if stored_env.public_key != alice.hb_id() {
        return TestResult::fail(name, format!("sender mismatch: expected {} got {}", alice.hb_id(), stored_env.public_key));
    }
    TestResult::ok(name)
}

// ---------------------------------------------------------------------------
// B2 — Cross-relay mailbox isolation
// ---------------------------------------------------------------------------

async fn b2_cross_relay_isolation(cfg: &Config) -> TestResult {
    let name = "B2: cross-relay mailbox isolation (relay-jp returns empty for relay-sg message)";
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    // Publish to relay-sg only.
    let env = make_message(&alice, &bob.hb_id(), 100);
    cfg.client.post(format!("{}/v1/publish", cfg.relay_sg))
        .json(&publish_body(&env)).send().await.ok();
    // Bob queries relay-jp — must get nothing.
    let (signed_at, signature) = mailbox_auth(&bob);
    let resp = cfg.client
        .get(format!("{}/v1/messages/{}", cfg.relay_jp, bob.hb_id()))
        .query(&[("signed_at", &signed_at), ("signature", &signature)])
        .send().await;
    match resp {
        Err(e) => return TestResult::fail(name, format!("relay-jp GET error: {e}")),
        Ok(r) if !r.status().is_success() => {
            return TestResult::fail(name, format!("relay-jp GET failed: {}", r.status()));
        }
        Ok(r) => {
            let body: Value = r.json().await.unwrap();
            let msgs = body["messages"].as_array().unwrap_or(&vec![]).clone();
            if !msgs.is_empty() {
                return TestResult::fail(name, format!("relay-jp has {len} messages — relays must not sync", len = msgs.len()));
            }
        }
    }
    TestResult::ok(name)
}

// ---------------------------------------------------------------------------
// B3 — Mailbox authentication rejections
// ---------------------------------------------------------------------------

async fn b3_mailbox_auth(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    let bob = HoardbookKeypair::generate();
    let eve = HoardbookKeypair::generate();

    // B3a: Eve's key cannot read Bob's mailbox (signed_key != path_key).
    {
        let name = "B3a: wrong-key mailbox read rejected";
        // Eve signs over BOB's mailbox using her own key.
        let signed_at = chrono::Utc::now().to_rfc3339();
        let signed = serde_json::json!({
            "purpose": MAILBOX_READ_PURPOSE,
            "public_key": bob.hb_id(),
            "signed_at": signed_at,
        });
        let sig = eve.sign(&signed);
        let resp = cfg.client
            .get(format!("{}/v1/messages/{}", cfg.relay_sg, bob.hb_id()))
            .query(&[("signed_at", &signed_at), ("signature", &sig)])
            .send().await;
        match resp {
            Ok(r) if !r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("accepted cross-key read (status {})", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    // B3b: Forged (garbage) signature rejected.
    {
        let name = "B3b: forged signature rejected";
        let signed_at = chrono::Utc::now().to_rfc3339();
        let garbage_sig = "deadbeef".repeat(16);
        let resp = cfg.client
            .get(format!("{}/v1/messages/{}", cfg.relay_sg, bob.hb_id()))
            .query(&[("signed_at", &signed_at), ("signature", &garbage_sig)])
            .send().await;
        match resp {
            Ok(r) if !r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("accepted forged signature (status {})", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    // B3c: Stale auth timestamp rejected.
    {
        let name = "B3c: stale mailbox auth timestamp rejected";
        let stale_at = (chrono::Utc::now() - chrono::Duration::seconds(310)).to_rfc3339();
        let signed = serde_json::json!({
            "purpose": MAILBOX_READ_PURPOSE,
            "public_key": bob.hb_id(),
            "signed_at": stale_at,
        });
        let sig = bob.sign(&signed);
        let resp = cfg.client
            .get(format!("{}/v1/messages/{}", cfg.relay_sg, bob.hb_id()))
            .query(&[("signed_at", &stale_at), ("signature", &sig)])
            .send().await;
        match resp {
            Ok(r) if !r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("accepted stale auth (status {})", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// B4 — Message deduplication (INSERT OR IGNORE on same from+sent_at)
// ---------------------------------------------------------------------------

async fn b4_deduplication(cfg: &Config) -> TestResult {
    let name = "B4: message deduplication across retries";
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    let env = make_message(&alice, &bob.hb_id(), 200);
    let body = publish_body(&env);
    // Post twice — should silently deduplicate.
    for _ in 0..2 {
        cfg.client.post(format!("{}/v1/publish", cfg.relay_sg))
            .json(&body).send().await.ok();
    }
    let (sa, sig) = mailbox_auth(&bob);
    let resp = cfg.client
        .get(format!("{}/v1/messages/{}", cfg.relay_sg, bob.hb_id()))
        .query(&[("signed_at", &sa), ("signature", &sig)])
        .send().await;
    match resp {
        Err(e) => TestResult::fail(name, e.to_string()),
        Ok(r) if !r.status().is_success() => TestResult::fail(name, format!("GET failed: {}", r.status())),
        Ok(r) => {
            let body: Value = r.json().await.unwrap();
            let count = body["messages"].as_array().map(|a| a.len()).unwrap_or(0);
            if count != 1 {
                TestResult::fail(name, format!("expected 1 message, got {count} (deduplication failed)"))
            } else {
                TestResult::ok(name)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// B5 — Stale message timestamp boundary
// ---------------------------------------------------------------------------

async fn b5_stale_timestamp(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();

    // B5a: 290s old message accepted.
    {
        let name = "B5a: message 290s old accepted";
        let env = make_stale_message(&alice, &bob.hb_id(), 290);
        match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg))
            .json(&publish_body(&env)).send().await
        {
            Ok(r) if r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("290s message rejected: {}", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    // B5b: 310s old message rejected.
    {
        let name = "B5b: message 310s old rejected";
        let env = make_stale_message(&alice, &bob.hb_id(), 310);
        match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg))
            .json(&publish_body(&env)).send().await
        {
            Ok(r) if !r.status().is_success() => out.push(TestResult::ok(name)),
            Ok(r) => out.push(TestResult::fail(name, format!("310s message accepted (expected rejection): {}", r.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// B6 — E2E encrypted message round-trip
// ---------------------------------------------------------------------------

async fn b6_encrypted_message(cfg: &Config) -> TestResult {
    let name = "B6: E2E encrypted message (X25519+ChaCha20, relay stores opaque bytes)";
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    let plaintext = "secret payload 🔒";
    let env = make_encrypted_message(&alice, &bob.hb_id(), plaintext, 300);
    // Publish
    match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg))
        .json(&publish_body(&env)).send().await
    {
        Ok(r) if !r.status().is_success() => {
            return TestResult::fail(name, format!("publish failed: {}", r.status()));
        }
        Err(e) => return TestResult::fail(name, e.to_string()),
        _ => {}
    }
    // Fetch
    let (sa, sig) = mailbox_auth(&bob);
    let resp = cfg.client
        .get(format!("{}/v1/messages/{}", cfg.relay_sg, bob.hb_id()))
        .query(&[("signed_at", &sa), ("signature", &sig)])
        .send().await;
    let msgs_value: Value = match resp {
        Err(e) => return TestResult::fail(name, e.to_string()),
        Ok(r) if !r.status().is_success() => return TestResult::fail(name, format!("GET failed: {}", r.status())),
        Ok(r) => r.json().await.unwrap(),
    };
    let msgs = msgs_value["messages"].as_array().unwrap_or(&vec![]).clone();
    if msgs.is_empty() {
        return TestResult::fail(name, "mailbox empty");
    }
    // Decode the stored envelope and verify relay stored it opaquely.
    let stored_env: hb_core::SignedEnvelope = match serde_json::from_value(msgs[0].clone()) {
        Ok(e) => e,
        Err(e) => return TestResult::fail(name, format!("envelope parse failed: {e}")),
    };
    let msg: hb_core::types::ChatMessage = match stored_env.parse_payload() {
        Ok(m) => m,
        Err(e) => return TestResult::fail(name, format!("payload parse failed: {e}")),
    };
    if !msg.encrypted {
        return TestResult::fail(name, "message stored as unencrypted — relay should not modify encrypted flag");
    }
    // Relay must have stored ciphertext, not plaintext.
    if msg.content.contains(plaintext) {
        return TestResult::fail(name, "relay stored plaintext — encryption not applied");
    }
    // Bob decrypts.
    let sent_at = msg.sent_at.to_rfc3339();
    let aad = hb_core::jcs::canonicalize(&serde_json::json!({
        "from": alice.hb_id(),
        "to": bob.hb_id(),
        "sent_at": sent_at,
    }));
    let alice_pubkey = hb_id_decode(&alice.hb_id()).unwrap();
    match bob.decrypt_from(&alice_pubkey, &msg.content, &aad) {
        Ok(recovered) if recovered == plaintext => TestResult::ok(name),
        Ok(recovered) => TestResult::fail(name, format!("decrypted to wrong content: {recovered:?}")),
        Err(e) => TestResult::fail(name, format!("decryption failed: {e}")),
    }
}
