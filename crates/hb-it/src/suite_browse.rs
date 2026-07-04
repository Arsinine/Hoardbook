//! Suite BROWSE — the M3 value loop end-to-end against a real relay: publish a collection
//! listing, discover it by tag-search, browse it back by share code, render partial listings on
//! loss, and prove a re-key kills the old browse-key for new listings. Drives the **real**
//! `hb-net` browse orchestration (`publish_listing` / `browse_share_code` / `search_teasers`) that
//! `hb-app` will call in M4 — the L2 suite tests production code, not a parallel reimpl.
//!
//! Maps to TEST_PLAN §4/§5a: PUB1-3 (N2/N3/N4 at the app layer), BR1-3 (N2/AB8/AB10),
//! SR1-3 (DISC1/DISC3/AB3), RK1 (AB9). Each publisher uses a fresh identity, so listing slugs are
//! author-scoped and need no run-id; cross-author discovery tags are namespaced via `ctx.tag()`.

use anyhow::{ensure, Result};
use hb_core::event::{build_listing_event, build_teaser, Teaser};
use hb_core::{Identity, ShareCode};
use hb_net::{browse_share_code, build_relay_list, publish_listing, search_teasers, split_listing};
use serde_json::Value;

use crate::harness::{result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    let mut out = vec![
        result("PUB1 publish→browse round-trip", pub1(ctx).await),
        result("PUB2 oversize split → browse full tree", pub2(ctx).await),
        result("PUB3 re-publish replaces (one current listing)", pub3(ctx).await),
        result("PUB4 deep single-root split → browse full tree", pub4(ctx).await),
        result("BR1 non-holder key → teaser, listing locked", br1(ctx).await),
        result("BR2 withheld part → K of N rendered", br2(ctx).await),
        result("BR3 browse resolves via relay fetch only", br3(ctx).await),
    ];
    // BR4 needs a 2nd relay (the peer's outbox the browser isn't initially connected to).
    out.push(if ctx.multi() {
        result("BR4 NIP-65 outbox: browse reaches a peer-only relay", br4(ctx).await)
    } else {
        TestResult::skip("BR4 NIP-65 outbox: browse reaches a peer-only relay", "needs a 2nd --relay")
    });
    out.extend([
        result("SR1 tag-search AND/OR over the wire", sr1(ctx).await),
        result("SR2 hit yields teaser, not listing", sr2(ctx).await),
        result("SR3 junk flood → deduped + capped", sr3(ctx).await),
        result("RK1 re-key kills old key for new listings", rk1(ctx).await),
    ]);
    out
}

fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn teaser(name: &str, tags: Vec<String>) -> Teaser {
    Teaser {
        display_name: name.into(),
        bio: "hoards".into(),
        tags,
        content_types: vec!["video".into()],
    }
}

/// A small listing payload with `n` named entries.
fn listing(slug: &str, n: usize) -> String {
    let entries: Vec<Value> =
        (0..n).map(|i| serde_json::json!({ "name": format!("title-{i:03}") })).collect();
    serde_json::json!({ "slug": slug, "content_types": ["video"], "entries": entries }).to_string()
}

/// A listing big enough to force a split under a 40 KiB part budget (≈70 KiB normalized at
/// n=1300, so the greedy chunker yields an index + ≥2 content parts).
fn big_listing(slug: &str, n: usize) -> String {
    let entries: Vec<Value> = (0..n)
        .map(|i| serde_json::json!({ "name": format!("title-{i:05}-padding-padding-padding-xx") }))
        .collect();
    serde_json::json!({ "slug": slug, "content_types": ["video"], "entries": entries }).to_string()
}

/// The real hoard shape (devtest #3 / M13): ONE root folder with `n` padded leaf files under it —
/// v1's breadth-only chunker could never split this; the depth-recursive v2 packer must.
fn deep_listing(slug: &str, n: usize) -> String {
    let children: Vec<Value> = (0..n)
        .map(|i| serde_json::json!({ "name": format!("file-{i:05}-padding-padding-padding-xx.bin") }))
        .collect();
    serde_json::json!({
        "slug": slug, "content_types": ["video"],
        "entries": [ { "name": "Movies", "children": children } ],
    })
    .to_string()
}

fn full_code(id: &Identity, browse_key: [u8; 32]) -> ShareCode {
    ShareCode::Full { pubkey: id.public_key(), browse_key }
}

