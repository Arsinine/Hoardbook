use crate::{Config, helpers::*, tap::TestResult};
use hb_core::HoardbookKeypair;
use serde_json::Value;

pub async fn run(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    out.push(a1_health(cfg).await);
    out.push(a2_cross_relay_isolation(cfg).await);
    out.extend(a3_timestamp_freshness(cfg).await);
    out.push(a4_concurrent_heartbeats(cfg).await);
    if cfg.slow {
        // A5 needs the relay's rate cap small enough to exceed in a short burst. The --slow
        // run raises RATE_LIMIT_MAX to 10000, so A5 would have to fire 10005 requests — skip
        // it here; the fast pass (cap 30) covers rate limiting.
        out.push(TestResult::skip("A5: rate limiting enforced", "fast-run only (relay cap raised for --slow)"));
        out.extend(a6_mailbox_race(cfg).await);
        out.push(a7_per_sender_cap(cfg).await);
    } else {
        out.push(a5_rate_limiting(cfg).await);
        out.push(TestResult::skip("A6: mailbox fill race", "requires --slow (RATE_LIMIT_MAX=10000 on relays)"));
        out.push(TestResult::skip("A7: per-sender cap cross-relay", "requires --slow (RATE_LIMIT_MAX=10000 on relays)"));
    }
    out
}

// ---------------------------------------------------------------------------
// A1 — Health endpoint reachability
// ---------------------------------------------------------------------------

async fn a1_health(cfg: &Config) -> TestResult {
    let name = "A1: health both relays";
    for relay in [&cfg.relay_sg, &cfg.relay_jp] {
        let url = format!("{}/v1/health", relay);
        match cfg.client.get(&url).send().await {
            Err(e) => return TestResult::fail(name, format!("{relay} unreachable: {e}")),
            Ok(resp) if !resp.status().is_success() => {
                return TestResult::fail(name, format!("{relay} returned {}", resp.status()));
            }
            Ok(resp) => {
                let body: Value = match resp.json().await {
                    Ok(v) => v,
                    Err(e) => return TestResult::fail(name, format!("{relay} non-JSON body: {e}")),
                };
                if body["ok"] != true {
                    return TestResult::fail(name, format!("{relay} ok=false: {body}"));
                }
            }
        }
    }
    // Verify cross-relay peer lists.
    for (relay, peer) in [(&cfg.relay_sg, &cfg.relay_jp), (&cfg.relay_jp, &cfg.relay_sg)] {
        let body: Value = cfg.client
            .get(format!("{relay}/v1/health"))
            .send().await.unwrap()
            .json().await.unwrap();
        let peers = body["peers"].as_array().unwrap_or(&vec![]).clone();
        let found = peers.iter().any(|p| p.as_str().map(|s| s.contains(peer.trim_start_matches("https://"))).unwrap_or(false));
        if !found {
            return TestResult::fail(name, format!("{relay} health.peers does not reference {peer}: {peers:?}"));
        }
    }
    TestResult::ok(name)
}

// ---------------------------------------------------------------------------
// A2 — Cross-relay isolation: relays don't proxy to each other
// ---------------------------------------------------------------------------

async fn a2_cross_relay_isolation(cfg: &Config) -> TestResult {
    let name = "A2: cross-relay isolation (dumb pipes)";
    let kp = HoardbookKeypair::generate();
    // Register Alice on relay-sg.
    let hb = make_heartbeat(&kp, None);
    if let Err(e) = cfg.client.post(format!("{}/v1/heartbeat", cfg.relay_sg))
        .json(&hb).send().await
    {
        return TestResult::fail(name, format!("heartbeat to relay-sg failed: {e}"));
    }
    // Relay-jp must NOT know Alice (it's a dumb pipe, not a mesh).
    let resp = match cfg.client
        .get(format!("{}/v1/peer/{}", cfg.relay_jp, kp.hb_id()))
        .send().await
    {
        Err(e) => return TestResult::fail(name, format!("peer GET on relay-jp failed: {e}")),
        Ok(r) => r,
    };
    if !resp.status().is_success() {
        return TestResult::fail(name, format!("relay-jp peer GET failed with {}", resp.status()));
    }
    let body: Value = resp.json().await.unwrap();
    if body["online"] == true {
        return TestResult::fail(
            name,
            "relay-jp incorrectly reports Alice as online — relays must not proxy to each other",
        );
    }
    TestResult::ok(name)
}

// ---------------------------------------------------------------------------
// A3 — Heartbeat timestamp freshness over real geo-distance
// ---------------------------------------------------------------------------

