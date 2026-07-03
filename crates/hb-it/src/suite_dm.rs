//! Suite DM — NIP-17 gift-wrapped direct messages (TEST_PLAN §4). Replaces the old Suite B.

use anyhow::{ensure, Result};
use hb_core::Identity;
use hb_net::{build_relay_list, resolve_recipient_relays, unwrap_dm, wrap_dm};
use nostr::prelude::*;

use crate::harness::{result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("DM1 happy path", dm1(ctx).await),
        result("DM2 sender hidden", dm2(ctx).await),
        result("DM3 wrong recipient", dm3(ctx).await),
        dm4(ctx).await,
        dm5(ctx).await,
        dm6(ctx).await,
    ]
}

fn inbox(recipient: &Identity) -> Filter {
    Filter::new().kind(Kind::GiftWrap).pubkey(recipient.public_key())
}

async fn dm1(ctx: &Ctx) -> Result<()> {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let wrap = wrap_dm(&alice, &bob.public_key(), "back room is open").await?;

    let ac = ctx.connect(&alice).await?;
    ac.publish(&wrap).await?;
    ac.disconnect().await;
    settle().await;

    let bc = ctx.connect(&bob).await?;
    let got = bc.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    bc.disconnect().await;
    ensure!(!got.is_empty(), "bob's inbox is empty");

    let dm = unwrap_dm(&bob, &got[0]).await?;
    ensure!(dm.content == "back room is open", "plaintext mismatch: {}", dm.content);
    ensure!(dm.sender == alice.public_key(), "recovered sender is not alice (chain verify failed)");
    Ok(())
}

async fn dm2(ctx: &Ctx) -> Result<()> {
    // The wrap the relay stores is signed by an ephemeral key — never the sender's npub.
    let alice = Identity::generate();
    let bob = Identity::generate();
    let wrap = wrap_dm(&alice, &bob.public_key(), "hi").await?;

    let ac = ctx.connect(&alice).await?;
    ac.publish(&wrap).await?;
    ac.disconnect().await;
    settle().await;

    let bc = ctx.connect(&bob).await?;
    let got = bc.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    bc.disconnect().await;
    ensure!(!got.is_empty(), "bob's inbox is empty");
    ensure!(got[0].kind == Kind::GiftWrap, "not a gift wrap");
    ensure!(got[0].pubkey != alice.public_key(), "relay can attribute the wrap to the sender");
    ensure!(got[0].pubkey != bob.public_key(), "wrap signed by the recipient");
    Ok(())
}

async fn dm3(ctx: &Ctx) -> Result<()> {
    // A DM addressed to bob does not reach carol's inbox, and even if handed the wrap, carol
    // cannot decrypt it.
    let alice = Identity::generate();
    let bob = Identity::generate();
    let carol = Identity::generate();
    let wrap = wrap_dm(&alice, &bob.public_key(), "secret").await?;

    let ac = ctx.connect(&alice).await?;
    ac.publish(&wrap).await?;
    ac.disconnect().await;
    settle().await;

    let cc = ctx.connect(&carol).await?;
    let carol_inbox = cc.fetch(inbox(&carol), FETCH_TIMEOUT).await?;
    // Fetch bob's wrap back from the relay and hand it to carol to prove she still can't read it.
    let bob_wrap = cc.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    cc.disconnect().await;

    ensure!(carol_inbox.is_empty(), "carol received a DM addressed to bob");
    ensure!(!bob_wrap.is_empty(), "could not refetch bob's wrap for the decrypt check");
    ensure!(
        unwrap_dm(&carol, &bob_wrap[0]).await.is_err(),
        "carol decrypted a DM addressed to bob"
    );
    Ok(())
}

async fn dm4(ctx: &Ctx) -> TestResult {
    let name = "DM4 multi-relay dedup";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, dm4_inner(ctx).await)
}

async fn dm4_inner(ctx: &Ctx) -> Result<()> {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let wrap = wrap_dm(&alice, &bob.public_key(), "dup me").await?;

    // Publish to all relays; the same wrap now lives on both.
    let ac = ctx.connect(&alice).await?;
    ac.publish(&wrap).await?;
    ac.disconnect().await;
    settle().await;

    // A multi-relay fetch collapses the two copies to one.
    let bc = ctx.connect(&bob).await?;
    let got = bc.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    bc.disconnect().await;
    ensure!(got.len() == 1, "same DM from {} relays did not dedup to 1, got {}", ctx.relays.len(), got.len());
    Ok(())
}

async fn dm5(ctx: &Ctx) -> TestResult {
    let name = "DM5 delivered to recipient read-relay (disjoint sets)";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, dm5_inner(ctx).await)
}

