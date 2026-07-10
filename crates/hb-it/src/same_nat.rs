//! Suite SAMENAT — same-NAT presence diagnosis (devtest #9, 2026-07-10). **Diagnosis-only**: proves
//! (or disproves) that two identities publishing presence from the same source IP both count as
//! online. No fix lives here — a shortfall just names which relay rejected which identity.
//!
//! `--same-nat` runs [`run`] against the **live** relays (the only environment where a per-IP cap
//! would bite). The CI-runnable differential row in Suite RELAY (RELAY4) shares this body against
//! the ephemeral strfry CI relay, which has no per-IP cap — green there proves the plumbing, not the
//! absence of the bug; the live diagnostic is `--same-nat` against production.

use anyhow::{ensure, Result};
use hb_core::{build_binding, verify_binding, Identity};
use hb_net::{count_online, select_newest_by_created_at, RelayClient};
use nostr::prelude::*;

use crate::harness::{is_online, now, result, settle, Ctx, FETCH_TIMEOUT, ONLINE_WINDOW_SECS};
use crate::tap::TestResult;

/// A presence beacon TTL comfortably inside the count's freshness window.
const PRESENCE_TTL: u64 = 1800;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![result("SAMENAT1 two identities from one IP each count online (+2)", same_nat1(ctx).await)]
}

/// The shared body: two identities, both connected from **this process** (one source IP), each
/// publish a presence beacon; the observer's online count must rise by exactly 2. Every per-relay
/// `PublishOutcome` is dumped to stderr — the evidence a same-NAT reject would otherwise leave
/// invisible below `tracing::debug`.
pub(crate) async fn same_nat1(ctx: &Ctx) -> Result<()> {
    let observer = Identity::generate();
    let oc = ctx.connect(&observer).await?;
    let before = count_online(&oc, ONLINE_WINDOW_SECS, FETCH_TIMEOUT).await?;

    let alice = Identity::generate();
    let bob = Identity::generate();
    let ac = ctx.connect(&alice).await?;
    let bc = ctx.connect(&bob).await?;

    let a_outcome = ac.publish(&build_binding(&alice, now(), PRESENCE_TTL)?).await?;
    let b_outcome = bc.publish(&build_binding(&bob, now(), PRESENCE_TTL)?).await?;
    // The evidence dump: a shortfall below names exactly which relay rejected which identity.
    eprintln!("   SAMENAT1 alice publish: accepted={:?} rejected={:?}", a_outcome.accepted, a_outcome.rejected);
    eprintln!("   SAMENAT1 bob   publish: accepted={:?} rejected={:?}", b_outcome.accepted, b_outcome.rejected);
    settle().await;

    verify_peer_online(&oc, &alice).await?;
    verify_peer_online(&oc, &bob).await?;
    let after = count_online(&oc, ONLINE_WINDOW_SECS, FETCH_TIMEOUT).await?;

    ac.disconnect().await;
    bc.disconnect().await;
    oc.disconnect().await;

    ensure!(
        after == before + 2,
        "same-NAT presence shortfall: before={before} after={after} (expected +2 — a shortfall names a per-IP relay reject above)"
    );
    Ok(())
}

/// Fetch `peer`'s newest presence via `client` and assert it verifies as a fresh online beacon.
/// (Mirrors `suite_relay::verify_peer_online` — small enough that duplicating beats a premature
/// shared helper across two suites that evolve independently.)
async fn verify_peer_online(client: &RelayClient, peer: &Identity) -> Result<()> {
    let events = client
        .fetch(
            Filter::new().author(peer.public_key()).kind(Kind::from_u16(hb_core::binding::KIND_PRESENCE)),
            FETCH_TIMEOUT,
        )
        .await?;
    let newest = select_newest_by_created_at(events).ok_or_else(|| anyhow::anyhow!("no presence for peer"))?;
    ensure!(is_online(newest.created_at.as_u64(), now()), "peer's presence is not within the online window");
    verify_binding(&newest, &peer.public_key(), now()).map_err(|e| anyhow::anyhow!("verify_binding failed: {e}"))?;
    Ok(())
}
