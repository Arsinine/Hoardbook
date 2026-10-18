//! Suite ID — identity & signing (TEST_PLAN §4), ported to secp256k1/Schnorr.

use anyhow::{ensure, Result};
use hb_core::binding::{build_binding, verify_binding};
use hb_core::event::{build_teaser, parse_teaser, Teaser, KIND_TEASER};
use hb_core::identity::{parse_npub, verify_event};
use hb_core::{HbError, Identity, ShareCode};
use nostr::prelude::*;

use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("ID1 tampered event rejected", id1(ctx).await),
        result("ID2 mangled npub/share-code rejected by codec", id2().await),
        result("ID3 forged binding rejected", id3(ctx).await),
        id4(ctx).await,
        result("ID5 imported ncryptsec key signs as its own npub on a relay", id5(ctx).await),
    ]
}

/// The NIP-49 spec vector: password "nostr" decrypts to `NIP49_SECRET_HEX`.
const NIP49_VECTOR: &str = "ncryptsec1qgg9947rlpvqu76pj5ecreduf9jxhselq2nae2kghhvd5g7dgjtcxfqtd67p9m0w57lspw8gsq6yphnm8623nsl8xn9j4jdzz84zm3frztj3z7s35vpzmqf6ksu8r89qk5z2zxfmu5gv8th8wclt0h4p";
const NIP49_SECRET_HEX: &str = "3501454135014541350145413501453fefb02227e449e57cf4d3a3ce05378683";

fn teaser() -> Teaser {
    Teaser {
        collections: Vec::new(),
        total_bytes: 0,
        contact_hint: None,
        looking_for: None,
        display_name: "signer".into(),
        bio: String::new(),
        tags: vec!["hbid".into()],
        content_types: vec!["video".into()],
        picture: None, hide_in_rosters: false,
    }
}

async fn id1(ctx: &Ctx) -> Result<()> {
    let id = Identity::generate();
    let ev = build_teaser(&id, &teaser(), true)?;
    let client = ctx.connect(&id).await?;
    client.publish(&ev).await?;
    settle().await;
    let got = client
        .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;
    ensure!(got.len() == 1, "expected the teaser back");

    // A relay-returned event mutated after signing fails verification (id no longer matches).
    let mut tampered = got[0].clone();
    tampered.content = "tampered after signing".into();
    ensure!(verify_event(&tampered).is_err(), "tampered event passed verification");
    ensure!(parse_teaser(&tampered).is_err(), "tampered teaser parsed");
    // The untouched event still verifies (control).
    verify_event(&got[0])?;
    Ok(())
}

async fn id2() -> Result<()> {
    // No network: the codec rejects a mangled npub / share-code before any use.
    let id = Identity::generate();

    let mut npub = id.npub();
    let last = npub.pop().unwrap();
    npub.push(if last == 'q' { 'p' } else { 'q' });
    ensure!(parse_npub(&npub).is_err(), "mangled npub accepted");

    let code = ShareCode::Full { pubkey: id.public_key(), browse_key: [9u8; 32] }.encode()?;
    let mut bad = code.clone();
    let last = bad.pop().unwrap();
    bad.push(if last == 'q' { 'p' } else { 'q' });
    ensure!(ShareCode::parse(&bad).is_err(), "mangled share-code accepted");

    // Outright garbage never panics, always Err.
    for s in ["", "npub1", "hbk1", "not a code", "::::"] {
        ensure!(ShareCode::parse(s).is_err(), "{s:?} parsed as a share code");
    }
    Ok(())
}

/// A key imported from a password-protected `ncryptsec` (how most Nostr clients export one) is the
/// SAME identity on the wire: an event it signs round-trips through a real relay, verifies, and is
/// found by an author filter on the vector's own pubkey. The relay is fed a fresh event per run,
/// so the author filter is narrowed to it by content to stay independent of earlier runs.
async fn id5(ctx: &Ctx) -> Result<()> {
    let imported = Identity::from_encrypted_secret(NIP49_VECTOR, "nostr")?;
    let expected = Identity::from_secret(NIP49_SECRET_HEX)?.public_key();
    ensure!(imported.public_key() == expected, "ncryptsec decrypted to the wrong key");

    let marker = format!("id5-{}", now());
    let mut t = teaser();
    t.bio = marker.clone();
    let ev = build_teaser(&imported, &t, true)?;
    let client = ctx.connect(&imported).await?;
    client.publish(&ev).await?;
    settle().await;
    let got = client
        .fetch(Filter::new().author(expected).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;

    let mine: Vec<_> = got.iter().filter(|e| e.id == ev.id).collect();
    ensure!(mine.len() == 1, "the imported key's event did not come back under its npub");
    verify_event(mine[0])?;
    ensure!(parse_teaser(mine[0])?.bio == marker, "teaser content changed in transit");
    Ok(())
}

async fn id3(ctx: &Ctx) -> Result<()> {
    let now = now();
    let a = Identity::generate();
    let b = Identity::generate();
    let presence = build_binding(&a, now, 30 * 60)?;

    let client = ctx.connect(&a).await?;
    client.publish(&presence).await?;
    settle().await;
    let got = client
        .fetch(
            Filter::new()
                .author(a.public_key())
                .kind(Kind::from_u16(hb_core::binding::KIND_PRESENCE)),
            FETCH_TIMEOUT,
        )
        .await?;
    client.disconnect().await;
    ensure!(got.len() == 1, "expected the presence event back");

    // A relay presenting A's (valid) binding as if it vouches for B is refused — H2 wrong-signer.
    ensure!(
        matches!(verify_binding(&got[0], &b.public_key(), now), Err(HbError::WrongSigner)),
        "a binding authored by A was accepted as B's"
    );
    // A binding mutated after signing fails on the canonical id.
    let mut forged = got[0].clone();
    forged.content = "forged".into();
    ensure!(verify_binding(&forged, &a.public_key(), now).is_err(), "tampered binding accepted");
    // The genuine binding still verifies for A (control).
    verify_binding(&got[0], &a.public_key(), now)?;
    Ok(())
}

async fn id4(ctx: &Ctx) -> TestResult {
    let name = "ID4 cross-relay key compat";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, id4_inner(ctx).await)
}

async fn id4_inner(ctx: &Ctx) -> Result<()> {
    let a = Identity::generate();
    let ev = build_teaser(&a, &teaser(), true)?;

    // Multi-publish lands the *same* signed event on both relays.
    let pubc = ctx.connect(&a).await?;
    pubc.publish(&ev).await?;
    pubc.disconnect().await;
    settle().await;

    // Fetch the event independently from each host; the Schnorr/NIP-01 id+sig are identical and
    // both verify — signing is host-agnostic.
    let c0 = ctx.connect_one(&a, 0).await?;
    let g0 = c0.fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT).await?;
    c0.disconnect().await;
    let c1 = ctx.connect_one(&a, 1).await?;
    let g1 = c1.fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT).await?;
    c1.disconnect().await;

    ensure!(g0.len() == 1 && g1.len() == 1, "teaser missing on a relay ({}, {})", g0.len(), g1.len());
    ensure!(g0[0].id == g1[0].id, "event id differs across relays");
    ensure!(g0[0].sig == g1[0].sig, "signature differs across relays");
    parse_teaser(&g0[0])?;
    parse_teaser(&g1[0])?;
    Ok(())
}
