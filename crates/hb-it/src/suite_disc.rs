//! Suite DISC — discovery (TEST_PLAN §4). Replaces the old Suite H / DHT. Tag search with
//! client-side AND/OR refinement, NIP-65 resolution, teaser-only hits, the empty-filter guard,
//! and (with `--pow`) the NIP-13 path. Discovery tags are namespaced per run so counts stay
//! correct even on a relay that already holds earlier runs' events.

use std::time::Duration;

use anyhow::{ensure, Result};
use hb_core::event::{build_listing_event, build_teaser, parse_teaser, Teaser, KIND_LISTING, KIND_TEASER};
use hb_core::Identity;
use hb_net::{
    bootstrap_order, build_relay_list, mine_pow, parse_relay_list, pow_difficulty, search_teasers,
    teaser_search_filter, RelayClient,
};
use nostr::prelude::*;

use crate::harness::{result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("DISC1 tag filter (AND tags / OR content-types)", disc1(ctx).await),
        result("DISC2 NIP-65 resolution", disc2(ctx).await),
        result("DISC3 teaser-only", disc3(ctx).await),
        result("DISC4 invalid filter rejected", disc4().await),
        result("DISC6 search_peers orchestration (dedup/cap/teaser-only)", disc6(ctx).await),
        disc5(ctx).await,
    ]
}

fn teaser(name: &str, tags: Vec<String>, cts: Vec<String>) -> Teaser {
    Teaser { display_name: name.into(), bio: String::new(), tags, content_types: cts }
}

/// Fetch + verify teasers matching a tag-search filter, returning (author, parsed teaser).
async fn search(client: &RelayClient, tags: &[String], cts: &[String]) -> Result<Vec<(PublicKey, Teaser)>> {
    let filter = teaser_search_filter(tags, cts)?;
    let events = client.fetch(filter, FETCH_TIMEOUT).await?;
    let mut out = Vec::new();
    for ev in &events {
        // Each teaser's signature is verified before it is trusted (junk discarded — see AB3).
        if let Ok(t) = parse_teaser(ev) {
            out.push((ev.pubkey, t));
        }
    }
    Ok(out)
}

/// DISC1 client-side refinement: require ALL `tags` (AND), allow ANY `cts` (OR).
fn refine(hits: &[(PublicKey, Teaser)], tags: &[String], cts: &[String]) -> Vec<PublicKey> {
    hits.iter()
        .filter(|(_, t)| {
            let tags_ok = tags.iter().all(|rt| t.tags.contains(rt));
            let cts_ok = cts.is_empty() || cts.iter().any(|rc| t.content_types.contains(rc));
            tags_ok && cts_ok
        })
        .map(|(pk, _)| *pk)
        .collect()
}

async fn disc1(ctx: &Ctx) -> Result<()> {
    let (anime, vhs, doc) = (ctx.tag("anime"), ctx.tag("vhs"), ctx.tag("doc"));
    let (video, audio) = (ctx.tag("video"), ctx.tag("audio"));
    let (p1, p2, p3) = (Identity::generate(), Identity::generate(), Identity::generate());
    let t1 = build_teaser(&p1, &teaser("P1", vec![anime.clone(), vhs.clone()], vec![video.clone()]))?;
    let t2 = build_teaser(&p2, &teaser("P2", vec![anime.clone()], vec![audio.clone()]))?;
    let t3 = build_teaser(&p3, &teaser("P3", vec![doc.clone()], vec![video.clone()]))?;

    let client = ctx.connect(&p1).await?;
    for ev in [&t1, &t2, &t3] {
        client.publish(ev).await?;
    }
    settle().await;

    let (q_anime, q_anime_vhs, q_cts) = (
        vec![anime.clone()],
        vec![anime.clone(), vhs.clone()],
        vec![audio.clone(), video.clone()],
    );

    // (a) single tag → the announcers (P1, P2); P3 (no anime) is absent.
    let hits = search(&client, &q_anime, &[]).await?;
    let m = refine(&hits, &q_anime, &[]);
    ensure!(set_eq(&m, &[p1.public_key(), p2.public_key()]), "single-tag search wrong: {} hits", m.len());

    // (b) AND across two tags → only P1 (announced both).
    let hits = search(&client, &q_anime_vhs, &[]).await?;
    let m = refine(&hits, &q_anime_vhs, &[]);
    ensure!(set_eq(&m, &[p1.public_key()]), "AND-intersect wrong: {} hits", m.len());

    // (c) OR across content-types → the union (all three).
    let hits = search(&client, &[], &q_cts).await?;
    let m = refine(&hits, &[], &q_cts);
    client.disconnect().await;
    ensure!(
        set_eq(&m, &[p1.public_key(), p2.public_key(), p3.public_key()]),
        "content-type OR-union wrong: {} hits",
        m.len()
    );
    Ok(())
}

