//! Suite PRIV — Private Collections (per-recipient gift-wrapped listings, M10; TEST_PLAN §4). The
//! private path: `seal_private_listing` → `publish_private_listing` (all relays) →
//! `fetch_private_listings` (a trusted recipient opens; a non-trusted / browse-key holder finds
//! nothing). The crypto negatives live at L1 (hb-core `priv_listing`); this suite proves the
//! relay round-trip + the **observable** invariants: the inner-author allowlist is enforced
//! post-decrypt, retries dedup, revoke kills the republish, and a private collection is invisible
//! to a tag search.

use anyhow::{ensure, Result};
use hb_core::{seal_private_listing, Identity};
use hb_net::{fetch_private_listings, publish_private_listing, search_teasers};
use nostr::prelude::*;

use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

const LISTING: &str = r#"{"slug":"vault","content_types":["video"],"entries":[{"name":"rare.mkv"}]}"#;
const LISTING_V2: &str =
    r#"{"slug":"vault","content_types":["video"],"entries":[{"name":"rare.mkv"},{"name":"new.mkv"}]}"#;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        priv1(ctx).await,
        result("PRIV2 trusted recipient opens; inner author ∈ allowlist", priv2(ctx).await),
        result("PRIV3 foreign/spoofed inner is filtered out post-decrypt", priv3(ctx).await),
        result("PRIV4 retried publish dedups to one by inner content", priv4(ctx).await),
        result("PRIV5 non-trusted peer finds no private listing (no hint)", priv5(ctx).await),
        result("PRIV6 revoke-on-republish: removed recipient can't read the new event", priv6(ctx).await),
        result("PRIV7 private tags never appear in a tag search (DISC1)", priv7(ctx).await),
    ]
}

/// PRIV1 (F14 multi-relay): the N wraps publish to ALL relays, and the recipient can fetch their
/// wrap from **each** relay individually. Gated to multi-relay.
async fn priv1(ctx: &Ctx) -> TestResult {
    let name = "PRIV1 multi-relay publish: wrap fetchable from each relay (F14)";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, priv1_inner(ctx).await)
}

async fn priv1_inner(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let wraps = seal_private_listing(&author, &[r.public_key()], LISTING, now())?;

    let ac = ctx.connect(&author).await?;
    publish_private_listing(&ac, &wraps).await?;
    ac.disconnect().await;
    settle().await;

    // Fetch from each relay on its own — the wrap must be present on both (multi-published).
    for idx in 0..ctx.relays.len() {
        let rc = ctx.connect_one(&r, idx).await?;
        let got = fetch_private_listings(&rc, &r, &[author.public_key()], FETCH_TIMEOUT).await?;
        rc.disconnect().await;
        ensure!(got.len() == 1, "relay {idx}: expected the private listing, got {}", got.len());
        ensure!(got[0].listing_json == LISTING, "relay {idx}: listing plaintext mismatch");
    }
    Ok(())
}