/// W2 / spec §9 — the case that fails against pre-M12 code: Alice and Bob are on **disjoint** relays.
/// Bob advertises relay B as his NIP-65 **read** relay; Alice (on relay A) resolves it and targets
/// the wrap there, so Bob (on relay B) fetches the DM even though Alice never reads/writes B otherwise.
async fn dm5_inner(ctx: &Ctx) -> Result<()> {
    let relay_a = ctx.relays[0].clone();
    let relay_b = ctx.relays[1].clone();
    let alice = Identity::generate();
    let bob = Identity::generate();

    // Bob advertises relay B as his read-relay, published where Alice can discover it (relay A — the
    // overlapping bootstrap relay for the kind-10002 lookup).
    let bob_on_a = ctx.connect_one(&bob, 0).await?;
    bob_on_a
        .publish(&build_relay_list(&bob, std::slice::from_ref(&relay_b), std::slice::from_ref(&relay_b))?)
        .await?;
    bob_on_a.disconnect().await;
    settle().await;

    // Alice (on relay A only) sends — resolve Bob's read-relays, ensure them, target the publish.
    let own = vec![relay_a.clone()];
    let ac = ctx.connect_one(&alice, 0).await?;
    let wrap = wrap_dm(&alice, &bob.public_key(), "see you on B").await?;
    let targets = resolve_recipient_relays(&ac, &bob.public_key(), &own, &own, FETCH_TIMEOUT).await;
    ensure!(targets.iter().any(|r| r == &relay_b), "Bob's read-relay was not resolved into the target set: {targets:?}");
    ac.ensure_relays(&targets, FETCH_TIMEOUT).await?;
    ac.publish_to(&wrap, &targets).await?;
    ac.disconnect().await;
    settle().await;

    // Bob, who only reads relay B, fetches the DM.
    let bc = ctx.connect_one(&bob, 1).await?;
    let got = bc.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    bc.disconnect().await;
    ensure!(!got.is_empty(), "Bob's read-relay (B) never received the DM — read-relay delivery failed");
    let dm = unwrap_dm(&bob, &got[0]).await?;
    ensure!(dm.content == "see you on B", "plaintext mismatch: {}", dm.content);
    Ok(())
}

async fn dm6(ctx: &Ctx) -> TestResult {
    let name = "DM6 targeted publish excludes unrelated relays";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, dm6_inner(ctx).await)
}

/// chorus #3 negative — the wrap must NOT be blasted to a connected-but-unrelated relay. Alice's pool
/// includes relay B (accreted from a prior browse), but the recipient's inbox + Alice's own are relay
/// A only → `publish_to` targets A alone, and the wrap is **absent** from relay B.
async fn dm6_inner(ctx: &Ctx) -> Result<()> {
    let relay_a = ctx.relays[0].clone();
    let relay_b = ctx.relays[1].clone();
    let alice = Identity::generate();
    let bob = Identity::generate();

    // Bob's read-relay is A (published to A for discovery).
    let bob_on_a = ctx.connect_one(&bob, 0).await?;
    bob_on_a
        .publish(&build_relay_list(&bob, std::slice::from_ref(&relay_a), std::slice::from_ref(&relay_a))?)
        .await?;
    bob_on_a.disconnect().await;
    settle().await;

    // Alice connects to A, then accretes relay B (as a prior browse of some other peer would).
    let own = vec![relay_a.clone()];
    let ac = ctx.connect_one(&alice, 0).await?;
    ac.ensure_relays(std::slice::from_ref(&relay_b), FETCH_TIMEOUT).await?; // B is now in Alice's pool but unrelated

    let wrap = wrap_dm(&alice, &bob.public_key(), "A only").await?;
    let targets = resolve_recipient_relays(&ac, &bob.public_key(), &own, &own, FETCH_TIMEOUT).await;
    ensure!(!targets.iter().any(|r| r == &relay_b), "relay B leaked into the DM target set: {targets:?}");
    ac.ensure_relays(&targets, FETCH_TIMEOUT).await?;
    ac.publish_to(&wrap, &targets).await?;
    ac.disconnect().await;
    settle().await;

    // The wrap is on A (intended) but ABSENT from B (the accreted, unrelated relay).
    let on_a = ctx.connect_one(&bob, 0).await?;
    let from_a = on_a.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    on_a.disconnect().await;
    let on_b = ctx.connect_one(&bob, 1).await?;
    let from_b = on_b.fetch(inbox(&bob), FETCH_TIMEOUT).await?;
    on_b.disconnect().await;

    ensure!(!from_a.is_empty(), "the wrap should be on relay A (the targeted relay)");
    ensure!(from_b.is_empty(), "the wrap leaked to relay B (an unrelated connected relay) — targeting failed");
    Ok(())
}