async fn pub1(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let key = bk(1);
    let slug = "criterion";
    let json = listing(slug, 5);

    let client = ctx.connect(&id).await?;
    client.publish(&build_teaser(&id, &teaser("archivebox", vec!["anime".into()]))?).await?;
    let published = publish_listing(&client, &id, slug, &key, &json, 40_000).await?;
    ensure!(published.parts == 1, "a small listing publishes as one part, got {}", published.parts);
    settle().await;

    let code = full_code(&id, key);
    let res = browse_share_code(&client, &code, slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    ensure!(res.teaser.is_some(), "browse should surface the public teaser");
    let listing = res.listing.ok_or_else(|| anyhow::anyhow!("holder browse returned no listing"))?;
    ensure!(listing.complete(), "a fully-published listing browses complete");
    ensure!(listing.entries.len() == 5, "expected 5 entries, got {}", listing.entries.len());
    Ok(())
}

async fn pub2(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let key = bk(2);
    let slug = "huge";
    let n = 1300;
    let json = big_listing(slug, n);

    let client = ctx.connect(&id).await?;
    let published = publish_listing(&client, &id, slug, &key, &json, 40_000).await?;
    ensure!(published.parts > 2, "oversize listing must split, got {} part(s)", published.parts);
    settle().await;

    let res = browse_share_code(&client, &full_code(&id, key), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    let listing = res.listing.ok_or_else(|| anyhow::anyhow!("split listing did not browse"))?;
    ensure!(listing.complete(), "all parts present → complete tree");
    ensure!(listing.entries.len() == n, "restitched {} of {n} entries", listing.entries.len());
    Ok(())
}

async fn pub3(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let key = bk(3);
    let slug = "shelf";

    let client = ctx.connect(&id).await?;
    publish_listing(&client, &id, slug, &key, &listing(slug, 2), 40_000).await?;
    // A 1s gap makes the second publish strictly newer (replaceable events key on created_at).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    publish_listing(&client, &id, slug, &key, &listing(slug, 7), 40_000).await?;
    settle().await;

    let res = browse_share_code(&client, &full_code(&id, key), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    let listing = res.listing.ok_or_else(|| anyhow::anyhow!("no listing after replace"))?;
    ensure!(
        listing.entries.len() == 7,
        "replace should leave only the newest (7 entries), got {}",
        listing.entries.len()
    );
    Ok(())
}

async fn pub4(ctx: &Ctx) -> Result<()> {
    // M13 depth-recursive split end-to-end: the whole hoard under ONE root folder (the shape v1's
    // breadth-only chunker refused as "too large") must publish as a v2 multi-part family and
    // browse back as the complete, correctly-grafted tree.
    let id = Identity::generate();
    let key = bk(7);
    let slug = "deephoard";
    let n = 3000;
    let json = deep_listing(slug, n);

    let client = ctx.connect(&id).await?;
    let published = publish_listing(&client, &id, slug, &key, &json, 40_000).await?;
    ensure!(published.parts > 2, "deep single-root listing must split, got {} part(s)", published.parts);
    settle().await;

    let res = browse_share_code(&client, &full_code(&id, key), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    let listing = res.listing.ok_or_else(|| anyhow::anyhow!("deep split listing did not browse"))?;
    ensure!(listing.complete(), "all parts present → complete tree");
    ensure!(listing.entries.len() == 1, "one root folder expected, got {}", listing.entries.len());
    let leaves = leaf_count(&listing.entries[0]);
    ensure!(leaves == n, "restitched {leaves} of {n} leaf files under the root folder");
    Ok(())
}

/// Recursive leaf count of a rendered entry: a node with no (or empty) `children` is one leaf; a
/// folder contributes the leaves beneath it.
fn leaf_count(node: &Value) -> usize {
    match node.get("children").and_then(Value::as_array) {
        Some(kids) if !kids.is_empty() => kids.iter().map(leaf_count).sum(),
        _ => 1,
    }
}

async fn br1(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let slug = "private";

    let client = ctx.connect(&id).await?;
    client.publish(&build_teaser(&id, &teaser("archivebox", vec!["anime".into()]))?).await?;
    publish_listing(&client, &id, slug, &bk(1), &listing(slug, 3), 40_000).await?;
    settle().await;

    // A full code with the WRONG browse-key: the teaser still shows; the listing is locked.
    let wrong = full_code(&id, bk(99));
    let res = browse_share_code(&client, &wrong, slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    ensure!(res.teaser.is_some(), "the public teaser must remain visible to a non-holder");
    ensure!(res.listing.is_none(), "a wrong browse-key must not decrypt the listing");
    Ok(())
}

async fn br2(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let key = bk(4);
    let slug = "lossy";
    let json = big_listing(slug, 1300);

    let parts = split_listing(slug, &json, 40_000)?;
    ensure!(parts.len() > 2, "need a split listing for the withhold case, got {}", parts.len());

    let client = ctx.connect(&id).await?;
    // Withhold the last content part — publish the index + every other part.
    for part in &parts[..parts.len() - 1] {
        client.publish(&build_listing_event(&id, &part.d_tag, &key, &part.json)?).await?;
    }
    settle().await;

    let res = browse_share_code(&client, &full_code(&id, key), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    let listing = res.listing.ok_or_else(|| anyhow::anyhow!("partial listing failed to render"))?;
    ensure!(!listing.complete(), "a withheld part must render partial, not complete");
    ensure!(
        listing.parts_present == listing.parts_total - 1,
        "expected exactly one missing part, present {} of {}",
        listing.parts_present,
        listing.parts_total
    );
    ensure!(listing.missing.len() == 1, "the withheld part must be reported in `missing`");
    Ok(())
}

async fn br3(ctx: &Ctx) -> Result<()> {
    // AB10/decision #3: a browse resolves entirely through RelayClient::fetch — there is no peer
    // dial. hb-net depends on no iroh/socket-to-peer API, so the browse completing purely from the
    // relay client *is* the positive assertion (a regression that routed browse through a peer
    // could not compile against this crate).
    let id = Identity::generate();
    let key = bk(5);
    let slug = "iprivate";

    let client = ctx.connect(&id).await?;
    publish_listing(&client, &id, slug, &key, &listing(slug, 4), 40_000).await?;
    settle().await;
    let res = browse_share_code(&client, &full_code(&id, key), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;

    let listing = res.listing.ok_or_else(|| anyhow::anyhow!("browse-via-relay returned no listing"))?;
    ensure!(listing.complete() && listing.entries.len() == 4, "browse resolved purely from relay reads");
    Ok(())
}

async fn br4(ctx: &Ctx) -> Result<()> {
    // NIP-65 outbox model: the peer advertises relay[1] as their write relay (in a kind-10002 on
    // relay[0]) and publishes their listing ONLY to relay[1]. A browser that starts connected to
    // relay[0] must resolve the relay-list, connect to relay[1] (via ensure_relays), and find the
    // listing there. Without acting on the resolution, the browse would see no listing.
    let id = Identity::generate();
    let key = bk(6);
    let slug = "outbox";
    let relay0 = ctx.relays[0].clone();
    let relay1 = ctx.relays[1].clone();

    // Peer's NIP-65 (write = relay1) published to relay0 only.
    let nip65 = build_relay_list(&id, std::slice::from_ref(&relay1), std::slice::from_ref(&relay1))?;
    let c0 = ctx.connect_one(&id, 0).await?;
    c0.publish(&nip65).await?;
    c0.disconnect().await;

    // Peer's listing published to relay1 only.
    let c1 = ctx.connect_one(&id, 1).await?;
    let listing_ev = build_listing_event(&id, slug, &key, &listing(slug, 4))?;
    c1.publish(&listing_ev).await?;
    c1.disconnect().await;
    settle().await;

    // Browser starts on relay0 only; seed/own = relay0. It must reach relay1 via NIP-65.
    let browser = ctx.connect_one(&Identity::generate(), 0).await?;
    let seed = vec![relay0.clone()];
    let res = browse_share_code(&browser, &full_code(&id, key), slug, &seed, &seed, FETCH_TIMEOUT).await?;
    browser.disconnect().await;

    ensure!(res.resolved_relays.iter().any(|r| r == &relay1), "NIP-65 should resolve the peer's outbox relay");
    let listing = res
        .listing
        .ok_or_else(|| anyhow::anyhow!("browse did not reach the peer's outbox relay (relay1)"))?;
    ensure!(listing.complete() && listing.entries.len() == 4, "the peer-only listing browsed via NIP-65");
    Ok(())
}

async fn sr1(ctx: &Ctx) -> Result<()> {
    // Two distinct authors; tags namespaced per-run AND suite (a `br-` base so a cross-suite tag
    // like DISC's `anime` can't leak into this AND-count). A has {anime, vhs}; B has {anime} only.
    let anime = ctx.tag("br-anime");
    let vhs = ctx.tag("br-vhs");
    let a = Identity::generate();
    let b = Identity::generate();
    let client_a = ctx.connect(&a).await?;
    client_a.publish(&build_teaser(&a, &teaser("a", vec![anime.clone(), vhs.clone()]))?).await?;
    client_a.disconnect().await;
    let client_b = ctx.connect(&b).await?;
    client_b.publish(&build_teaser(&b, &teaser("b", vec![anime.clone()]))?).await?;
    client_b.disconnect().await;
    settle().await;

    let client = ctx.connect(&Identity::generate()).await?;
    // AND: both terms required → only A.
    let and_hits = search_teasers(&client, &[anime.clone(), vhs.clone()], &[], 50, FETCH_TIMEOUT).await?;
    ensure!(and_hits.len() == 1, "AND-intersect should match only A, got {}", and_hits.len());
    ensure!(and_hits[0].npub == a.npub(), "the AND hit must be author A");
    // Single term → both A and B.
    let or_hits = search_teasers(&client, std::slice::from_ref(&anime), &[], 50, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(or_hits.len() == 2, "a single shared tag should match both authors, got {}", or_hits.len());
    Ok(())
}

async fn sr2(ctx: &Ctx) -> Result<()> {
    // DISC3: a tag-search hit carries the teaser only — never the (encrypted) listing.
    let tag = ctx.tag("br-solo");
    let id = Identity::generate();
    let client = ctx.connect(&id).await?;
    client.publish(&build_teaser(&id, &teaser("solo", vec![tag.clone()]))?).await?;
    // Also publish a listing for this author; search must NOT surface it.
    publish_listing(&client, &id, "secret", &bk(1), &listing("secret", 3), 40_000).await?;
    settle().await;

    let hits = search_teasers(&client, &[tag], &[], 50, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(hits.len() == 1, "expected the one teaser hit, got {}", hits.len());
    // The hit type structurally carries only a teaser; assert the teaser arrived (no listing leak).
    ensure!(hits[0].teaser.display_name == "solo", "teaser payload present without any listing");
    Ok(())
}

async fn sr3(ctx: &Ctx) -> Result<()> {
    // AB3 over the wire: dedup (a replaced teaser collapses to one npub) + cap. (Bad-sig discard is
    // the L1 half — strfry rejects invalid signatures, so a bad-sig event can't be stored here.)
    let tag = ctx.tag("br-flood");
    // Six distinct authors announce the tag; one of them republishes (dedup must hold).
    let mut authors = Vec::new();
    for i in 0..6 {
        let id = Identity::generate();
        let client = ctx.connect(&id).await?;
        client.publish(&build_teaser(&id, &teaser(&format!("f{i}"), vec![tag.clone()]))?).await?;
        if i == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            client.publish(&build_teaser(&id, &teaser("f0-again", vec![tag.clone()]))?).await?;
        }
        client.disconnect().await;
        authors.push(id);
    }
    settle().await;

    let client = ctx.connect(&Identity::generate()).await?;
    let all = search_teasers(&client, std::slice::from_ref(&tag), &[], 50, FETCH_TIMEOUT).await?;
    ensure!(all.len() == 6, "six authors should dedup to six hits (one republished), got {}", all.len());
    // Cap: ask for at most 3.
    let capped = search_teasers(&client, &[tag], &[], 3, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(capped.len() == 3, "result cap not honoured: got {}", capped.len());
    Ok(())
}

async fn rk1(ctx: &Ctx) -> Result<()> {
    // AB9 end-to-end: after re-keying a collection (a fresh browse-key, re-published under the same
    // slug), the OLD browse-key can no longer decrypt the now-current listing; the NEW key can.
    let id = Identity::generate();
    let slug = "rekey";
    let old = bk(10);
    let new = bk(11);

    let client = ctx.connect(&id).await?;
    publish_listing(&client, &id, slug, &old, &listing(slug, 4), 40_000).await?;
    settle().await;
    // Old code works before the re-key.
    let before = browse_share_code(&client, &full_code(&id, old), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    ensure!(before.listing.is_some(), "the old key should decrypt the pre-rekey listing");

    // Re-key: republish the same slug under the new browse-key (replaces the prior event).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    publish_listing(&client, &id, slug, &new, &listing(slug, 4), 40_000).await?;
    settle().await;

    let with_old = browse_share_code(&client, &full_code(&id, old), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    ensure!(with_old.listing.is_none(), "the leaked OLD key must not decrypt the re-keyed listing");
    let with_new = browse_share_code(&client, &full_code(&id, new), slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(with_new.listing.is_some(), "the new key must decrypt the re-keyed listing");
    Ok(())
}