/// PRIV2: a trusted recipient opens the listing; the INNER author (behind the ephemeral outer) is
/// matched against the allowlist — a valid author passes, and an empty allowlist filters it out.
async fn priv2(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let wraps = seal_private_listing(&author, &[r.public_key()], LISTING, now())?;

    let ac = ctx.connect(&author).await?;
    publish_private_listing(&ac, &wraps).await?;
    ac.disconnect().await;
    settle().await;

    let rc = ctx.connect(&r).await?;
    // The relay filter is `#p == me` (recipient tag), NOT an author filter: the fetched wrap's outer
    // author is the ephemeral key, never the recipient. Assert that explicitly (chorus: prove `#p`,
    // not author, is the match key).
    let raw =
        rc.fetch(Filter::new().kind(Kind::GiftWrap).pubkey(r.public_key()), FETCH_TIMEOUT).await?;
    ensure!(!raw.is_empty(), "the recipient's #p inbox holds the wrap");
    ensure!(
        raw.iter().all(|e| e.pubkey != r.public_key()),
        "the fetched wrap's outer author is ephemeral (≠ recipient) — matched by #p tag, not author"
    );
    // Trusted: author is in the allowlist → opens.
    let got = fetch_private_listings(&rc, &r, &[author.public_key()], FETCH_TIMEOUT).await?;
    ensure!(got.len() == 1, "trusted recipient should open exactly one listing, got {}", got.len());
    ensure!(got[0].listing_json == LISTING, "listing plaintext mismatch");
    ensure!(got[0].inner_author == author.public_key(), "inner author behind the ephemeral wrap is the real author");
    // Same recipient, empty allowlist → the author is not trusted → filtered to nothing.
    let none = fetch_private_listings(&rc, &r, &[], FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(none.is_empty(), "an empty allowlist must filter out a valid wrap (post-decrypt author check)");
    Ok(())
}

/// PRIV3: an untrusted sender E seals to the same recipient R. R, whose allowlist is {T} (the
/// trusted author), opens E's wrap but the inner author E ∉ allowlist, so it is filtered — only T's
/// listing survives. And a foreign recipient (attacker A) finds nothing in its own inbox.
async fn priv3(ctx: &Ctx) -> Result<()> {
    let trusted = Identity::generate();
    let evil = Identity::generate();
    let r = Identity::generate();
    let attacker = Identity::generate();

    let t_wraps = seal_private_listing(&trusted, &[r.public_key()], LISTING, now())?;
    let e_wraps = seal_private_listing(&evil, &[r.public_key()], LISTING_V2, now())?;

    let pc = ctx.connect(&trusted).await?;
    publish_private_listing(&pc, &t_wraps).await?;
    publish_private_listing(&pc, &e_wraps).await?;
    pc.disconnect().await;
    settle().await;

    let rc = ctx.connect(&r).await?;
    let got = fetch_private_listings(&rc, &r, &[trusted.public_key()], FETCH_TIMEOUT).await?;
    ensure!(got.len() == 1, "only the trusted author's listing survives the allowlist, got {}", got.len());
    ensure!(got[0].inner_author == trusted.public_key(), "the surviving listing is the trusted author's");
    ensure!(got[0].listing_json == LISTING, "the spoofed (untrusted-author) listing must not surface");

    // Foreign recipient: the attacker's inbox holds none of R's wraps.
    let af = fetch_private_listings(&rc, &attacker, &[trusted.public_key()], FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(af.is_empty(), "a foreign recipient must fetch no private listing addressed to R");
    Ok(())
}

/// PRIV4: a retried publish (two seals of the same listing → two wraps with distinct outer ids,
/// fresh ephemeral keys) dedups to a single opened listing on the read side.
async fn priv4(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let t = now();
    // Two independent seals of the same listing at the same logical time (a retry).
    let first = seal_private_listing(&author, &[r.public_key()], LISTING, t)?;
    let retry = seal_private_listing(&author, &[r.public_key()], LISTING, t)?;
    ensure!(first[0].id != retry[0].id, "a retry has a distinct outer event id (fresh ephemeral)");

    let ac = ctx.connect(&author).await?;
    publish_private_listing(&ac, &first).await?;
    publish_private_listing(&ac, &retry).await?;
    ac.disconnect().await;
    settle().await;

    let rc = ctx.connect(&r).await?;
    let got = fetch_private_listings(&rc, &r, &[author.public_key()], FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(got.len() == 1, "two wraps of the same listing must dedup to one, got {}", got.len());
    Ok(())
}

/// PRIV5: a peer who is not a recipient fetches and finds nothing — there is no "this collection is
/// private" hint to discover (the wrap is p-tagged only to the real recipient).
async fn priv5(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let outsider = Identity::generate();
    let wraps = seal_private_listing(&author, &[r.public_key()], LISTING, now())?;

    let ac = ctx.connect(&author).await?;
    publish_private_listing(&ac, &wraps).await?;
    ac.disconnect().await;
    settle().await;

    let oc = ctx.connect(&outsider).await?;
    // Even with the author "trusted", the outsider's inbox (p-tag = outsider) holds no wrap.
    let got = fetch_private_listings(&oc, &outsider, &[author.public_key()], FETCH_TIMEOUT).await?;
    // And a raw scan of gift-wraps addressed to the outsider returns nothing about the collection.
    let raw = oc
        .fetch(Filter::new().kind(Kind::GiftWrap).pubkey(outsider.public_key()), FETCH_TIMEOUT)
        .await?;
    oc.disconnect().await;
    ensure!(got.is_empty(), "a non-recipient must surface no private listing");
    ensure!(raw.is_empty(), "no gift-wrap is addressed to a non-recipient — no enumeration hint");
    Ok(())
}

/// PRIV6 (revoke = re-seal on republish, the AB9 model): round 1 seals to {R, S}; round 2 republishes
/// an *updated* listing to {S} only. S sees the new listing; R is frozen at the round-1 snapshot and
/// never receives (cannot decrypt) the round-2 event. Honest caveat: R's already-fetched round-1
/// copy is unaffected — revoke stops only future republishes.
async fn priv6(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let s = Identity::generate();
    let allow = [author.public_key()];

    let round1 = seal_private_listing(&author, &[r.public_key(), s.public_key()], LISTING, now())?;
    let ac = ctx.connect(&author).await?;
    publish_private_listing(&ac, &round1).await?;
    settle().await;

    // R can read round 1.
    let rc = ctx.connect(&r).await?;
    let r_round1 = fetch_private_listings(&rc, &r, &allow, FETCH_TIMEOUT).await?;
    ensure!(r_round1.len() == 1 && r_round1[0].listing_json == LISTING, "R reads the round-1 listing");

    // Round 2: R is revoked; republish the UPDATED listing to S only.
    let round2 = seal_private_listing(&author, &[s.public_key()], LISTING_V2, now() + 1)?;
    publish_private_listing(&ac, &round2).await?;
    ac.disconnect().await;
    settle().await;

    // S sees the new listing (newest wins per slug); R never gets the round-2 update.
    let sc = ctx.connect(&s).await?;
    let s_now = fetch_private_listings(&sc, &s, &allow, FETCH_TIMEOUT).await?;
    sc.disconnect().await;
    ensure!(s_now.len() == 1 && s_now[0].listing_json == LISTING_V2, "S reads the republished (updated) listing");

    let r_now = fetch_private_listings(&rc, &r, &allow, FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(
        r_now.iter().all(|o| o.listing_json != LISTING_V2),
        "a revoked recipient must NOT be able to decrypt the republished event"
    );
    Ok(())
}

/// PRIV7 (DISC1): a private collection is never tag-discoverable. Publishing the private wraps emits
/// no public `t`-tagged teaser, so a tag search for a term inside the private listing finds nothing.
async fn priv7(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let secret_tag = ctx.tag("privsecret");
    let listing = format!(r#"{{"slug":"vault","tags":["{secret_tag}"],"entries":[]}}"#);
    let wraps = seal_private_listing(&author, &[r.public_key()], &listing, now())?;

    let ac = ctx.connect(&author).await?;
    publish_private_listing(&ac, &wraps).await?;
    settle().await;
    let hits = search_teasers(&ac, std::slice::from_ref(&secret_tag), &[], 50, FETCH_TIMEOUT).await?;
    ac.disconnect().await;
    ensure!(
        hits.iter().all(|h| h.npub != author.npub()),
        "a private-collection tag must never be discoverable via a public tag search"
    );
    Ok(())
}
