//! Suite V5 — the v5 size-gated read rule (QURATOR-344/345) and the compressed carriers, over a
//! live relay (hb-it L2).
//!
//! Rows:
//!   * V1–V4 — the auto-grant path: an asker's teaser `total_bytes` drives the size rule, and a
//!     pass mints a key grant the asker receives and decrypts with; a fail means NO grant exists
//!     and the listing does not open. R1 bigger asker granted + decrypts · R2 smaller asker: no
//!     grant, teaser only · R3 tie grants · R4 absent asker teaser ⇒ no grant (and the
//!     zero-reads-zero-author edge through the same live path).
//!   * V5 — a compressed listing survives publish → split → relay → restitch → render byte-exact,
//!     plus a manifest-envelope round-trip of the same parts (`manifest_v == MANIFEST_V`).
//!   * V6 — a crafted high-ratio payload is REFUSED at the decompression cap, over the wire, with
//!     the refusal NAMING the cap (never a bare "browse failed").
//!
//! HONEST GAP (CLAUDE.md §5 step 2) — state it, don't bury it: hb-it cannot link hb-app (the
//! Tauri crate), so V1–V4 RE-COMPOSE the answerer's decision from the same reachable production
//! parts the shipped `auto_approve` loop calls — [`hb_net::fetch_peer_teaser`] +
//! [`hb_core::size_rule::may_read`] (absent ⇒ 0) — they are NOT the production loop itself. The
//! drift pin at the bottom of this file (`production_grant_decision_still_uses_these_parts`)
//! `include_str!`s `crates/hb-app/src/auto_approve.rs` and asserts the `access_grant_decision`
//! body still routes through `may_read(` and that the loop still calls `fetch_peer_teaser(` —
//! if production stops using these parts, this suite reds instead of silently describing
//! something else. (The same include_str! pin pattern as `harness::window_pin` and
//! `suite_cap::ensure_budget_matches_hb_app`.)
//!
//! MUTATION PROOFS (P-10) — each row names the production edit that must red it:
//!   * V1 — seal a ZEROED key in `hb_core::seal_key_grant`: the granted key no longer decrypts
//!     the owner's listing, and V1's byte-exact decrypt assertion reds.
//!   * V2 — in `hb_core::size_rule::may_read`, replace `reader_total >= author_total` with
//!     `true`: the smaller asker is then granted, and V2's no-grant/teaser-only assertions red.
//!   * V3 — in `hb_core::size_rule::may_read`, change `>=` to `>`: a tie stops granting, and
//!     V3 reds.
//!   * V4 — in `hb_core::size_rule::may_read`, delete `reader_total > 0 &&`: an absent (⇒ 0)
//!     asker is then "granted" against a zero-size owner, and V4 reds.
//!   * V5 — in `hb_net::publish_listing`, skip publishing the FINAL part event: the family
//!     browses back incomplete and the entry-for-entry equality reds.
//!   * V6 — raise the cap comparison in `hb_core::listing::decompress_body`
//!     (`out.len() as u64 > MAX_DECOMPRESSED_LISTING_BYTES`) to `u64::MAX`: the bomb decompresses
//!     and V6's names-the-cap refusal assertion reds.

use anyhow::{ensure, Result};
use hb_core::event::{
    build_listing_event, build_teaser, parse_listing_event, Teaser, KIND_LISTING,
};
use hb_core::{
    build_manifest_envelope, seal_key_grant, Identity, ShareCode, MANIFEST_V,
};
use hb_net::{
    fetch_full_listing_from, fetch_key_grants, fetch_peer_teaser, publish_listing,
    publish_private_listing, split_listing, RenderedListing,
};
use nostr::prelude::*;
use serde_json::{json, Value};

use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::suite_cap::LISTING_MAX_BYTES;
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("V1 bigger asker is granted and decrypts the owner's listing", v1(ctx).await),
        result("V2 smaller asker gets no grant and sees only the teaser", v2(ctx).await),
        result("V3 equal sizes grant (the tie reads)", v3(ctx).await),
        result("V4 absent asker teaser (and a zero owner) grants nothing", v4(ctx).await),
        result("V5 compressed listing round-trip + manifest round-trip, byte-exact", v5(ctx).await),
        result(
            "V6 a crafted high-ratio payload is refused at the decompression cap (refusal names it)",
            v6(ctx).await,
        ),
    ]
}