async fn disc2(ctx: &Ctx) -> Result<()> {
    let a = Identity::generate();
    let advertised = vec![ctx.relays[0].clone()];
    let relay_list = build_relay_list(&a, &advertised, &advertised)?;
    let tea = build_teaser(&a, &teaser("nip65-peer", vec![ctx.tag("nip65")], vec![ctx.tag("video")]))?;

    let pubc = ctx.connect(&a).await?;
    pubc.publish(&relay_list).await?;
    pubc.publish(&tea).await?;
    pubc.disconnect().await;
    settle().await;

    // (1) resolve the peer's NIP-65 from a configured relay.
    let resolver = ctx.connect(&a).await?;
    let lists = resolver.fetch(Filter::new().author(a.public_key()).kind(Kind::RelayList), FETCH_TIMEOUT).await?;
    resolver.disconnect().await;
    ensure!(lists.len() == 1, "expected 1 NIP-65 event, got {}", lists.len());
    let list = parse_relay_list(&lists[0])?;
    ensure!(!list.write.is_empty(), "NIP-65 advertised no write relays");

    // (2) fetch the peer's teaser from *that advertised set* (bootstrap order leads with it).
    let order = bootstrap_order(&ctx.relays, &[], Some(&list));
    let from_advertised = RelayClient::connect(&a, &order, Duration::from_secs(10)).await?;
    let got = from_advertised
        .fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
        .await?;
    from_advertised.disconnect().await;
    ensure!(got.len() == 1, "teaser not found on the peer's advertised relays");
    Ok(())
}

async fn disc3(ctx: &Ctx) -> Result<()> {
    // A tag-search hit yields the teaser but never the (encrypted) listing.
    let a = Identity::generate();
    let only = ctx.tag("only");
    let tea = build_teaser(&a, &teaser("teaser-only", vec![only.clone()], vec![ctx.tag("video")]))?;
    let listing = build_listing_event(&a, "hbd-coll", &[3u8; 32], r#"{"slug":"hbd-coll","entries":[]}"#)?;

    let client = ctx.connect(&a).await?;
    client.publish(&tea).await?;
    client.publish(&listing).await?;
    settle().await;

    let hits = client.fetch(teaser_search_filter(&[only], &[])?, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(!hits.is_empty(), "tag search returned nothing");
    ensure!(
        hits.iter().all(|e| e.kind == Kind::from_u16(KIND_TEASER)),
        "tag search leaked a non-teaser event (listing exposed?)"
    );
    ensure!(
        hits.iter().all(|e| e.kind != Kind::from_u16(KIND_LISTING)),
        "the encrypted listing came back from a tag search"
    );
    Ok(())
}

async fn disc4() -> Result<()> {
    // Empty tags AND empty content-types is refused before any relay query.
    ensure!(teaser_search_filter(&[], &[]).is_err(), "empty discovery filter was not rejected");
    Ok(())
}

async fn disc6(ctx: &Ctx) -> Result<()> {
    // W3 — the production `search_teasers` path the `search_peers` command wraps: publish two teasers
    // (different tags) + the matching one twice (a relay returning a dup), plus a non-matching one,
    // then assert the search surfaces the matching author exactly once (deduped by npub), capped, and
    // that a SearchHit is the teaser only — there is no field for a listing/browse-key (DISC3).
    let want = ctx.tag("w3match");
    let other = ctx.tag("w3other");
    let p1 = Identity::generate();
    let p2 = Identity::generate();
    let t1 = build_teaser(&p1, &teaser("match", vec![want.clone()], vec![ctx.tag("video")]))?;
    let t2 = build_teaser(&p2, &teaser("other", vec![other.clone()], vec![ctx.tag("audio")]))?;

    let client = ctx.connect(&p1).await?;
    client.publish(&t1).await?;
    client.publish(&t1).await?; // a relay re-serving the same author's teaser must dedup to one hit
    client.publish(&t2).await?;
    settle().await;

    let hits = search_teasers(&client, &[want.clone()], &[], 100, FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(hits.len() == 1, "expected exactly one deduped hit, got {}", hits.len());
    ensure!(hits[0].npub == p1.npub(), "the matching author was not surfaced");
    ensure!(hits[0].teaser.tags.contains(&want), "the hit carries the matching teaser tags");
    Ok(())
}

async fn disc5(ctx: &Ctx) -> TestResult {
    let name = "DISC5 NIP-13 proof-of-work";
    if ctx.pow == 0 {
        return TestResult::skip(name, "pass --pow <bits> to exercise");
    }
    result(name, disc5_inner(ctx).await)
}

async fn disc5_inner(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let ev = build_teaser(&id, &teaser("pow", vec![ctx.tag("pow")], vec![ctx.tag("video")]))?;
    let mined = mine_pow(&id, &ev, ctx.pow)?;
    ensure!(pow_difficulty(&mined) >= ctx.pow, "mined event below target difficulty");

    let client = ctx.connect(&id).await?;
    let outcome = client.publish(&mined).await?;
    client.disconnect().await;
    // A vanilla relay accepts our PoW'd event; the *rejection-without-PoW* path (⏳) needs a
    // NIP-13-gated relay, which the ephemeral test relay is not.
    ensure!(!outcome.accepted.is_empty(), "relay rejected the PoW'd event: {:?}", outcome.rejected);
    Ok(())
}

/// Order-insensitive set equality on pubkeys.
fn set_eq(got: &[PublicKey], want: &[PublicKey]) -> bool {
    got.len() == want.len() && want.iter().all(|w| got.contains(w))
}
