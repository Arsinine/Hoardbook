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

// One restatement per crate: reuse the harness constant, which is pinned to hb-app's source.
use crate::harness::ONLINE_WINDOW_SECS as WINDOW;
const TTL: u64 = 30 * 60;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("COUNT1 online distinct + multi-relay dedup + canary excluded", count1(ctx).await),
        result("COUNT2 userbase distinct across kinds + canary excluded", count2(ctx).await),
        result("COUNT3 canary-no-pollution (counts + discovery)", count3(ctx).await),
    ]
}

fn teaser(name: &str, tags: Vec<String>, cts: Vec<String>) -> Teaser {
    Teaser { display_name: name.into(), bio: String::new(), tags, content_types: cts, picture: None }
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

/// COUNT2: a real author publishing two kinds (teaser + presence) raises `count_userbase_for` by one
/// (distinct across kinds); a canary author raises it by zero. The reads are **scoped to the authors
/// the test minted** via `count_userbase_for`, so the relay's global cap/window — which made the
/// unbounded `count_userbase` read return different subsets on two successive calls (COUNT2 red since
/// 2026-08-03) — can no longer move the figure. The tally itself is unchanged: it still runs through
/// the production `count_distinct_userbase` (sig-verified, canary-excluded, distinct-by-author), so
/// scoping the *fetch* does not change *what is counted*.
async fn count2(ctx: &Ctx) -> Result<()> {
    let observer = Identity::generate();
    let client = ctx.connect(&observer).await?;

    let a = Identity::generate();
    let before = hb_net::count_userbase_for(&client, &[a.public_key()], FETCH_TIMEOUT).await?;

    let ac = ctx.connect(&a).await?;
    ac.publish(&build_teaser(&a, &teaser("a", vec![ctx.tag("cnt")], vec![ctx.tag("video")]), true)?).await?;
    ac.publish(&build_binding(&a, now(), TTL)?).await?;
    ac.disconnect().await;
    settle().await;
    let after_a = hb_net::count_userbase_for(&client, &[a.public_key()], FETCH_TIMEOUT).await?;
    ensure!(
        after_a == before + 1,
        "one new author across two kinds must count once: before={before} after={after_a}"
    );

    let canary = Identity::generate();
    let cc = ctx.connect(&canary).await?;
    cc.publish(&with_canary_marker(&canary, &build_teaser(&canary, &teaser("c", vec![ctx.tag("cnt")], vec![ctx.tag("video")]), true)?)?).await?;
    cc.publish(&with_canary_marker(&canary, &build_binding(&canary, now(), TTL)?)?).await?;
    cc.disconnect().await;
    settle().await;
    let after_canary =
        hb_net::count_userbase_for(&client, &[a.public_key(), canary.public_key()], FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(
        after_canary == after_a,
        "the canary author polluted the userbase count: after_a={after_a} after_canary={after_canary}"
    );
    Ok(())
}

/// COUNT3 — the end-to-end canary-no-pollution regression: run a full canary cycle, then prove (a)
/// the canary's own author is invisible to BOTH counts (online + userbase) even when you ask the
/// relay specifically for its events, and (b) a canary teaser is invisible to a tag search while an
/// identical non-canary teaser is found.
///
/// **Determinism (2026-08-10):** the reads are scoped to the canary's own author via
/// `fetch_presence_for_authors` / `count_userbase_for`. The previous unbounded `count_online` /
/// `count_userbase` reads were global across authors, so ambient presence/userbase traffic on the
/// shared relay moved them between reads (COUNT3's online assertion failed `5 -> 0` on the SG relay
/// even though `count_online` is `.since()`-bounded). Scoping to the canary's own author makes both
/// halves deterministic while keeping the assertion exact: the canary's marked events must tally
/// zero even when the fetch asks for that author by key — the marker, not query obscurity, is what
/// excludes them.
async fn count3(ctx: &Ctx) -> Result<()> {
    let observer = Identity::generate();
    let client = ctx.connect(&observer).await?;

    // A full canary cycle publishes a marked teaser+listing+presence (+ a DM, + cross-region).
    let run = crate::canary::run_canary(&ctx.relays).await;
    ensure!(run.all_passed(), "the canary cycle itself failed: {}", run.to_json());
    settle().await;

    // The canary's ephemeral key: recover the PublicKey from the run's npub to scope the reads.
    let canary_pk = hb_core::identity::parse_npub(&run.npub)?;
    let canary_authors = std::slice::from_ref(&canary_pk);

    // (a) The canary's own author must be invisible to both counts. The cycle just published a
    // marked teaser+listing+presence for this exact key, so a non-zero tally here would mean the
    // marker exclusion regressed. `fetch_presence_for_authors` is the author-bounded read immune to
    // the global relay cap (its map length IS the online count for these authors); the userbase
    // count goes through the same production `count_distinct_userbase`.
    let online_map = hb_net::fetch_presence_for_authors(&client, canary_authors, WINDOW, FETCH_TIMEOUT).await?;
    let online_after = online_map.0.len();
    ensure!(
        online_after == 0,
        "canary presence polluted the online count (author-scoped): {online_after} for the canary's own npub"
    );
    let users_after = hb_net::count_userbase_for(&client, canary_authors, FETCH_TIMEOUT).await?;
    ensure!(
        users_after == 0,
        "canary events polluted the userbase count (author-scoped): {users_after} for the canary's own npub"
    );

    // Discovery: a canary teaser under a unique tag must NOT surface; a real one with the same tag must.
    let uniq = ctx.tag("canarydisc");
    let canary = Identity::generate();
    let canary_teaser = with_canary_marker(
        &canary,
        &build_teaser(&canary, &teaser("disc-canary", vec![uniq.clone()], vec![ctx.tag("video")]), true)?,
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
    rc.publish(&build_teaser(&real, &teaser("disc-real", vec![uniq.clone()], vec![ctx.tag("video")]), true)?).await?;
    rc.disconnect().await;
    settle().await;
    let hits2 = hb_net::search_teasers(&client, &[uniq], &[], 100, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(hits2.len() == 1, "a real teaser with the same tag must be discoverable (got {})", hits2.len());
    Ok(())
}
