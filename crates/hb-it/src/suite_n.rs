//! Suite N — Nostr events: publish / fetch / replace / delete (TEST_PLAN §4). Every case is a
//! real round-trip against the relay.

use std::time::Duration;

use anyhow::{ensure, Result};
use hb_core::binding::{build_binding, verify_binding, KIND_PRESENCE};
use hb_core::event::{
    build_listing_event, build_teaser, parse_listing_event, parse_teaser, Teaser, KIND_LISTING,
    KIND_TEASER,
};
use hb_core::Identity;
use hb_net::{build_deletion, restitch_listing, split_listing};
use nostr::prelude::*;

use crate::harness::{is_online, now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("N1 teaser round-trip", n1(ctx).await),
        result("N2 encrypted listing", n2(ctx).await),
        result("N3 replaceable semantics", n3(ctx).await),
        result("N4 oversize listing split/restitch", n4(ctx).await),
        result("N5 NIP-09 deletion", n5(ctx).await),
        result("N6 presence freshness", n6(ctx).await),
    ]
}

fn teaser() -> Teaser {
    Teaser {
        display_name: "archivebox_prime".into(),
        bio: "90s anime, VHS rips".into(),
        tags: vec!["anime".into(), "vhs".into()],
        content_types: vec!["video".into()],
    }
}

fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

async fn n1(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let ev = build_teaser(&id, &teaser())?;
    let client = ctx.connect(&id).await?;
    client.publish(&ev).await?;
    settle().await;
    let got = client
        .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;

    ensure!(got.len() == 1, "expected exactly 1 teaser, got {}", got.len());
    // parse_teaser re-verifies the Schnorr signature + schema version.
    ensure!(parse_teaser(&got[0])? == teaser(), "teaser payload mismatch after round-trip");
    let hashtags: Vec<&str> = got[0].tags.hashtags().collect();
    ensure!(
        hashtags.contains(&"anime") && hashtags.contains(&"video"),
        "discovery `t` tags missing: {hashtags:?}"
    );
    ensure!(!got[0].content.contains("contact_hint"), "public teaser leaked contact_hint");
    Ok(())
}

async fn n2(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let json = r#"{"slug":"criterion","entries":[{"name":"Seven Samurai"}]}"#;
    let ev = build_listing_event(&id, "criterion", &bk(1), json)?;
    let client = ctx.connect(&id).await?;
    client.publish(&ev).await?;
    settle().await;
    let got = client
        .fetch(
            Filter::new()
                .author(id.public_key())
                .kind(Kind::from_u16(KIND_LISTING))
                .identifier("criterion"),
            FETCH_TIMEOUT,
        )
        .await?;
    client.disconnect().await;

    ensure!(got.len() == 1, "expected 1 listing, got {}", got.len());
    // Holder of the browse-key decrypts.
    let (slug, decrypted) = parse_listing_event(&got[0], &bk(1))?;
    ensure!(slug == "criterion" && decrypted == json, "holder decrypt mismatch");
    // Non-holder fails cleanly (sees only ciphertext).
    ensure!(parse_listing_event(&got[0], &bk(99)).is_err(), "a non-holder decrypted the listing");
    Ok(())
}

async fn n3(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let mut v1 = teaser();
    v1.display_name = "v1".into();
    let mut v2 = teaser();
    v2.display_name = "v2".into();

    let client = ctx.connect(&id).await?;
    client.publish(&build_teaser(&id, &v1)?).await?;
    // The teaser is a replaceable event keyed on created_at; a 1s gap makes v2 strictly newer.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    client.publish(&build_teaser(&id, &v2)?).await?;
    settle().await;
    let got = client
        .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;

    ensure!(got.len() == 1, "replaceable kind kept {} events, want exactly 1", got.len());
    ensure!(parse_teaser(&got[0])?.display_name == "v2", "old teaser survived the replace");
    Ok(())
}

