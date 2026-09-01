//! Suite KG — Key Grant (QURATOR-160): deliver a browse key sealed per recipient, kind 31_114.
//!
//! The **crypto negatives live at L1** (`hb_core::priv_listing`'s unit tests already pin the
//! tampered-rumor-pubkey, wrong-inner-kind, outer-kind-fuzz and non-recipient cases). This suite
//! covers what L1 structurally cannot see: the **relay round-trip** and the **observable inbox**
//! behaviour — what a real relay stores, returns, and mixes together.
//!
//! Three things are only true at L2:
//!   * the browse key survives a relay round-trip byte-identical, and the event the relay hands
//!     back carries no plaintext of it (INV-2, checked on the wire rather than in memory);
//!   * `p`-tag routing is what keeps a grant out of a non-recipient's inbox — there is no
//!     "a key was granted" hint to enumerate;
//!   * a real `#p` inbox mixes 31_113 private listings and 31_114 key grants into one stream of
//!     kind-1059 wraps, so each opener must refuse the other's payload *in situ*.
//!
//! MUTATION PROOFS (P-10) — each names the production edit that must red the row, so no row here
//! is taken on trust:
//!   * KG4 — delete the inner-kind pin in `priv_listing::open_key_grant` (step 4, the
//!     `rumor.kind != Kind::from_u16(KIND_KEY_GRANT)` guard): a 31_113 listing then opens as a grant.
//!   * KG1 — in `priv_listing::seal_wrapped`, add the secret to the OUTER wrap as a tag
//!     (`Tag::custom(TagKind::custom("k"), [hex::encode(secret)])`): the on-the-wire INV-2 scan reds.
//!   * KG2 — in `seal_wrapped`, hoist the ephemeral key out of the per-recipient loop so all N wraps
//!     share one outer author: the F15 unlinkability assertion reds.
//!   * KG1/KG2/KG4/KG5 — seal a zeroed key in `seal_key_grant`: every key-equality assertion reds.
//!
//! HONEST LIMITS, so a later reader does not over-read this suite:
//!   * KG3 asserts a property the RELAY enforces (`#p` routing), not one of ours. It is the PRIV5
//!     analogue and is worth having, but no mutation of Hoardbook code reds it.
//!   * KG1's on-the-wire INV-2 scan is weaker than it looks: the payload is encrypted twice, so a
//!     plaintext key in the *body* would still not appear in the outer event. It catches the leak
//!     shape that has actually bitten (an index/debug tag on the outer wrap), not every leak.

use anyhow::{ensure, Result};
use hb_core::{
    open_key_grant, open_private_listing, seal_key_grant, seal_private_listing, BrowseKey, Identity,
};
use hb_net::publish_private_listing;
use nostr::prelude::*;

use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

const LISTING: &str = r#"{"slug":"vault","content_types":["video"],"entries":[{"name":"rare.mkv"}]}"#;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("KG1 grant round-trips a relay; key intact, no plaintext on the wire", kg1(ctx).await),
        kg2(ctx).await,
        result("KG3 a non-recipient's inbox holds no grant (no enumeration hint)", kg3(ctx).await),
        result("KG4 shared inbox: each opener refuses the other kind's wrap", kg4(ctx).await),
        result("KG5 open_key_grant applies NO allowlist — the caller must", kg5(ctx).await),
    ]
}