/// A deterministic, obviously-synthetic browse key. Distinct per row so a stray event from an
/// earlier run cannot satisfy a later assertion (the `suite_keygrant::browse_key` pattern).
fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// Build + publish a peer's profile teaser with a GIVEN `total_bytes` — the v5 size-rule input,
/// production-shaped (`hb_core::event::build_teaser`, the same fn `commands::profile` drives).
fn teaser_event(id: &Identity, name: &str, total: u64) -> Result<Event> {
    let t = Teaser {
        display_name: name.to_string(),
        bio: String::new(),
        tags: vec![],
        content_types: vec![],
        picture: None,
        hide_in_rosters: false,
        total_bytes: total,
        contact_hint: None,
    };
    Ok(build_teaser(id, &t, false)?)
}

/// A small, production-shaped public listing (the same tree shape `collection_to_listing_json`
/// emits: name / item_type / tags / children), so the decrypt+render path sees a real envelope.
fn listing_json(slug: &str, n: usize) -> String {
    json!({
        "slug": slug,
        "description": "suite v5 fixture",
        "item_count": n,
        "content_types": ["video"],
        "snapshot_fingerprint": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "entries": (0..n).map(|i| json!({
            "name": format!("title-{i:03}.mkv"),
            // P-10 sibling of the "compression makes size fixtures vacuous" lesson: the split
            // budget is compression-aware, so a repetitive fixture zstd-flattens to ONE part and
            // proves nothing about the carrier. `pad` is a deterministic xorshift stream — the
            // fixture stays reproducible (byte-exact round-trip is meaningful) while being
            // incompressible, so the SPLIT, not the compressor, decides the part count.
            "pad": pseudo_hex(i as u64 ^ 0x9e37_79b9_7f4a_7c15, 96),
            "item_type": "File", "tags": [], "children": [],
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// Deterministic incompressible hex — a seeded xorshift64* stream, so the fixture is byte-identical
/// across runs (the round-trip assertions stay exact) yet zstd cannot shrink it.
fn pseudo_hex(mut seed: u64, len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.push_str(&format!("{seed:016x}"));
    }
    out.truncate(len);
    out
}

/// Publish the owner side (teaser with `total` + a small public listing under `key`) and return
/// the listing's entries as published, so every row can compare the browsed-back tree against
/// the exact bytes that went up.
async fn publish_owner(
    ctx: &Ctx,
    owner: &Identity,
    slug: &str,
    key: &[u8; 32],
    total: u64,
) -> Result<Value> {
    let listing = listing_json(slug, 6);
    let entries = serde_json::from_str::<Value>(&listing)?["entries"].clone();

    let client = ctx.connect(owner).await?;
    client.publish(&teaser_event(owner, "v5-owner", total)?).await?;
    client.publish(&build_listing_event(owner, slug, key, &listing)?).await?;
    client.disconnect().await;
    settle().await;
    Ok(entries)
}

/// THE ANSWERER HALF, re-composed from the reachable production parts (see the header's HONEST
/// GAP note): fetch the asker's CURRENT teaser, absent ⇒ 0, then decide with the ONE rule fn.
/// This is exactly the shape `auto_approve.rs`'s access-request step runs — which is why the
/// drift pin at the bottom of this file pins that production body to these two calls.
async fn grant_decision(ctx: &Ctx, asker: &PublicKey, owner_total: u64) -> Result<bool> {
    // Connected with a throwaway identity: the fetch is author-pinned at the filter, the client
    // identity carries no decision weight.
    let c = ctx.connect(&Identity::generate()).await?;
    let asker_teaser = fetch_peer_teaser(&c, asker, FETCH_TIMEOUT).await?;
    c.disconnect().await;
    let asker_total = asker_teaser.map(|t| t.total_bytes).unwrap_or(0);
    Ok(hb_core::size_rule::may_read(asker_total, owner_total))
}

/// The asker's receive half: fetch the `#p` inbox against an allowlist of exactly `owner`, and
/// return the granted keys with their verified inner authors.
async fn asker_inbox(
    ctx: &Ctx,
    asker: &Identity,
    owner_pk: &PublicKey,
) -> Result<Vec<hb_core::OpenedKeyGrant>> {
    let c = ctx.connect(asker).await?;
    let grants = fetch_key_grants(&c, asker, std::slice::from_ref(owner_pk), FETCH_TIMEOUT).await?;
    c.disconnect().await;
    Ok(grants)
}

/// The positive half shared by V1/V3: the grant is minted, published, RECEIVED by the asker with
/// the owner as verified inner author, and the granted key decrypts the owner's published listing
/// back to the exact entries that were published.
async fn granted_and_decrypts(
    ctx: &Ctx,
    owner: &Identity,
    asker: &Identity,
    slug: &str,
    key: &[u8; 32],
    published_entries: &Value,
) -> Result<()> {
    let wraps = seal_key_grant(owner, &[asker.public_key()], key, now())?;
    let oc = ctx.connect(owner).await?;
    publish_private_listing(&oc, &wraps).await?;
    oc.disconnect().await;
    settle().await;

    let grants = asker_inbox(ctx, asker, &owner.public_key()).await?;
    ensure!(
        !grants.is_empty(),
        "the owner's grant is missing from the asker's inbox — the v5 grant did not arrive"
    );
    ensure!(
        grants.iter().all(|g| g.inner_author == owner.public_key()),
        "every grant in the asker's inbox must name the OWNER as the verified inner author"
    );
    ensure!(
        grants.iter().all(|g| g.browse_key == *key),
        "the granted key is not the key that was sealed"
    );

    // The point of the grant: the asker can now decrypt the owner's published listing.
    let ac = ctx.connect(asker).await?;
    let rendered =
        fetch_full_listing_from(&ac, &owner.public_key(), slug, key, &ctx.relays, FETCH_TIMEOUT)
            .await?;
    ac.disconnect().await;
    ensure!(
        rendered.complete(),
        "the owner's listing browsed back incomplete under the granted key"
    );
    ensure_entries_match(&rendered, published_entries)?;
    Ok(())
}

/// The negative half shared by V2/V4: NO grant exists in the asker's inbox, the asker still sees
/// the owner's public TEASER (that is all v5 promises them), and the listing does not open
/// without the key — with the wrong key the decrypt fails rather than yielding the tree.
async fn refused_and_locked(
    ctx: &Ctx,
    owner: &Identity,
    asker: &Identity,
    slug: &str,
    _key: &[u8; 32],
    owner_total: u64,
) -> Result<()> {
    let grants = asker_inbox(ctx, asker, &owner.public_key()).await?;
    ensure!(
        grants.is_empty(),
        "a REFUSED asker found {} grant(s) in its inbox — the size rule failed to gate the grant",
        grants.len()
    );

    // "Everyone else you get a teaser": the teaser is still fully readable.
    let ac = ctx.connect(asker).await?;
    let teaser = fetch_peer_teaser(&ac, &owner.public_key(), FETCH_TIMEOUT).await?;
    ensure!(
        teaser.map(|t| t.total_bytes) == Some(owner_total),
        "the owner's teaser (total_bytes={}) must stay readable to a refused asker",
        owner_total
    );

    // … and without a granted key the listing does not open (a wrong key decrypts to nothing).
    let wrong = bk(0xEE);
    let res =
        fetch_full_listing_from(&ac, &owner.public_key(), slug, &wrong, &ctx.relays, FETCH_TIMEOUT)
            .await;
    ac.disconnect().await;
    ensure!(
        res.is_err(),
        "the owner's listing OPENED under a wrong key — the read gate is not the encryption"
    );
    Ok(())
}

/// Entry-for-entry equality between the browsed-back tree and the bytes that were published —
/// the "byte-exact" half of V5. serde Value equality compares every name/child byte.
fn ensure_entries_match(rendered: &RenderedListing, published_entries: &Value) -> Result<()> {
    let got = serde_json::to_value(&rendered.entries)?;
    ensure!(
        &got == published_entries,
        "the browsed-back tree does not match the published tree byte-for-byte"
    );
    Ok(())
}

/// V1 (R1): a BIGGER asker is granted and decrypts.
async fn v1(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let asker = Identity::generate();
    let slug = ctx.tag("v5r1");
    let key = bk(0xA1);
    let published = publish_owner(ctx, &owner, &slug, &key, 1_000_000).await?;

    // The asker publishes a teaser with a BIGGER total.
    let ac = ctx.connect(&asker).await?;
    ac.publish(&teaser_event(&asker, "v5-asker-big", 2_000_000)?).await?;
    ac.disconnect().await;
    settle().await;

    ensure!(
        grant_decision(ctx, &asker.public_key(), 1_000_000).await?,
        "a bigger asker must be granted by the v5 size rule"
    );
    granted_and_decrypts(ctx, &owner, &asker, &slug, &key, &published).await
}

/// V2 (R2): a SMALLER asker gets no grant and sees only the teaser.
async fn v2(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let asker = Identity::generate();
    let slug = ctx.tag("v5r2");
    let key = bk(0xA2);
    let _ = publish_owner(ctx, &owner, &slug, &key, 2_000_000).await?;

    let ac = ctx.connect(&asker).await?;
    ac.publish(&teaser_event(&asker, "v5-asker-small", 1_000)?).await?;
    ac.disconnect().await;
    settle().await;

    ensure!(
        !grant_decision(ctx, &asker.public_key(), 2_000_000).await?,
        "a smaller asker must NOT be granted by the v5 size rule"
    );
    refused_and_locked(ctx, &owner, &asker, &slug, &key, 2_000_000).await
}

/// V3 (R3): equal sizes grant — the tie reads, end to end over a live relay.
async fn v3(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let asker = Identity::generate();
    let slug = ctx.tag("v5r3");
    let key = bk(0xA3);
    let published = publish_owner(ctx, &owner, &slug, &key, 1_500_000).await?;

    let ac = ctx.connect(&asker).await?;
    ac.publish(&teaser_event(&asker, "v5-asker-tie", 1_500_000)?).await?;
    ac.disconnect().await;
    settle().await;

    ensure!(
        grant_decision(ctx, &asker.public_key(), 1_500_000).await?,
        "a TIE must read — the v5 rule is reader ≥ author"
    );
    granted_and_decrypts(ctx, &owner, &asker, &slug, &key, &published).await
}

/// V4 (R4): an asker who never published a teaser has NO `total_bytes` ⇒ fetch returns `None` ⇒
/// the rule sees 0 and grants nothing. The owner here is deliberately ZERO-size too, so the row
/// also pins the "zero reads nothing, not even another zero" edge through the same live path
/// (the third `may_read` unit case, taken end-to-end).
async fn v4(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let asker = Identity::generate();
    let slug = ctx.tag("v5r4");
    let key = bk(0xA4);
    let _ = publish_owner(ctx, &owner, &slug, &key, 0).await?;

    // The asker publishes NOTHING — no teaser ever existed for this fresh identity.
    ensure!(
        !grant_decision(ctx, &asker.public_key(), 0).await?,
        "an absent teaser (⇒ total 0) must grant nothing — not even against a zero-size owner"
    );
    refused_and_locked(ctx, &owner, &asker, &slug, &key, 0).await
}

/// V5 (R5): a compressed listing survives the full carrier chain — publish → split → relay →
/// restitch → render — entry-for-entry byte-exact, and the SAME split parts round-trip through a
/// signed manifest envelope (`manifest_v == MANIFEST_V`, restored parts byte-exact).
async fn v5(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let slug = ctx.tag("v5r5");
    let key = bk(0xA5);
    crate::suite_cap::ensure_budget_matches_hb_app()?;

    // Large enough to split comfortably at the production 40 KB budget (≈120 KB of tree).
    let listing = listing_json(&slug, 1200);
    let published_entries = serde_json::from_str::<Value>(&listing)?["entries"].clone();

    // Publish through the production path (which splits + seals per part).
    let client = ctx.connect(&owner).await?;
    let published = publish_listing(&client, &owner, &slug, &key, &listing, LISTING_MAX_BYTES).await?;
    ensure!(
        published.parts >= 2,
        "the fixture must have SPLIT ({} part(s)) — a single-event fixture proves nothing about the carrier",
        published.parts
    );
    client.disconnect().await;
    settle().await;

    // The relay holds exactly the events the split produced: one distinct `d` per part.
    let fc = ctx.connect(&Identity::generate()).await?;
    let events = fc
        .fetch(Filter::new().author(owner.public_key()).kind(Kind::from_u16(KIND_LISTING)), FETCH_TIMEOUT)
        .await?;
    let mut ds: Vec<String> = events
        .iter()
        .filter_map(|e| e.tags.identifier().map(|d| d.to_string()))
        .filter(|d| d == &slug || d.starts_with(&format!("{slug}#part")))
        .collect();
    ds.sort();
    ds.dedup();
    ensure!(
        ds.len() == published.parts,
        "the relay holds {} distinct part d-tag(s) but the split produced {}",
        ds.len(),
        published.parts
    );

    // Browse it back through the production browse path and compare byte-for-byte.
    let code = ShareCode::Full { pubkey: owner.public_key(), browse_key: key };
    let res = hb_net::browse_share_code(&fc, &code, &slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT)
        .await?;
    fc.disconnect().await;
    let rendered =
        res.listing.ok_or_else(|| anyhow::anyhow!("the compressed listing did not come back"))?;
    ensure!(rendered.complete(), "the split listing browsed back incomplete");
    ensure_entries_match(&rendered, &published_entries)?;

    // Manifest round-trip of the SAME parts: seal + sign via the production envelope builder,
    // then open as a peer would (verify author, decrypt) and compare the restored parts —
    // restitched, byte-exact — to the original split output.
    let parts: Vec<String> =
        split_listing(&slug, &listing, LISTING_MAX_BYTES)?.into_iter().map(|p| p.json).collect();
    let fp = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let envelope = build_manifest_envelope(&owner, &slug, &key, fp, now(), &parts)?;
    ensure!(
        envelope.manifest_v == MANIFEST_V,
        "the envelope must carry the current manifest discriminant ({MANIFEST_V}), got {}",
        envelope.manifest_v
    );
    let restored = envelope.open(&key, &owner.public_key())?;
    ensure!(
        restored == parts,
        "the manifest round-trip did not restore the split parts byte-exactly"
    );
    let want = hb_net::restitch_listing(&parts)?;
    let got = hb_net::restitch_listing(&restored)?;
    ensure!(
        want == got,
        "the manifest-restored listing restitches to different bytes than the published split"
    );
    Ok(())
}

/// V6 (R6): a crafted high-ratio payload is refused at the decompression cap OVER THE WIRE.
///
/// The payload is a 68 MB single-run body — a ~100,000:1 compression ratio, far past the
/// 64 MiB [`hb_core::MAX_DECOMPRESSED_LISTING_BYTES`] ceiling. It is sealed by the production
/// `encrypt_listing` (inside [`build_listing_event`]), exactly as every published listing is —
/// the hostile shape is the PLAINTEXT ratio, not a bypassed seal.
///
/// WHY NOT THE UNIT TEST'S RLE BOMB: that fixture (`hb_core::listing::tests::crafted_rle_bomb`)
/// is hand-built so it can be sealed by the test-local `seal_inner`, which needs
/// `hb_core::listing::conversation_key` — a PRIVATE fn, and `listing.rs` is fenced to a sibling
/// lane. Re-deriving the conversation key here would be a second crypto implementation in a
/// harness, and copying the bomb frame alone cannot be sealed. This row reaches the same cap
/// with the production seal on a real high-ratio body instead — every byte the decrypt path
/// processes was produced by production code.
async fn v6(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let slug = ctx.tag("v5r6");
    let key = bk(0xA6);

    // 68 MB of one repeated byte: a legit-shaped JSON string whose zstd stream is a few hundred
    // bytes, so it fits one kind-31111 event but decompresses ~1 MB PAST the 64 MiB cap.
    let bomb = json!({
        "slug": slug,
        "description": "x".repeat(68 * 1024 * 1024),
        "entries": [],
    })
    .to_string();

    let client = ctx.connect(&owner).await?;
    let event = build_listing_event(&owner, &slug, &key, &bomb)?;
    ensure!(
        event.content.len() < 65_000,
        "the sealed bomb must fit one relay event ({} bytes) — the ratio, not the size, is the weapon",
        event.content.len()
    );
    client.publish(&event).await?;
    client.disconnect().await;
    settle().await;

    // Fetch it via the production browse path: decrypt must SUCCEED (the seal is ours) and the
    // decompression cap must then refuse it, NAMING the cap.
    let browser = ctx.connect(&Identity::generate()).await?;
    let res =
        fetch_full_listing_from(&browser, &owner.public_key(), &slug, &key, &ctx.relays, FETCH_TIMEOUT)
            .await;
    browser.disconnect().await;
    let err = res.err().ok_or_else(|| {
        anyhow::anyhow!("the 68 MB high-ratio body was DECODED — the decompression cap never fired")
    })?;
    let msg = format!("{err:#}");
    ensure!(
        msg.contains("decompression cap"),
        "the refusal must NAME the decompression cap, not just fail; got: {msg}"
    );

    // And the same body, offered directly to the L1 parser primitive (verify + tags + decrypt —
    // the exact fn the browse path calls per event), refuses with the cap too: the L2 refusal is
    // the primitive's refusal, not a harness special case.
    let l1 = parse_listing_event(&event, &key);
    let l1_msg = format!("{}", l1.err().ok_or_else(|| {
        anyhow::anyhow!("decrypt_listing accepted the bomb body")
    })?);
    ensure!(
        l1_msg.contains("decompression cap"),
        "the L1 primitive must name the decompression cap too; got: {l1_msg}"
    );
    Ok(())
}

/// The drift pin (see the header): production's grant decision must still route through the
/// parts these rows re-compose, or this suite describes a codebase that no longer exists.
#[cfg(test)]
mod v5_drift_pin {
    /// P-10 mutation: delete the `hb_core::size_rule::may_read(asker_total, my_total)` call from
    /// `access_grant_decision` in `crates/hb-app/src/auto_approve.rs` — this pin reds. (Deleting
    /// the call is the honest mutation: the fn's own doc comment does not quote the call form,
    /// so nothing left in the slice can satisfy the count.)
    #[test]
    fn production_grant_decision_still_uses_these_parts() {
        crate::suite_cap::ensure_budget_matches_hb_app().expect("the restated publish budget matches hb-app");
        const SRC: &str = include_str!("../../hb-app/src/auto_approve.rs");

        // The decision body, sliced between its fn and the next fn, so the doc comments above it
        // (and the P-10 comment quoting the call near the tests, ~line 1848) cannot satisfy the
        // count — only the LIVE body can.
        let start = SRC.find("fn access_grant_decision(").expect("auto_approve.rs still declares access_grant_decision");
        let rest = &SRC[start..];
        let end = rest.find("fn access_dedup_key(").expect("auto_approve.rs still declares access_dedup_key after the decision");
        let body = &rest[..end];
        assert!(
            body.contains("may_read("),
            "auto_approve's access_grant_decision no longer calls hb_core::size_rule::may_read — \
             Suite V5's re-composed decision (and its drift claim) is stale"
        );

        // The answerer half must still fetch the asker's CURRENT teaser through the same
        // production fetch these rows call (the use-statement form has no paren, so only a real
        // call site satisfies this).
        assert!(
            SRC.matches("fetch_peer_teaser(").count() >= 1,
            "auto_approve.rs no longer calls fetch_peer_teaser — the answerer stopped reading the \
             live teaser Suite V5 drives"
        );
    }
}
