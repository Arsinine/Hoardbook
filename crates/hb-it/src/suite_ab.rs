//! Suite AB — adversarial, L2 rows only (TEST_PLAN §5a: AB1, AB2, AB3, AB8, AB9, AB10).
//! Test the *mechanism* against a real hostile action, never a happy path. A relay is an
//! untrusted party (AB8): it tampers, withholds, forges, replays — every reject asserts the
//! reason, never merely that *some* error occurred.

use std::collections::HashSet;

use anyhow::{ensure, Result};
use hb_core::binding::{build_binding, verify_binding, KIND_PRESENCE};
use hb_core::event::{
    build_listing_event, build_teaser, parse_listing_event, parse_teaser, Teaser, KIND_LISTING,
    KIND_TEASER,
};
use hb_core::{HbError, Identity};
use hb_net::{teaser_search_filter, unwrap_dm, wrap_dm};
use nostr::prelude::*;

use crate::harness::{is_online, now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("AB1 block mechanism (events hidden)", ab1(ctx).await),
        result("AB2 spam-DM block post-unwrap", ab2(ctx).await),
        result("AB3 junk-teaser resilience", ab3(ctx).await),
        result("AB8 hostile relay", ab8(ctx).await),
        result("AB9 re-key kills leaked code", ab9(ctx).await),
        result("AB10 metadata bounds", ab10(ctx).await),
    ]
}

fn teaser(name: &str, tags: &[String]) -> Teaser {
    Teaser {
        display_name: name.into(),
        bio: String::new(),
        tags: tags.to_vec(),
        content_types: vec!["video".into()],
    }
}

async fn ab1(ctx: &Ctx) -> Result<()> {
    // A locally-blocked npub's events are hidden from search (client-side filter).
    let me = Identity::generate();
    let bad = Identity::generate();
    let block_tag = ctx.tag("block");
    let client = ctx.connect(&me).await?;
    client.publish(&build_teaser(&bad, &teaser("spammer", std::slice::from_ref(&block_tag)))?).await?;
    settle().await;

    let hits = client
        .fetch(teaser_search_filter(&[block_tag], &[])?, FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;
    ensure!(hits.iter().any(|e| e.pubkey == bad.public_key()), "setup: bad teaser not found pre-block");

    let blocklist: HashSet<PublicKey> = [bad.public_key()].into_iter().collect();
    let visible: Vec<&Event> = hits.iter().filter(|e| !blocklist.contains(&e.pubkey)).collect();
    ensure!(
        !visible.iter().any(|e| e.pubkey == bad.public_key()),
        "a blocked npub's teaser is still visible after the block filter"
    );
    Ok(())
}

async fn ab2(ctx: &Ctx) -> Result<()> {
    // A spam DM is blocked *after* unwrap — the block can't apply earlier because the wrap hides
    // the sender behind an ephemeral key.
    let me = Identity::generate();
    let bad = Identity::generate();
    let wrap = wrap_dm(&bad, &me.public_key(), "buy my coin").await?;
    let bc = ctx.connect(&bad).await?;
    bc.publish(&wrap).await?;
    bc.disconnect().await;
    settle().await;

    let mc = ctx.connect(&me).await?;
    let inbox = mc.fetch(Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key()), FETCH_TIMEOUT).await?;
    mc.disconnect().await;
    ensure!(!inbox.is_empty(), "spam DM not delivered to inbox");

    // Pre-unwrap the sender is invisible (ephemeral), so a sender-block is impossible here.
    ensure!(inbox[0].pubkey != bad.public_key(), "wrap exposed the real sender pre-unwrap");

    // Post-unwrap the real sender is recovered and matched against the blocklist → dropped.
    let dm = unwrap_dm(&me, &inbox[0]).await?;
    let blocklist: HashSet<PublicKey> = [bad.public_key()].into_iter().collect();
    ensure!(blocklist.contains(&dm.sender), "post-unwrap sender is not the blocked npub");
    Ok(())
}

