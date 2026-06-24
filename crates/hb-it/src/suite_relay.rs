//! Suite RELAY — relay-set resilience & cross-client presence (TEST_PLAN §Suite RELAY; REGRESSION
//! #81–83; devtest 2026-06-23 → HANDOVER #8/#11). Written red-first; passes once the client tolerates
//! a dead relay in the set (M12 W1).
//!
//! - **RELAY1** a single **unreachable** relay in the set does not break (or stall) reads.
//! - **RELAY2** two clients on the **same** relay see each other online (the #11 headline).
//! - **RELAY3** a failed cycle then a success surfaces a number — the no-sticky-"–" recovery.

use std::time::{Duration, Instant};

use anyhow::{ensure, Result};
use hb_core::binding::KIND_PRESENCE;
use hb_core::{build_binding, verify_binding, Identity};
use hb_net::{count_online, select_newest_by_created_at, RelayClient};
use nostr::prelude::*;

use crate::harness::{is_online, now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

/// A bogus relay that refuses every connection (privileged unused port) — the "dead relay in the
/// set" the connect-per-command model dragged every read against.
const DEAD_RELAY: &str = "ws://127.0.0.1:1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A presence beacon TTL comfortably inside the count's freshness window.
const PRESENCE_TTL: u64 = 1800;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("RELAY1 degraded-relay tolerance", relay1(ctx).await),
        result("RELAY2 cross-client presence is mutually visible", relay2(ctx).await),
        result("RELAY3 count recovers — no sticky –", relay3(ctx).await),
    ]
}

async fn relay1(ctx: &Ctx) -> Result<()> {
    // A set of [reachable, dead]: connect succeeds (one relay came up), and a publish+fetch returns
    // the reachable relay's result **bounded** — it must not block on the dead relay for the full
    // timeout on every call (the rate-limit/half-open drag #11 described).
    let reachable = ctx.relays[0].clone();
    let set = vec![reachable, DEAD_RELAY.to_string()];
    let alice = Identity::generate();

    let started = Instant::now();
    let client = RelayClient::connect(&alice, &set, CONNECT_TIMEOUT).await?;
    let beacon = build_binding(&alice, now(), PRESENCE_TTL)?;
    client.publish(&beacon).await?; // accepted by the reachable relay despite the dead one
    settle().await;

    let got = client
        .fetch(Filter::new().author(alice.public_key()).kind(Kind::from_u16(KIND_PRESENCE)), FETCH_TIMEOUT)
        .await?;
    let elapsed = started.elapsed();
    client.disconnect().await;

    ensure!(!got.is_empty(), "the reachable relay's presence was not returned despite a dead relay in the set");
    // Bounded: the whole connect+publish+fetch must not have been dragged to ~3×RELAY_TIMEOUT by the
    // dead relay (it would be if every op waited the full handshake budget on the dead host).
    ensure!(
        elapsed < Duration::from_secs(25),
        "a dead relay dragged the read to {elapsed:?} (≥25s) — not bounded"
    );
    Ok(())
}

async fn relay2(ctx: &Ctx) -> Result<()> {
    // The #11 headline: two clients on the SAME relay must see each other online. Each publishes a
    // presence beacon; from each side the count tallies ≥2 distinct fresh npubs AND the other's
    // beacon verifies.
    let relay = vec![ctx.relays[0].clone()];
    let alice = Identity::generate();
    let bob = Identity::generate();

    let ac = RelayClient::connect(&alice, &relay, CONNECT_TIMEOUT).await?;
    let bc = RelayClient::connect(&bob, &relay, CONNECT_TIMEOUT).await?;
    ac.publish(&build_binding(&alice, now(), PRESENCE_TTL)?).await?;
    bc.publish(&build_binding(&bob, now(), PRESENCE_TTL)?).await?;
    settle().await;

    // From Alice: at least Alice + Bob online, and Bob's beacon verifies as online here.
    let from_a = count_online(&ac, crate::harness::ONLINE_WINDOW_SECS, FETCH_TIMEOUT).await?;
    ensure!(from_a >= 2, "from Alice the online count is {from_a} (<2) — the two clients don't see each other");
    verify_peer_online(&ac, &bob).await?;

    // Symmetric from Bob.
    let from_b = count_online(&bc, crate::harness::ONLINE_WINDOW_SECS, FETCH_TIMEOUT).await?;
    ensure!(from_b >= 2, "from Bob the online count is {from_b} (<2) — not mutually visible");
    verify_peer_online(&bc, &alice).await?;

    ac.disconnect().await;
    bc.disconnect().await;
    Ok(())
}

/// Fetch `peer`'s newest presence via `client` and assert it verifies as a fresh online beacon.
async fn verify_peer_online(client: &RelayClient, peer: &Identity) -> Result<()> {
    let events = client
        .fetch(Filter::new().author(peer.public_key()).kind(Kind::from_u16(KIND_PRESENCE)), FETCH_TIMEOUT)
        .await?;
    let newest = select_newest_by_created_at(events).ok_or_else(|| anyhow::anyhow!("no presence for peer"))?;
    ensure!(is_online(newest.created_at.as_u64(), now()), "peer's presence is not within the online window");
    verify_binding(&newest, &peer.public_key(), now()).map_err(|e| anyhow::anyhow!("verify_binding failed: {e}"))?;
    Ok(())
}

async fn relay3(ctx: &Ctx) -> Result<()> {
    // No sticky "–": an errored cycle (a fully-dead set won't connect — bounded, not a hang) followed
    // by a successful count surfaces a number. The cache *keeps* the number across a later failure;
    // that retention is the L1 differential (`online::apply_refresh`). Here we prove the network layer
    // both fails cleanly and then yields a real count.
    let started = Instant::now();
    let dead_only = RelayClient::connect(&Identity::generate(), &[DEAD_RELAY.to_string()], CONNECT_TIMEOUT).await;
    ensure!(dead_only.is_err(), "connecting to a fully-dead set must error (the errored cycle), not succeed");
    ensure!(
        started.elapsed() < Duration::from_secs(25),
        "the errored cycle hung instead of failing bounded"
    );

    // The recovery: a reachable relay with a fresh presence yields a number ≥ 1.
    let alice = Identity::generate();
    let client = RelayClient::connect(&alice, &[ctx.relays[0].clone()], CONNECT_TIMEOUT).await?;
    client.publish(&build_binding(&alice, now(), PRESENCE_TTL)?).await?;
    settle().await;
    let n = count_online(&client, crate::harness::ONLINE_WINDOW_SECS, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(n >= 1, "after a recovery the count must surface a number (got {n})");
    Ok(())
}