async fn a3_timestamp_freshness(cfg: &Config) -> Vec<TestResult> {
    let mut out = Vec::new();
    // A3a: fresh heartbeat accepted.
    {
        let name = "A3a: fresh heartbeat accepted (relay-sg)";
        let kp = HoardbookKeypair::generate();
        let hb = make_heartbeat(&kp, None);
        let r = cfg.client.post(format!("{}/v1/heartbeat", cfg.relay_sg)).json(&hb).send().await;
        match r {
            Ok(resp) if resp.status().is_success() => out.push(TestResult::ok(name)),
            Ok(resp) => out.push(TestResult::fail(name, format!("got {}", resp.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }
    // A3b: heartbeat signed 290s ago accepted (inside 300s window).
    {
        let name = "A3b: heartbeat signed 290s ago accepted";
        let kp = HoardbookKeypair::generate();
        let hb = make_stale_heartbeat(&kp, 290);
        let r = cfg.client.post(format!("{}/v1/heartbeat", cfg.relay_sg)).json(&hb).send().await;
        match r {
            Ok(resp) if resp.status().is_success() => out.push(TestResult::ok(name)),
            Ok(resp) => out.push(TestResult::fail(name, format!("290s old heartbeat rejected: {}", resp.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }
    // A3c: heartbeat signed 310s ago rejected (outside 300s window).
    {
        let name = "A3c: heartbeat signed 310s ago rejected";
        let kp = HoardbookKeypair::generate();
        let hb = make_stale_heartbeat(&kp, 310);
        let r = cfg.client.post(format!("{}/v1/heartbeat", cfg.relay_sg)).json(&hb).send().await;
        match r {
            Ok(resp) if !resp.status().is_success() => out.push(TestResult::ok(name)),
            Ok(resp) => out.push(TestResult::fail(name, format!("310s old heartbeat was accepted (expected rejection): {}", resp.status()))),
            Err(e) => out.push(TestResult::fail(name, e.to_string())),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// A4 — Concurrent heartbeat upserts (SQLite ON CONFLICT)
// ---------------------------------------------------------------------------

async fn a4_concurrent_heartbeats(cfg: &Config) -> TestResult {
    let name = "A4: concurrent heartbeat upserts";
    let kp = HoardbookKeypair::generate();
    let n = 10usize;
    let mut handles = tokio::task::JoinSet::new();
    for _ in 0..n {
        let client = cfg.client.clone();
        let url = format!("{}/v1/heartbeat", cfg.relay_sg);
        let hb = make_heartbeat(&kp, None);
        handles.spawn(async move {
            client.post(&url).json(&hb).send().await
        });
    }
    let mut errors = Vec::new();
    while let Some(res) = handles.join_next().await {
        match res {
            Ok(Ok(resp)) if !resp.status().is_success() => {
                errors.push(resp.status().to_string());
            }
            Ok(Err(e)) => errors.push(e.to_string()),
            Err(e) => errors.push(e.to_string()),
            _ => {}
        }
    }
    if !errors.is_empty() {
        return TestResult::fail(name, format!("{}/{n} requests failed: {:?}", errors.len(), errors));
    }
    // Peer must be online after all upserts.
    let resp = cfg.client
        .get(format!("{}/v1/peer/{}", cfg.relay_sg, kp.hb_id()))
        .send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let body: Value = r.json().await.unwrap();
            if body["online"] != true {
                return TestResult::fail(name, format!("peer offline after {n} heartbeats: {body}"));
            }
        }
        Ok(r) => return TestResult::fail(name, format!("peer GET returned {}", r.status())),
        Err(e) => return TestResult::fail(name, e.to_string()),
    }
    TestResult::ok(name)
}

// ---------------------------------------------------------------------------
// A5 — Rate limiting from two distinct IPs
// ---------------------------------------------------------------------------

async fn a5_rate_limiting(cfg: &Config) -> TestResult {
    let name = "A5: rate limiting enforced";
    let kp = HoardbookKeypair::generate();
    let url = format!("{}/v1/heartbeat", cfg.relay_jp);
    // Send rate_limit_max+5 requests; expect to hit the limit.
    let burst = cfg.rate_limit_max as usize + 5;
    let mut successes = 0usize;
    let mut rate_limited = false;
    for _ in 0..burst {
        let hb = make_heartbeat(&kp, None);
        match cfg.client.post(&url).json(&hb).send().await {
            Err(e) => return TestResult::fail(name, format!("request failed: {e}")),
            Ok(resp) if resp.status().is_success() => successes += 1,
            Ok(resp) => {
                let text = resp.text().await.unwrap_or_default();
                if text.contains("rate limit") {
                    rate_limited = true;
                    break;
                }
                // Other non-success (e.g. duplicate timestamp) - skip
            }
        }
    }
    if !rate_limited {
        return TestResult::fail(
            name,
            format!("sent {burst} requests, none were rate-limited (rate_limit_max={})", cfg.rate_limit_max),
        );
    }
    if successes == 0 {
        return TestResult::fail(name, "rate limited on first request — no successful requests at all");
    }
    if successes > cfg.rate_limit_max as usize {
        return TestResult::fail(
            name,
            format!("{successes} requests succeeded, but cap is {} — rate limiter not enforcing", cfg.rate_limit_max),
        );
    }
    // This test intentionally saturated relay-jp's per-IP rate window. The relay rate-limits
    // ALL endpoints per IP (publish/heartbeat/messages/peer) and returns 400 on exceed, so
    // later relay-jp tests from this same IP (B2, D4) would otherwise be collaterally
    // throttled. Wait for the 60s window to drain so the suite stays isolated.
    tokio::time::sleep(std::time::Duration::from_secs(61)).await;
    TestResult::ok(name)
}

// ---------------------------------------------------------------------------
// A6 — Mailbox fill race (concurrent publish near per-pair cap)
// ---------------------------------------------------------------------------

async fn a6_mailbox_race(cfg: &Config) -> Vec<TestResult> {
    let name_seq = "A6a: mailbox sequential fill to near per-pair cap";
    let name_race = "A6b: mailbox concurrent race at cap boundary";
    let mut out = Vec::new();
    let alice = HoardbookKeypair::generate();
    let bob = HoardbookKeypair::generate();
    // Insert 45 messages sequentially (per-pair cap is 50).
    for i in 0..45i64 {
        let env = make_message(&alice, &bob.hb_id(), i * 2); // +2ms offset each to ensure unique sent_at
        let body = publish_body(&env);
        match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg)).json(&body).send().await {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => {
                out.push(TestResult::fail(name_seq, format!("msg {i} rejected: {}", r.status())));
                out.push(TestResult::skip(name_race, "A6a setup failed"));
                return out;
            }
            Err(e) => {
                out.push(TestResult::fail(name_seq, e.to_string()));
                out.push(TestResult::skip(name_race, "A6a setup failed"));
                return out;
            }
        }
    }
    out.push(TestResult::ok(name_seq));
    // Fire 10 concurrent messages to race at the boundary.
    let mut handles = tokio::task::JoinSet::new();
    for i in 0..10i64 {
        let client = cfg.client.clone();
        let url = format!("{}/v1/publish", cfg.relay_sg);
        let env = make_message(&alice, &bob.hb_id(), 10000 + i * 2);
        let body = publish_body(&env);
        handles.spawn(async move {
            client.post(&url).json(&body).send().await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        });
    }
    let mut accepted = 0usize;
    while let Some(res) = handles.join_next().await {
        if res.unwrap_or(false) { accepted += 1; }
    }
    // Expect exactly 5 to succeed (to reach the cap of 50), but due to TOCTOU race
    // up to all 10 might succeed. Either way, we document the behavior.
    if accepted > 5 {
        out.push(TestResult::fail(
            name_race,
            format!(
                "TOCTOU race: {accepted}/10 concurrent messages accepted past per-pair cap of 50 \
                 (45 sequential + {accepted} concurrent = {} total, exceeds cap). \
                 The count+insert is not atomic.",
                45 + accepted,
            ),
        ));
    } else {
        out.push(TestResult::ok(name_race));
    }
    out
}

// ---------------------------------------------------------------------------
// A7 — Per-sender cap (M6) is enforced independently per relay
// ---------------------------------------------------------------------------

async fn a7_per_sender_cap(cfg: &Config) -> TestResult {
    let name = "A7: per-sender cap enforced independently per relay";
    let alice = HoardbookKeypair::generate();
    // Send 200 messages to 10 different recipients (20 each) on relay-sg.
    // Per-pair cap=50 is not hit (20 < 50). Sender cap=200 is reached after 200.
    let recipients: Vec<HoardbookKeypair> = (0..10).map(|_| HoardbookKeypair::generate()).collect();
    for (r_idx, recipient) in recipients.iter().enumerate() {
        for i in 0..20i64 {
            let offset = (r_idx as i64) * 100 + i * 2;
            let env = make_message(&alice, &recipient.hb_id(), offset);
            let body = publish_body(&env);
            match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg)).json(&body).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => {
                    let text = r.text().await.unwrap_or_default();
                    return TestResult::fail(name, format!("msg {}/{} rejected early: {text}", r_idx * 20 + i as usize, 200));
                }
                Err(e) => return TestResult::fail(name, e.to_string()),
            }
        }
    }
    // 201st message must be rejected (sender cap=200 exhausted).
    let extra_recipient = HoardbookKeypair::generate();
    let env = make_message(&alice, &extra_recipient.hb_id(), 50000);
    let body = publish_body(&env);
    match cfg.client.post(format!("{}/v1/publish", cfg.relay_sg)).json(&body).send().await {
        Ok(r) if !r.status().is_success() => {
            let text = r.text().await.unwrap_or_default();
            if text.contains("sender") || text.contains("quota") {
                // Expected
            }
        }
        Ok(r) => {
            return TestResult::fail(name, format!("201st message accepted (status {}), sender cap not enforced", r.status()));
        }
        Err(e) => return TestResult::fail(name, e.to_string()),
    }
    // Relay-jp must enforce its OWN independent cap (Alice hasn't sent to it yet).
    let another = HoardbookKeypair::generate();
    let env2 = make_message(&alice, &another.hb_id(), 51000);
    let body2 = publish_body(&env2);
    match cfg.client.post(format!("{}/v1/publish", cfg.relay_jp)).json(&body2).send().await {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => return TestResult::fail(name, format!("relay-jp rejected first message from Alice (cap should be independent): {}", r.status())),
        Err(e) => return TestResult::fail(name, e.to_string()),
    }
    TestResult::ok(name)
}
