//! Suite BIGRELAY — M16 Layer 3: the FULL listing family on the owner's big relay, behind the
//! paywall teaser. Drives the real `hb-net` W2 primitives (`publish_listing_to` /
//! `fetch_full_listing_from` / `fetch_full_listing_if_current`) against two real relays —
//! `relay[0]` = a public relay, `relay[1]` = the big relay.
//!
//! Proves the three W2 properties end-to-end:
//!   BIG1 — the family publishes to the big relay **only** (never leaks to the public relay, INV-5),
//!          fetches back exclusively from the big relay, and restitches the complete tree; a
//!          fingerprint mismatch keeps the teaser.
//!   BIG2 — a **stale** big-relay snapshot (older fingerprint than the teaser's) does NOT supersede
//!          the teaser (M16 headline failure mode #1).
//!
//! Needs a 2nd `--relay`; skipped with one relay.

use anyhow::{ensure, Result};
use hb_core::event::KIND_LISTING;
use hb_core::Identity;
use hb_net::{fetch_full_listing_if_current, publish_listing_capped, publish_listing_to};
use nostr::prelude::*;
use serde_json::Value;

use crate::harness::{result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    if !ctx.multi() {
        return vec![
            TestResult::skip("BIG1 full family on the big relay → full tree, no public leak", "needs a 2nd --relay"),
            TestResult::skip("BIG2 stale big-relay snapshot → gate keeps the teaser", "needs a 2nd --relay"),
        ];
    }
    vec![
        result("BIG1 full family on the big relay → full tree, no public leak", big1(ctx).await),
        result("BIG2 stale big-relay snapshot → gate keeps the teaser", big2(ctx).await),
    ]
}

// The teaser and the family share this fingerprint (same source tree); FP_OLD is a stale snapshot.
const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const FP_OLD: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// A full listing of `n` padded entries carrying `snapshot_fingerprint = fp` — big enough to split
/// into an index + several content parts under the 40 KiB per-part budget.
fn full_listing(slug: &str, n: usize, fp: &str) -> String {
    let entries: Vec<Value> = (0..n)
        .map(|i| serde_json::json!({ "name": format!("title-{i:05}-padding-padding-padding-xx") }))
        .collect();
    serde_json::json!({
        "slug": slug, "content_types": ["video"], "snapshot_fingerprint": fp, "entries": entries,
    })
    .to_string()
}

async fn big1(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let key = bk(21);
    let slug = "vault";
    let n = 1300;
    let full = full_listing(slug, n, FP);
    let big = ctx.relays[1].clone();

    // Full family → the big relay (relay[1]) ONLY, via publish_to targeting.
    let cbig = ctx.connect_one(&owner, 1).await?;
    let published =
        publish_listing_to(&cbig, &owner, slug, &key, &full, 40_000, std::slice::from_ref(&big)).await?;
    ensure!(published.parts > 2, "the full family must split, got {} part(s)", published.parts);
    cbig.disconnect().await;

    // The truncated paywall teaser → the public relay (relay[0]) only.
    let cpub = ctx.connect_one(&owner, 0).await?;
    let teaser = publish_listing_capped(&cpub, &owner, slug, &key, &full, 40_000).await?;
    ensure!(teaser.truncated, "the same listing must truncate to a single teaser event");
    cpub.disconnect().await;
    settle().await;

    // A holder browses: fetch the full family from the big relay, gated on the teaser's fingerprint.
    let browser = ctx.connect(&Identity::generate()).await?;
    let current = fetch_full_listing_if_current(
        &browser,
        &owner.public_key(),
        slug,
        &key,
        std::slice::from_ref(&big),
        FP,
        FETCH_TIMEOUT,
    )
    .await?;
    let full_tree =
        current.ok_or_else(|| anyhow::anyhow!("a current big-relay family must supersede the teaser"))?;
    ensure!(full_tree.complete(), "the full family renders as a complete tree");
    ensure!(full_tree.entries.len() == n, "restitched {} of {n} entries", full_tree.entries.len());

    // A wrong fingerprint must NOT supersede (the stale-guard, exercised over the wire).
    let mismatched = fetch_full_listing_if_current(
        &browser,
        &owner.public_key(),
        slug,
        &key,
        std::slice::from_ref(&big),
        "deadbeef",
        FETCH_TIMEOUT,
    )
    .await?;
    ensure!(mismatched.is_none(), "a fingerprint mismatch must keep the teaser");
    browser.disconnect().await;

    // No leak (INV-5): the public relay holds ONLY the truncated single — none of the `#part` family.
    let cpub_read = ctx.connect_one(&Identity::generate(), 0).await?;
    let pub_events = cpub_read
        .fetch(Filter::new().author(owner.public_key()).kind(Kind::from_u16(KIND_LISTING)), FETCH_TIMEOUT)
        .await?;
    cpub_read.disconnect().await;
    let leaked_parts =
        pub_events.iter().any(|e| e.tags.identifier().is_some_and(|d| d.contains("#part")));
    ensure!(!leaked_parts, "the big-relay family must NOT leak to the public relay (INV-5)");
    ensure!(
        pub_events.iter().any(|e| e.tags.identifier() == Some(slug)),
        "the public relay must still hold the truncated teaser"
    );
    Ok(())
}

async fn big2(ctx: &Ctx) -> Result<()> {
    // Stale big relay: the family carries an OLDER fingerprint than the (newer) teaser the browser
    // knows. The gate must keep the teaser — never serve the stale full tree (M16 failure mode #1).
    let owner = Identity::generate();
    let key = bk(22);
    let slug = "restale";
    let big = ctx.relays[1].clone();

    let cbig = ctx.connect_one(&owner, 1).await?;
    publish_listing_to(
        &cbig,
        &owner,
        slug,
        &key,
        &full_listing(slug, 1300, FP_OLD),
        40_000,
        std::slice::from_ref(&big),
    )
    .await?;
    cbig.disconnect().await;
    settle().await;

    // The browser knows the teaser's newer fingerprint (FP); the big relay only has FP_OLD.
    let browser = ctx.connect(&Identity::generate()).await?;
    let current = fetch_full_listing_if_current(
        &browser,
        &owner.public_key(),
        slug,
        &key,
        std::slice::from_ref(&big),
        FP,
        FETCH_TIMEOUT,
    )
    .await?;
    browser.disconnect().await;
    ensure!(current.is_none(), "a stale big-relay snapshot must not supersede the teaser");
    Ok(())
}