async fn n4(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let key = bk(4);
    let slug = "huge";
    // A tree whose single encrypted event (~76 KiB) exceeds strfry's 64 KiB cap, but whose
    // ~57 KiB plaintext still fits NIP-44's 65408-byte limit — so the *relay* cap is what bites.
    let big = big_listing(950);

    let client = ctx.connect(&id).await?;
    // The unsplit listing can't be published as one event (relay cap; NIP-44 also caps
    // plaintext) — this is *why* we split. Robust to whichever cap binds.
    let single_publishable = match build_listing_event(&id, slug, &key, &big) {
        Ok(ev) => client.publish(&ev).await.is_ok(),
        Err(_) => false,
    };
    ensure!(
        !single_publishable,
        "an oversize listing was publishable as a single event — the N4 size cap is not in force"
    );

    // Split under a plaintext budget that keeps each encrypted part comfortably under the cap.
    let parts = split_listing(slug, &big, 40_000)?;
    ensure!(parts.len() > 1, "expected an oversize listing to split, got {} part(s)", parts.len());
    for p in &parts {
        client.publish(&build_listing_event(&id, &p.d_tag, &key, &p.json)?).await?;
    }
    settle().await;

    let got = client
        .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_LISTING)), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;
    ensure!(got.len() == parts.len(), "fetched {} parts, published {}", got.len(), parts.len());

    // Decrypt each part (verifies the listing event) and collect the payloads to restitch.
    let mut fetched: Vec<String> = Vec::new();
    for ev in &got {
        fetched.push(parse_listing_event(ev, &key)?.1);
    }
    let restitched = restitch_listing(&fetched)?;
    ensure!(restitched == norm(&big)?, "restitched tree does not match the original");
    Ok(())
}

async fn n5(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let ev = build_teaser(&id, &teaser())?;
    let client = ctx.connect(&id).await?;
    client.publish(&ev).await?;
    settle().await;

    // Build + publish a well-formed NIP-09 deletion. The hard invariant is that the *request* is
    // well-formed and accepted; honouring it is best-effort (a relay may ignore it).
    let del = build_deletion(&id, &ev)?;
    ensure!(del.kind == Kind::EventDeletion, "deletion has wrong kind {}", del.kind.as_u16());
    let outcome = client.publish(&del).await?;
    ensure!(!outcome.accepted.is_empty(), "relay rejected the deletion request: {:?}", outcome.rejected);
    settle().await;

    let after = client
        .fetch(
            Filter::new()
                .author(id.public_key())
                .kind(Kind::from_u16(KIND_TEASER))
                .identifier("hoardbook-teaser"),
            FETCH_TIMEOUT,
        )
        .await?;
    client.disconnect().await;
    // Best-effort observation (not a pass/fail criterion): strfry honours NIP-09.
    eprintln!("   N5: relay honoured the deletion (event gone): {}", after.is_empty());
    Ok(())
}

async fn n6(ctx: &Ctx) -> Result<()> {
    let now = now();

    // A freshly-refreshed presence reads online and carries a verifiable npub freshness binding
    // (v0.9.6: presence is status-only — no node key, no address; transport lives in Mascara).
    let id = Identity::generate();
    let fresh = build_binding(&id, now, 30 * 60)?;
    let client = ctx.connect(&id).await?;
    client.publish(&fresh).await?;
    settle().await;
    let got = client
        .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_PRESENCE)), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;
    ensure!(got.len() == 1, "expected 1 presence event, got {}", got.len());
    verify_binding(&got[0], &id.public_key(), now)?; // signature + author-pin + window verify
    ensure!(is_online(got[0].created_at.as_u64(), now), "fresh presence read as offline");

    // A 15-minute-old presence (different peer, so it doesn't replace the fresh one) reads
    // offline, while its binding still verifies.
    let id2 = Identity::generate();
    let stale = build_binding(&id2, now - 15 * 60, 30 * 60)?;
    let c2 = ctx.connect(&id2).await?;
    c2.publish(&stale).await?;
    settle().await;
    let got2 = c2
        .fetch(Filter::new().author(id2.public_key()).kind(Kind::from_u16(KIND_PRESENCE)), FETCH_TIMEOUT)
        .await?;
    c2.disconnect().await;
    ensure!(got2.len() == 1, "expected 1 stale presence, got {}", got2.len());
    ensure!(!is_online(got2[0].created_at.as_u64(), now), "15-min-old presence read as online");
    verify_binding(&got2[0], &id2.public_key(), now)?; // binding still valid, just stale
    Ok(())
}

/// A listing payload with `n` padded folder entries — at n=950 the plaintext is ≈54 KiB (within
/// NIP-44's 65408-byte cap) but its encrypted event is ≈72 KiB, over strfry's 64 KiB event cap.
fn big_listing(n: usize) -> String {
    let entries: Vec<serde_json::Value> = (0..n)
        .map(|i| serde_json::json!({ "name": format!("folder-{i:05}-padding-padding-xx"), "size": 1_000_000 + i }))
        .collect();
    serde_json::json!({ "slug": "huge", "content_types": ["video"], "entries": entries }).to_string()
}

/// Canonical (sorted-key) form, so split→restitch can be compared byte-for-byte.
fn norm(s: &str) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::from_str::<serde_json::Value>(s)?)?)
}
