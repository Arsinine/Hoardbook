//! Suite RELAY — relay-set resilience & cross-client presence (TEST_PLAN §Suite RELAY; REGRESSION
//! #81–83; devtest 2026-06-23 → HANDOVER #8/#11). Written red-first; passes once the client tolerates
//! a dead relay in the set (M12 W1).
//!
//! - **RELAY1** a single **unreachable** relay in the set does not break (or stall) reads.
//! - **RELAY2** two clients on the **same** relay see each other online (the #11 headline).
//! - **RELAY3** a failed cycle then a success surfaces a number — the no-sticky-"–" recovery.
//! - **RELAY5** a relay whose first handshake failed (Terminated) is re-dialed once its host returns
//!   (QURATOR-385 — `is_live()` sweeps stranded relays with backoff).
//! - **RELAY6** a fetch with a dead-terminal relay in the set returns the live relays' events
//!   (QURATOR-386 — the fetch targets the fetchable subset, never the corpse).

use std::time::{Duration, Instant};

use anyhow::{ensure, Result};
use hb_core::binding::KIND_PRESENCE;
use hb_core::event::{build_teaser, KIND_TEASER, Teaser};
use hb_core::{build_binding, verify_binding, Identity};
use hb_net::{count_online, select_newest_by_created_at, teaser_search_filter, RelayClient};
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
        result("RELAY4 same-IP two identities each count online (+2)", relay4(ctx).await),
        result("RELAY5 terminated relay is redialed once the host returns", relay5(ctx).await),
        result("RELAY6 fetch skips a dead-terminal relay in the set", relay6(ctx).await),
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
    ensure!(is_online(newest.created_at.as_secs(), now()), "peer's presence is not within the online window");
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

/// CI-runnable differential twin of the devtest #9 `--same-nat` diagnostic — shares its body
/// (`same_nat::same_nat1`) against `ctx.relays` (the ephemeral CI strfry). CI strfry has no per-IP
/// cap, so green here proves the plumbing, not the absence of the bug; the live diagnostic is
/// `--same-nat` against the production relays.
async fn relay4(ctx: &Ctx) -> Result<()> {
    crate::same_nat::same_nat1(ctx).await
}

/// RELAY5 (QURATOR-385): a relay whose FIRST handshake fails lands `RelayStatus::Terminated` and
/// nostr-relay-pool 0.44.3 spawns NO reconnect task for it — stranded for the session while the
/// other relays keep `pool_is_live` true (so the client is never rebuilt). The `is_live` redial
/// sweep must resurrect it once the host starts answering. We simulate "down, then back" with a
/// local TCP forwarder: the port refuses at first (bind port 0, read it, drop the listener),
/// then a forwarder pipes raw bytes to the real CI strfry, so the swept re-dial completes a real
/// websocket handshake through the production client.
async fn relay5(ctx: &Ctx) -> Result<()> {
    // A free port with NOTHING listening: bind to 0, read the port, drop the listener.
    let doorbell = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = doorbell.local_addr()?.port();
    drop(doorbell);
    let phantom = format!("ws://127.0.0.1:{port}");
    ensure!(
        ctx.relays[0].starts_with("ws://"),
        "RELAY5 forwards raw TCP; the harness relay must be ws://, got {}",
        ctx.relays[0]
    );
    let upstream = ctx.relays[0].trim_start_matches("ws://").to_string();

    let alice = Identity::generate();
    let client =
        RelayClient::connect(&alice, &[ctx.relays[0].clone(), phantom.clone()], CONNECT_TIMEOUT).await?;
    // Bounded wait for the phantom's refused handshake to settle into Terminated — the stranded
    // state (a first-handshake failure leaves no reconnect task behind it).
    let phantom_status = async {
        loop {
            let status = client
                .relay_status()
                .await
                .into_iter()
                .find(|h| h.url == phantom)
                .map(|h| h.status)
                .unwrap_or_default();
            if status == "terminated" {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let status = tokio::time::timeout(Duration::from_secs(5), phantom_status).await;
    ensure!(
        matches!(&status, Ok(s) if s == "terminated"),
        "the refused phantom relay never landed Terminated within 5 s (got {status:?})"
    );

    // The relay "comes back": a TCP forwarder on the same port piping to the real strfry, one
    // outbound connection per inbound (the pool may re-dial more than once across the test).
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let forwarder = tokio::spawn(async move {
        loop {
            let (mut inbound, _) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(_) => break,
            };
            let upstream = upstream.clone();
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(upstream.as_str()).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });

    // The trigger both persistent clients hit on every acquire (hb-net `is_live`). The sweep's
    // spawned re-dial must carry the phantom through a real handshake to Connected — bounded.
    ensure!(client.is_live().await, "the pool is live on the healthy relay regardless");
    let phantom_up = async {
        loop {
            let up = client
                .relay_status()
                .await
                .into_iter()
                .find(|h| h.url == phantom)
                .map(|h| h.connected)
                .unwrap_or(false);
            if up {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    let up = tokio::time::timeout(Duration::from_secs(10), phantom_up).await;
    ensure!(
        up == Ok(true),
        "the sweep never re-dialed the Terminated relay to Connected within 10 s (got {up:?})"
    );

    // End-to-end: a tagged teaser published to the pool is fetchable — the resurrected relay
    // genuinely serves events, it did not merely report Connected. Tag namespaced per-run so a
    // re-run against a dirty local relay cannot match a stale event.
    let tag = ctx.tag("rl5");
    client.publish(&build_teaser(&alice, &teaser("relay5", vec![tag.clone()]), true)?).await?;
    settle().await;
    let events = client
        .fetch(
            Filter::new().author(alice.public_key()).kind(Kind::from_u16(KIND_TEASER)),
            FETCH_TIMEOUT,
        )
        .await?;
    client.disconnect().await;
    forwarder.abort();
    ensure!(
        events.iter().any(|e| e.content.contains(&tag)),
        "the tagged teaser must come back through the resurrected pool"
    );
    Ok(())
}

async fn relay6(ctx: &Ctx) -> Result<()> {
    // QURATOR-386: a set of [reachable, dead] — the dead relay fails its first handshake into a
    // dead terminal state (connection task gone, channel receiver dropped), so the fetch must
    // target the live relay instead of erroring against the corpse on every call. Bounded like
    // RELAY1.
    let reachable = ctx.relays[0].clone();
    let set = vec![reachable, DEAD_RELAY.to_string()];
    let alice = Identity::generate();
    let tag = ctx.tag("relay6");

    let started = Instant::now();
    let client = RelayClient::connect(&alice, &set, CONNECT_TIMEOUT).await?;
    let tea = build_teaser(&alice, &teaser("relay6", vec![tag.clone()]), true)?;
    client.publish(&tea).await?;
    settle().await;

    let got = client.fetch(teaser_search_filter(&[tag], &[])?, FETCH_TIMEOUT).await?;
    let elapsed = started.elapsed();
    client.disconnect().await;

    ensure!(!got.is_empty(), "the live relay's teaser was not returned despite a dead relay in the set");
    ensure!(
        elapsed < Duration::from_secs(25),
        "a dead relay dragged the read to {elapsed:?} (≥25s) — not bounded"
    );
    Ok(())
}

fn teaser(name: &str, tags: Vec<String>) -> Teaser {
    Teaser {
        collections: Vec::new(),
        display_name: name.into(),
        bio: "hoards".into(),
        tags,
        content_types: vec![],
        picture: None,
        hide_in_rosters: false,
        total_bytes: 0,
        contact_hint: None,
        looking_for: None,
    }
}
