//! Suite COUNT — relay-derived count (M9 Track C; TEST_PLAN §4, extends Suite DISC/N). Proves the
//! `hb_net::count_online`/`count_userbase` queries end-to-end against the live (ephemeral) relay set,
//! plus the **canary-no-pollution** guarantee. Assertions are **differential** (before/after a
//! publish) so they are robust to the events earlier suites left on the shared relay — the absolute
//! count is irrelevant; the delta a known publish causes is what is asserted.

use anyhow::{ensure, Result};
use hb_core::event::{build_listing_event, build_teaser, Teaser};
use hb_core::{build_binding, Identity};

use crate::canary::with_canary_marker;
use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

const WINDOW: u64 = 600; // online freshness window (Decision #12)
const TTL: u64 = 30 * 60;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("COUNT1 online distinct + multi-relay dedup + canary excluded", count1(ctx).await),
        result("COUNT2 userbase distinct across kinds + canary excluded", count2(ctx).await),
        result("COUNT3 canary-no-pollution (counts + discovery)", count3(ctx).await),
    ]
}

fn teaser(name: &str, tags: Vec<String>, cts: Vec<String>) -> Teaser {
    Teaser { display_name: name.into(), bio: String::new(), tags, content_types: cts }
}

/// COUNT1: a fresh non-canary npub published to **every** relay raises `count_online` by exactly one
/// (distinct + multi-relay dedup); a fresh **canary** npub raises it by zero (marker exclusion).
async fn count1(ctx: &Ctx) -> Result<()> {
    let observer = Identity::generate();
    let client = ctx.connect(&observer).await?;
    let before = hb_net::count_online(&client, WINDOW, FETCH_TIMEOUT).await?;

    // A real fresh presence from a new npub, published to all relays.
    let real = Identity::generate();
    let rc = ctx.connect(&real).await?;
    rc.publish(&build_binding(&real, now(), TTL)?).await?;
    rc.disconnect().await;
    settle().await;
    let after_real = hb_net::count_online(&client, WINDOW, FETCH_TIMEOUT).await?;
    ensure!(
        after_real == before + 1,
        "a fresh npub on {} relay(s) must count exactly once (dedup): before={before} after={after_real}",
        ctx.relays.len()
    );

    // A canary-marked fresh presence from another new npub → must NOT be counted.
    let canary = Identity::generate();
    let cc = ctx.connect(&canary).await?;
    cc.publish(&with_canary_marker(&canary, &build_binding(&canary, now(), TTL)?)?).await?;
    cc.disconnect().await;
    settle().await;
    let after_canary = hb_net::count_online(&client, WINDOW, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(
        after_canary == after_real,
        "the canary npub polluted the online count: after_real={after_real} after_canary={after_canary}"
    );
    Ok(())
}

/// COUNT2: a real author publishing two kinds (teaser + presence) raises `count_userbase` by one
/// (distinct across kinds); a canary author raises it by zero.
async fn count2(ctx: &Ctx) -> Result<()> {
    let observer = Identity::generate();
    let client = ctx.connect(&observer).await?;
    let before = hb_net::count_userbase(&client, FETCH_TIMEOUT).await?;

    let a = Identity::generate();
    let ac = ctx.connect(&a).await?;
    ac.publish(&build_teaser(&a, &teaser("a", vec![ctx.tag("cnt")], vec![ctx.tag("video")]))?).await?;
    ac.publish(&build_binding(&a, now(), TTL)?).await?;
    ac.disconnect().await;
    settle().await;
    let after_a = hb_net::count_userbase(&client, FETCH_TIMEOUT).await?;
    ensure!(
        after_a == before + 1,
        "one new author across two kinds must count once: before={before} after={after_a}"
    );

    let canary = Identity::generate();
    let cc = ctx.connect(&canary).await?;
    cc.publish(&with_canary_marker(&canary, &build_teaser(&canary, &teaser("c", vec![ctx.tag("cnt")], vec![ctx.tag("video")]))?)?).await?;
    cc.publish(&with_canary_marker(&canary, &build_binding(&canary, now(), TTL)?)?).await?;
    cc.disconnect().await;
    settle().await;
    let after_canary = hb_net::count_userbase(&client, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(
        after_canary == after_a,
        "the canary author polluted the userbase count: after_a={after_a} after_canary={after_canary}"
    );
    Ok(())
}

/// COUNT3 — the end-to-end canary-no-pollution regression: run a full canary cycle, then prove (a)
/// neither count moved, and (b) a canary teaser is invisible to a tag search while an identical
/// non-canary teaser is found.
async fn count3(ctx: &Ctx) -> Result<()> {
    let observer = Identity::generate();
    let client = ctx.connect(&observer).await?;
    let online_before = hb_net::count_online(&client, WINDOW, FETCH_TIMEOUT).await?;
    let users_before = hb_net::count_userbase(&client, FETCH_TIMEOUT).await?;

    // A full canary cycle publishes a marked teaser+listing+presence (+ a DM, + cross-region).
    let run = crate::canary::run_canary(&ctx.relays).await;
    ensure!(run.all_passed(), "the canary cycle itself failed: {}", run.to_json());
    settle().await;

    let online_after = hb_net::count_online(&client, WINDOW, FETCH_TIMEOUT).await?;
    let users_after = hb_net::count_userbase(&client, FETCH_TIMEOUT).await?;
    ensure!(online_after == online_before, "canary presence polluted the online count: {online_before} -> {online_after}");
    ensure!(users_after == users_before, "canary events polluted the userbase count: {users_before} -> {users_after}");

    // Discovery: a canary teaser under a unique tag must NOT surface; a real one with the same tag must.
    let uniq = ctx.tag("canarydisc");
    let canary = Identity::generate();
    let canary_teaser = with_canary_marker(
        &canary,
        &build_teaser(&canary, &teaser("disc-canary", vec![uniq.clone()], vec![ctx.tag("video")]))?,
    )?;
    let cc = ctx.connect(&canary).await?;
    cc.publish(&canary_teaser).await?;
    // belt-and-braces: also publish a (marked) listing so a misfiring search couldn't leak it either.
    cc.publish(&with_canary_marker(&canary, &build_listing_event(&canary, "hbd-canary", &[5u8; 32], r#"{"entries":[]}"#)?)?).await?;
    cc.disconnect().await;
    settle().await;
    let hits = hb_net::search_teasers(&client, std::slice::from_ref(&uniq), &[], 100, FETCH_TIMEOUT).await?;
    ensure!(hits.is_empty(), "DISC tag search surfaced the canary teaser ({} hits)", hits.len());

    let real = Identity::generate();
    let rc = ctx.connect(&real).await?;
    rc.publish(&build_teaser(&real, &teaser("disc-real", vec![uniq.clone()], vec![ctx.tag("video")]))?).await?;
    rc.disconnect().await;
    settle().await;
    let hits2 = hb_net::search_teasers(&client, &[uniq], &[], 100, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(hits2.len() == 1, "a real teaser with the same tag must be discoverable (got {})", hits2.len());
    Ok(())
}