/// A deterministic, obviously-synthetic browse key. Distinct per row so a stray event from an
/// earlier run cannot satisfy a later assertion.
fn browse_key(seed: u8) -> BrowseKey {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add(i as u8);
    }
    k
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Publish gift wraps. `publish_private_listing` is kind-agnostic (it publishes the events it is
/// handed), so the grant wraps ride the same production publish path as private listings.
async fn publish_wraps(client: &hb_net::RelayClient, wraps: &[Event]) -> Result<()> {
    publish_private_listing(client, wraps).await?;
    Ok(())
}

/// KG1: the granter seals a browse key to R and publishes. R fetches its own `#p` inbox from the
/// relay and opens the wrap the RELAY returned — the key is byte-identical, the inner author is the
/// verified granter, and the outer wrap author is a fresh ephemeral key (neither party).
///
/// The INV-2 half is the point of doing this at L2: the assertion runs against the JSON the relay
/// stored and served back, not against an in-memory event we just built.
async fn kg1(ctx: &Ctx) -> Result<()> {
    let granter = Identity::generate();
    let r = Identity::generate();
    let key = browse_key(0x11);

    let wraps = seal_key_grant(&granter, &[r.public_key()], &key, now())?;
    ensure!(wraps.len() == 1, "one recipient → one wrap, got {}", wraps.len());

    let gc = ctx.connect(&granter).await?;
    publish_wraps(&gc, &wraps).await?;
    gc.disconnect().await;
    settle().await;

    let rc = ctx.connect(&r).await?;
    let raw = rc.fetch(Filter::new().kind(Kind::GiftWrap).pubkey(r.public_key()), FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(!raw.is_empty(), "the recipient's #p inbox holds the grant wrap");

    // The wrap the relay served back must open to the exact key that was sealed.
    let opened = raw
        .iter()
        .find_map(|e| open_key_grant(&r, e).ok())
        .ok_or_else(|| anyhow::anyhow!("the recipient could not open any wrap in its inbox as a key grant"))?;
    ensure!(opened.browse_key == key, "the browse key must survive the relay round-trip byte-identical");
    ensure!(
        opened.inner_author == granter.public_key(),
        "inner_author is the VERIFIED seal signer (the granter), not the ephemeral outer author"
    );

    // Unlinkability: the outer author is ephemeral — neither the granter nor the recipient.
    ensure!(
        raw.iter().all(|e| e.pubkey != granter.public_key() && e.pubkey != r.public_key()),
        "the outer wrap author must be a fresh ephemeral key (unlinkable to either party)"
    );

    // INV-2 on the wire: nothing the relay stored may contain the browse key in the clear.
    let hex_lower = to_hex(&key);
    let hex_upper = hex_lower.to_uppercase();
    for e in &raw {
        let json = e.as_json();
        ensure!(
            !json.contains(&hex_lower) && !json.contains(&hex_upper),
            "INV-2: the browse key appears IN THE CLEAR in an event the relay stored and served back"
        );
    }
    Ok(())
}

/// KG2 (F14 multi-relay): the N wraps publish to ALL relays, and each recipient can fetch **its own**
/// wrap from **each** relay individually — and never the other recipient's. Gated to multi-relay.
async fn kg2(ctx: &Ctx) -> TestResult {
    let name = "KG2 multi-relay: each recipient's wrap fetchable from every relay, never the other's";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, kg2_inner(ctx).await)
}

async fn kg2_inner(ctx: &Ctx) -> Result<()> {
    let granter = Identity::generate();
    let r = Identity::generate();
    let s = Identity::generate();
    let key = browse_key(0x22);

    let wraps = seal_key_grant(&granter, &[r.public_key(), s.public_key()], &key, now())?;
    ensure!(wraps.len() == 2, "two recipients → two wraps, got {}", wraps.len());
    ensure!(wraps[0].id != wraps[1].id, "the two wraps are distinct events (fresh ephemeral each)");
    ensure!(wraps[0].pubkey != wraps[1].pubkey, "F15: the N wraps are mutually unlinkable (distinct ephemeral authors)");

    let gc = ctx.connect(&granter).await?;
    publish_wraps(&gc, &wraps).await?;
    gc.disconnect().await;
    settle().await;

    for (who, me) in [("R", &r), ("S", &s)] {
        for idx in 0..ctx.relays.len() {
            let c = ctx.connect_one(me, idx).await?;
            let raw = c
                .fetch(Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key()), FETCH_TIMEOUT)
                .await?;
            c.disconnect().await;
            let opened: Vec<_> = raw.iter().filter_map(|e| open_key_grant(me, e).ok()).collect();
            ensure!(
                opened.len() == 1,
                "{who} on relay {idx}: expected exactly its own grant, opened {}",
                opened.len()
            );
            ensure!(opened[0].browse_key == key, "{who} on relay {idx}: granted key mismatch");
            // p-tag routing: the OTHER recipient's wrap is not in this inbox at all.
            let other = if who == "R" { wraps[1].id } else { wraps[0].id };
            ensure!(
                raw.iter().all(|e| e.id != other),
                "{who} on relay {idx}: the other recipient's wrap must not be routed into this inbox"
            );
        }
    }
    Ok(())
}