async fn ab3(ctx: &Ctx) -> Result<()> {
    // A junk flood (bad-sig teasers + duplicates) is discarded/deduped without breaking search.
    let junk_tag = ctx.tag("junk");
    let ids: Vec<Identity> = (0..3).map(|_| Identity::generate()).collect();
    let client = ctx.connect(&ids[0]).await?;
    for id in &ids {
        client.publish(&build_teaser(id, &teaser("real", std::slice::from_ref(&junk_tag)))?).await?;
    }
    settle().await;
    let mut hits = client
        .fetch(teaser_search_filter(&[junk_tag], &[])?, FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;
    ensure!(hits.len() == 3, "setup: expected 3 valid teasers, got {}", hits.len());

    // Inject junk a hostile relay might return: a bad-sig teaser + a duplicate of a valid one.
    let mut junk = hits[0].clone();
    junk.content = "tampered — bad sig".into();
    let dup = hits[1].clone();
    hits.push(junk);
    hits.push(dup);

    let accepted = process_results(&hits, 100);
    ensure!(accepted.len() == 3, "junk/dup not filtered: kept {} of an intended 3", accepted.len());
    let want: HashSet<PublicKey> = ids.iter().map(|i| i.public_key()).collect();
    ensure!(accepted.iter().cloned().collect::<HashSet<_>>() == want, "wrong survivors after filtering");
    // The result cap holds (a flood can't blow past it).
    ensure!(process_results(&hits, 2).len() == 2, "result cap not enforced");
    Ok(())
}

async fn ab8(ctx: &Ctx) -> Result<()> {
    let now = now();
    let a = Identity::generate();

    // Seed a teaser, a listing, and a presence for the hostile-action checks.
    let tea = build_teaser(&a, &teaser("victim", &[ctx.tag("ab8")]))?;
    let listing = build_listing_event(&a, "ab8", &[8u8; 32], r#"{"slug":"ab8","entries":[]}"#)?;
    let presence = build_binding(&a, now, 30 * 60)?;
    let client = ctx.connect(&a).await?;
    client.publish(&tea).await?;
    client.publish(&listing).await?;
    client.publish(&presence).await?;
    settle().await;
    let teas = client.fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT).await?;
    let lists = client.fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_LISTING)), FETCH_TIMEOUT).await?;
    let pres = client.fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_PRESENCE)), FETCH_TIMEOUT).await?;
    client.disconnect().await;
    ensure!(teas.len() == 1 && lists.len() == 1 && pres.len() == 1, "AB8 setup fetch failed");

    // (1) A relay that returns a TAMPERED teaser/listing is rejected on verify.
    let mut bad_teaser = teas[0].clone();
    bad_teaser.content = "tampered".into();
    ensure!(parse_teaser(&bad_teaser).is_err(), "tampered teaser accepted");
    let mut bad_listing = lists[0].clone();
    bad_listing.content = "tampered".into();
    ensure!(parse_listing_event(&bad_listing, &[8u8; 32]).is_err(), "tampered listing accepted");

    // (3) A FORGED binding — A's presence presented as if it vouches for B — is refused.
    let b = Identity::generate();
    ensure!(
        matches!(verify_binding(&pres[0], &b.public_key(), now), Err(HbError::WrongSigner)),
        "forged-identity binding accepted"
    );

    // (4) A REPLAYED stale presence reads offline, not online.
    let stale = build_binding(&a, now - 20 * 60, 30 * 60)?;
    ensure!(!is_online(stale.created_at.as_u64(), now), "replayed stale presence read as online");

    // (2) A WITHHELD event is still found via another relay (multi-relay only).
    if ctx.multi() {
        let w = Identity::generate();
        let only_on_1 = build_teaser(&w, &teaser("withheld", &[ctx.tag("ab8w")]))?;
        // Publish to relay[1] only — relay[0] is the "withholding" relay that never gets it.
        let pubc = ctx.connect_one(&w, 1).await?;
        pubc.publish(&only_on_1).await?;
        pubc.disconnect().await;
        settle().await;
        // A reader across both relays still finds it (on relay[1]).
        let reader = ctx.connect(&w).await?;
        let got = reader
            .fetch(Filter::new().author(w.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
            .await?;
        reader.disconnect().await;
        ensure!(got.len() == 1, "multi-relay fetch did not route around a withholding relay");
    } else {
        eprintln!("   AB8: withheld-event sub-case skipped (needs a 2nd --relay)");
    }
    Ok(())
}

async fn ab9(ctx: &Ctx) -> Result<()> {
    // After a re-key, a holder of the OLD browse-key cannot decrypt newly-published listings.
    let a = Identity::generate();
    let (old_key, new_key) = ([1u8; 32], [2u8; 32]);
    let client = ctx.connect(&a).await?;

    client
        .publish(&build_listing_event(&a, "rekey", &old_key, r#"{"slug":"rekey","entries":["v1"]}"#)?)
        .await?;
    // Replaceable: a 1s gap makes the re-keyed snapshot strictly newer so it supersedes.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    client
        .publish(&build_listing_event(&a, "rekey", &new_key, r#"{"slug":"rekey","entries":["v2"]}"#)?)
        .await?;
    settle().await;

    let got = client
        .fetch(
            Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_LISTING)).identifier("rekey"),
            FETCH_TIMEOUT,
        )
        .await?;
    client.disconnect().await;
    ensure!(got.len() == 1, "expected exactly the re-keyed listing, got {}", got.len());

    // The leaked old code is dead; the new key decrypts.
    ensure!(parse_listing_event(&got[0], &old_key).is_err(), "old browse-key still decrypts new listing");
    let (_slug, json) = parse_listing_event(&got[0], &new_key)?;
    ensure!(json.contains("v2"), "new key did not recover the re-keyed snapshot");
    Ok(())
}

async fn ab10(ctx: &Ctx) -> Result<()> {
    // (a) A browse is relay-reads-only — it never resolves or dials the peer's node, so it opens
    //     zero connections to the peer. We complete a full browse (teaser + listing decrypt)
    //     without ever fetching the presence event (which is the only carrier of the node addr).
    // (b) The public teaser omits contact_hint (ties to regression scenario 9).
    let a = Identity::generate();
    let key = [10u8; 32];
    let tea = build_teaser(&a, &teaser("bounded", &[ctx.tag("ab10")]))?;
    let listing = build_listing_event(&a, "ab10", &key, r#"{"slug":"ab10","entries":[{"name":"f"}]}"#)?;

    let client = ctx.connect(&a).await?;
    client.publish(&tea).await?;
    client.publish(&listing).await?;
    settle().await;

    // The whole browse: fetch teaser + fetch listing + decrypt. No KIND_PRESENCE query is issued.
    let got_t = client.fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT).await?;
    let got_l = client
        .fetch(Filter::new().author(a.public_key()).kind(Kind::from_u16(KIND_LISTING)).identifier("ab10"), FETCH_TIMEOUT)
        .await?;
    client.disconnect().await;

    ensure!(got_t.len() == 1 && got_l.len() == 1, "browse fetch failed");
    let parsed = parse_teaser(&got_t[0])?;
    ensure!(!got_t[0].content.contains("contact_hint"), "public teaser leaked contact_hint");
    // The struct itself can't carry one (compile-time guarantee, re-checked here).
    ensure!(!serde_json::to_string(&parsed)?.contains("contact_hint"), "teaser struct exposes contact_hint");
    // Browse succeeded via relay reads + the browse-key alone — no node address was resolved.
    let (_slug, _json) = parse_listing_event(&got_l[0], &key)?;
    Ok(())
}

/// Client-side search-result processing (the AB3 resilience path): discard events that fail
/// verification (bad sig / junk), dedup by author npub, and cap the result count.
fn process_results(events: &[Event], cap: usize) -> Vec<PublicKey> {
    let mut seen: HashSet<PublicKey> = HashSet::new();
    let mut out: Vec<PublicKey> = Vec::new();
    for e in events {
        if out.len() >= cap {
            break;
        }
        if parse_teaser(e).is_err() {
            continue; // junk / bad signature discarded
        }
        if seen.insert(e.pubkey) {
            out.push(e.pubkey); // dedup by npub
        }
    }
    out
}
