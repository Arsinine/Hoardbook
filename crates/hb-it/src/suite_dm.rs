//! Suite DM — NIP-17 gift-wrapped direct messages (TEST_PLAN §4). Replaces the old Suite B.

use anyhow::{ensure, Result};
use hb_core::Identity;
use hb_net::{unwrap_dm, wrap_dm};
use nostr::prelude::*;

use crate::harness::{result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("DM1 happy path", dm1(ctx).await),
        result("DM2 sender hidden", dm2(ctx).await),
        result("DM3 wrong recipient", dm3(ctx).await),
        dm4(ctx).await,
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