/// KG3: an outsider who is not a recipient finds nothing. The grant leaves no trace in their inbox —
/// there is no "someone was granted a key" hint to enumerate (the PRIV5 analogue for 31_114).
async fn kg3(ctx: &Ctx) -> Result<()> {
    let granter = Identity::generate();
    let r = Identity::generate();
    let outsider = Identity::generate();

    let wraps = seal_key_grant(&granter, &[r.public_key()], &browse_key(0x33), now())?;
    let gc = ctx.connect(&granter).await?;
    publish_wraps(&gc, &wraps).await?;
    gc.disconnect().await;
    settle().await;

    let oc = ctx.connect(&outsider).await?;
    let raw = oc
        .fetch(Filter::new().kind(Kind::GiftWrap).pubkey(outsider.public_key()), FETCH_TIMEOUT)
        .await?;
    oc.disconnect().await;
    ensure!(raw.is_empty(), "no gift-wrap is addressed to a non-recipient — no enumeration hint");
    Ok(())
}

/// KG4: a real `#p` inbox mixes kinds. Publish BOTH a 31_113 private listing and a 31_114 key grant
/// to the same recipient, fetch the inbox, and prove each opener refuses the other's wrap in situ —
/// the event-confusion guard, observed where the confusion can actually happen.
async fn kg4(ctx: &Ctx) -> Result<()> {
    let author = Identity::generate();
    let r = Identity::generate();
    let key = browse_key(0x44);

    let grant = seal_key_grant(&author, &[r.public_key()], &key, now())?;
    let listing = seal_private_listing(&author, &[r.public_key()], LISTING, now())?;
    let grant_id = grant[0].id;
    let listing_id = listing[0].id;
    ensure!(grant_id != listing_id, "the grant and the listing are distinct events");

    let ac = ctx.connect(&author).await?;
    publish_wraps(&ac, &grant).await?;
    publish_wraps(&ac, &listing).await?;
    ac.disconnect().await;
    settle().await;

    let rc = ctx.connect(&r).await?;
    let raw = rc.fetch(Filter::new().kind(Kind::GiftWrap).pubkey(r.public_key()), FETCH_TIMEOUT).await?;
    rc.disconnect().await;

    let got_grant = raw.iter().find(|e| e.id == grant_id).ok_or_else(|| anyhow::anyhow!("the grant wrap is missing from the inbox"))?;
    let got_listing = raw.iter().find(|e| e.id == listing_id).ok_or_else(|| anyhow::anyhow!("the listing wrap is missing from the inbox"))?;

    // Each opens as its own kind …
    let opened = open_key_grant(&r, got_grant)?;
    ensure!(opened.browse_key == key, "the grant opens to the sealed key");
    let opened_listing = open_private_listing(&r, got_listing)?;
    ensure!(opened_listing.listing_json == LISTING, "the listing opens to its plaintext");

    // … and refuses the other's, even though both decrypt to a well-formed rumor for this recipient.
    ensure!(
        open_key_grant(&r, got_listing).is_err(),
        "a 31_113 private listing must NOT open as a key grant (inner-kind pin)"
    );
    ensure!(
        open_private_listing(&r, got_grant).is_err(),
        "a 31_114 key grant must NOT open as a private listing (inner-kind pin)"
    );
    Ok(())
}

/// KG5: `open_key_grant` deliberately applies **no** allowlist — a stranger can seal a grant into
/// anyone's inbox and it opens, reporting the stranger as `inner_author`. That is the documented
/// contract (the *caller* decides whether the granter may hand it a key), and pinning it here means
/// a future silent filter inside the primitive reds this row instead of quietly changing the model.
async fn kg5(ctx: &Ctx) -> Result<()> {
    let stranger = Identity::generate();
    let r = Identity::generate();
    let key = browse_key(0x55);

    let wraps = seal_key_grant(&stranger, &[r.public_key()], &key, now())?;
    let sc = ctx.connect(&stranger).await?;
    publish_wraps(&sc, &wraps).await?;
    sc.disconnect().await;
    settle().await;

    let rc = ctx.connect(&r).await?;
    let raw = rc.fetch(Filter::new().kind(Kind::GiftWrap).pubkey(r.public_key()), FETCH_TIMEOUT).await?;
    rc.disconnect().await;

    let opened = raw
        .iter()
        .find_map(|e| open_key_grant(&r, e).ok())
        .ok_or_else(|| anyhow::anyhow!("a stranger's grant should still OPEN — the primitive does not filter"))?;
    ensure!(opened.browse_key == key, "the stranger's key is delivered as sealed");
    ensure!(
        opened.inner_author == stranger.public_key(),
        "the caller is handed the granter's identity so IT can apply the allowlist"
    );
    Ok(())
}
