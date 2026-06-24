//! Suite TOPIC — Topics (M11; spec §11; TEST_PLAN §Suite TOPIC). The relay round-trip + the
//! **observable** invariants over the ephemeral strfry; the crypto negatives live at L1 (hb-core
//! `topic`). Proves: the **public participation bar both directions** (a non-member sees ciphertext
//! only; a joiner obtains the key and reads/writes), the **member-pubkey-is-a-pseudonym** raw-query
//! (no real npub leaks, m6), the **spoofable** member count, **private** invite + request→approve
//! admission (a non-member finds nothing), **M3** any-member-may-invite, **leave→shrink +
//! auto-dissolve**, the **24h** channel filter, and **F14** multi-relay fetch-from-each.

use anyhow::{anyhow, ensure, Result};
use hb_core::topic::{
    build_announce, build_public_join, roster, seal_membership, topic_id_for_name, NonceSet,
    TopicKey, TopicMeta, KIND_TOPIC_MEMBER, POST_TTL_SECS,
};
use hb_core::{new_topic, Identity};
use hb_net::{
    approve_join, discover_public_topics, fetch_channel, fetch_invite, fetch_join_requests,
    fetch_membership_events, fetch_roster, join_public, join_topic, leave_topic, member_count,
    post_to_channel, publish_topic, request_join,
};
use nostr::prelude::*;

use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        topic1(ctx).await,
        result("TOPIC2 public participation bar: non-member sees ciphertext, joiner reads+writes (B1)", topic2(ctx).await),
        result("TOPIC3 raw membership query: every pubkey is a derived pseudonym, no npub leaks (m6/B2)", topic3(ctx).await),
        result("TOPIC4 member_count is the spoofable tagged count; roster excludes the forgery", topic4(ctx).await),
        result("TOPIC5 private: unlisted (no announce) + invite admits; non-member finds nothing", topic5(ctx).await),
        result("TOPIC6 private: request→approve admits over a NIP-17 DM", topic6(ctx).await),
        result("TOPIC7 M3: any member may invite — a non-creator admits a newcomer (intended)", topic7(ctx).await),
        result("TOPIC8 leave retracts (roster shrinks); last leave ⇒ dissolved (empty roster)", topic8(ctx).await),
        result("TOPIC9 channel: NIP-40 expiry tag present + the client filters >24h locally", topic9(ctx).await),
        result("TOPIC10 W4: same path (case/space variant) converges + activity-ranked discovery", topic10(ctx).await),
    ]
}

// ── helpers ──────────────────────────────────────────────────────────────────────────────────────

/// A per-test-unique public Topic (so the name-derived `topic_id` can't collide across tests/runs).
/// W4: a public name is a **category-rooted path** now (`video/…`), so the per-test suffix sits under
/// the `video` root.
fn mk_public(ctx: &Ctx, suffix: &str) -> (TopicMeta, TopicKey) {
    let name = format!("video/hbit-topic-{}-{}", suffix, ctx.run_id);
    new_topic(&name, "a subject group", vec![ctx.tag(&format!("topic-{suffix}"))], false).unwrap()
}

/// A per-test-unique private Topic (random topic_id, no announce; freeform name — W4 root rule is
/// public-only).
fn mk_private(ctx: &Ctx, suffix: &str) -> (TopicMeta, TopicKey) {
    let name = format!("hbit-priv-{}-{}", suffix, ctx.run_id);
    new_topic(&name, "secret", vec![ctx.tag(&format!("priv-{suffix}"))], true).unwrap()
}

/// Create a public Topic: publish the announce + the public-join credential + the creator's own
/// membership to all relays. Returns the creator's membership event.
async fn create_public(ctx: &Ctx, creator: &Identity, meta: &TopicMeta, key: &TopicKey) -> Result<Event> {
    let announce = build_announce(creator, meta, now())?;
    let public_join = build_public_join(creator, meta, key, now())?;
    let membership = seal_membership(key, &meta.topic_id, &creator, now())?;
    let cc = ctx.connect(creator).await?;
    publish_topic(&cc, &[announce, public_join, membership.clone()]).await?;
    cc.disconnect().await;
    settle().await;
    Ok(membership)
}

// ── TOPIC1 (F14 multi-relay) ─────────────────────────────────────────────────────────────────────

/// TOPIC1 (F14): a public Topic's announce + membership are fetchable from **each** relay individually.
async fn topic1(ctx: &Ctx) -> TestResult {
    let name = "TOPIC1 multi-relay: announce + membership fetchable from each relay (F14)";
    if !ctx.multi() {
        return TestResult::skip(name, "needs a 2nd --relay");
    }
    result(name, topic1_inner(ctx).await)
}

async fn topic1_inner(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "f14");
    create_public(ctx, &creator, &meta, &key).await?;

    for idx in 0..ctx.relays.len() {
        let rc = ctx.connect_one(&creator, idx).await?;
        let found = discover_public_topics(&rc, &meta.tags, FETCH_TIMEOUT).await?;
        let members = fetch_membership_events(&rc, &meta.topic_id, FETCH_TIMEOUT).await?;
        rc.disconnect().await;
        ensure!(found.iter().any(|(m, _)| m.topic_id == meta.topic_id), "relay {idx}: announce missing");
        ensure!(!members.is_empty(), "relay {idx}: membership missing");
    }
    Ok(())
}

// ── TOPIC2 (the participation bar, both directions) ──────────────────────────────────────────────

async fn topic2(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "bar");
    create_public(ctx, &creator, &meta, &key).await?;

    // (a) A non-member holds no key: raw membership events exist (ciphertext), but the roster
    //     identities are unreadable. member_count works WITHOUT a key (it's the discovery signal).
    let non_member = Identity::generate();
    let (_om, wrong_key) = new_topic("other/unrelated", "", vec![], false).unwrap();
    let nc = ctx.connect(&non_member).await?;
    let raw = fetch_membership_events(&nc, &meta.topic_id, FETCH_TIMEOUT).await?;
    ensure!(!raw.is_empty(), "membership events are on the relay (ciphertext)");
    ensure!(roster(&wrong_key, &raw).is_empty(), "a non-member (wrong/no key) cannot read the roster identities");
    let count = member_count(&nc, &meta.topic_id, FETCH_TIMEOUT).await?;
    nc.disconnect().await;
    ensure!(count >= 1, "the member count is visible pre-join, got {count}");

    // (b) A joiner obtains the key via the public-join credential, joins, reads the roster, posts.
    let joiner = Identity::generate();
    let jc = ctx.connect(&joiner).await?;
    let (jmeta, jkey) = join_public(&jc, &meta.name, &mut NonceSet::new(), now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("a joiner found no public-join credential"))?;
    ensure!(jkey.as_bytes() == key.as_bytes(), "the joiner obtains the real topic key");
    join_topic(&jc, &jkey, &jmeta.topic_id, &joiner, now()).await?;
    post_to_channel(&jc, &jkey, &jmeta.topic_id, &joiner, "hello topic", now()).await?;
    jc.disconnect().await;
    settle().await;

    let rc = ctx.connect(&joiner).await?;
    let ros = fetch_roster(&rc, &meta.topic_id, &jkey, FETCH_TIMEOUT).await?;
    let chan = fetch_channel(&rc, &meta.topic_id, &jkey, now(), FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(ros.contains(&creator.public_key()), "the joiner reads the creator on the roster");
    ensure!(ros.contains(&joiner.public_key()), "the joiner's own membership is now visible");
    ensure!(chan.iter().any(|p| p.body == "hello topic"), "the joiner reads the channel post");
    Ok(())
}

// ── TOPIC3 (raw query — no real npub leaks) ──────────────────────────────────────────────────────

async fn topic3(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "pseudo");
    create_public(ctx, &creator, &meta, &key).await?;
    // A second member, so there are ≥2 pseudonyms to check.
    let m2 = Identity::generate();
    let ev = seal_membership(&key, &meta.topic_id, &m2, now())?;
    let cc = ctx.connect(&creator).await?;
    publish_topic(&cc, std::slice::from_ref(&ev)).await?;
    cc.disconnect().await;
    settle().await;

    let observer = Identity::generate();
    let oc = ctx.connect(&observer).await?;
    let raw = fetch_membership_events(&oc, &meta.topic_id, FETCH_TIMEOUT).await?;
    oc.disconnect().await;
    ensure!(raw.len() >= 2, "expected ≥2 membership events, got {}", raw.len());
    for e in &raw {
        ensure!(e.pubkey != creator.public_key(), "a membership pubkey leaked the creator's real npub");
        ensure!(e.pubkey != m2.public_key(), "a membership pubkey leaked a member's real npub");
    }
    Ok(())
}

// ── TOPIC4 (member_count is spoofable; roster is sound) ──────────────────────────────────────────

async fn topic4(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "count");
    create_public(ctx, &creator, &meta, &key).await?;

    // A forged membership event: a random key, tagged to the topic_id, junk (un-decryptable) content.
    let forger = Identity::generate();
    let forged = forger
        .sign(
            EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), "not-a-real-encrypted-membership")
                .tags([Tag::identifier(meta.topic_id.clone())])
                .custom_created_at(Timestamp::from(now())),
        )
        .map_err(|e| anyhow!("{e}"))?;
    let cc = ctx.connect(&forger).await?;
    publish_topic(&cc, std::slice::from_ref(&forged)).await?;
    cc.disconnect().await;
    settle().await;

    let oc = ctx.connect(&creator).await?;
    let count = member_count(&oc, &meta.topic_id, FETCH_TIMEOUT).await?;
    let raw = fetch_membership_events(&oc, &meta.topic_id, FETCH_TIMEOUT).await?;
    oc.disconnect().await;
    // The count is inflated by the forgery (spoofable, documented limit)...
    ensure!(count >= 2, "a forged membership inflates the spoofable count, got {count}");
    // ...but the decrypted roster excludes it (un-decryptable / unbound), so it stays sound.
    let sound = roster(&key, &raw);
    ensure!(sound == vec![creator.public_key()], "the decrypted roster excludes the forgery: {sound:?}");
    Ok(())
}

// ── TOPIC5 (private: unlisted + invite admits) ───────────────────────────────────────────────────

async fn topic5(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let invitee = Identity::generate();
    let (meta, key) = mk_private(ctx, "inv");

    // Create the private Topic: NO announce — just the creator's membership.
    let membership = seal_membership(&key, &meta.topic_id, &creator, now())?;
    let cc = ctx.connect(&creator).await?;
    publish_topic(&cc, std::slice::from_ref(&membership)).await?;

    // A non-member's public discovery (by the topic's tag) finds nothing — it is unlisted.
    let discovered = discover_public_topics(&cc, &meta.tags, FETCH_TIMEOUT).await?;
    ensure!(
        !discovered.iter().any(|(m, _)| m.topic_id == meta.topic_id),
        "a private Topic must NOT be publicly discoverable"
    );

    // Admit the invitee with a sealed, single-use, expiring invite.
    approve_join(&cc, &creator, &invitee.public_key(), &meta, &key, now()).await?;
    cc.disconnect().await;
    settle().await;

    // The invitee redeems the invite, joins, and reads the roster.
    let ic = ctx.connect(&invitee).await?;
    let (imeta, ikey) = fetch_invite(&ic, &invitee, &mut NonceSet::new(), now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("the invitee found no invite"))?;
    ensure!(ikey.as_bytes() == key.as_bytes(), "the invite carries the real topic key");
    join_topic(&ic, &ikey, &imeta.topic_id, &invitee, now()).await?;
    ic.disconnect().await;
    settle().await;

    let rc = ctx.connect(&invitee).await?;
    let ros = fetch_roster(&rc, &meta.topic_id, &ikey, FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(ros.contains(&creator.public_key()) && ros.contains(&invitee.public_key()), "the admitted invitee is on the roster");
    Ok(())
}

// ── TOPIC6 (private: request → approve over a DM) ────────────────────────────────────────────────

async fn topic6(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let requester = Identity::generate();
    let (meta, key) = mk_private(ctx, "req");

    let membership = seal_membership(&key, &meta.topic_id, &creator, now())?;
    let cc = ctx.connect(&creator).await?;
    publish_topic(&cc, std::slice::from_ref(&membership)).await?;
    cc.disconnect().await;
    settle().await;

    // The requester (who learned of the Topic from a member) DMs a join request to the creator.
    let qc = ctx.connect(&requester).await?;
    request_join(&qc, &requester, &creator.public_key(), &meta.topic_id, &meta.name).await?;
    qc.disconnect().await;
    settle().await;

    // The creator reads the request and approves it (mints + publishes an invite to the requester).
    let cc = ctx.connect(&creator).await?;
    let reqs = fetch_join_requests(&cc, &creator, FETCH_TIMEOUT).await?;
    ensure!(
        reqs.iter().any(|(who, r)| *who == requester.public_key() && r.topic_id == meta.topic_id),
        "the creator received the join request"
    );
    approve_join(&cc, &creator, &requester.public_key(), &meta, &key, now()).await?;
    cc.disconnect().await;
    settle().await;

    // The requester redeems the approval + joins.
    let rc = ctx.connect(&requester).await?;
    let (imeta, ikey) = fetch_invite(&rc, &requester, &mut NonceSet::new(), now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("the requester found no approval invite"))?;
    join_topic(&rc, &ikey, &imeta.topic_id, &requester, now()).await?;
    rc.disconnect().await;
    settle().await;

    let vc = ctx.connect(&requester).await?;
    let ros = fetch_roster(&vc, &meta.topic_id, &ikey, FETCH_TIMEOUT).await?;
    vc.disconnect().await;
    ensure!(ros.contains(&requester.public_key()), "the approved requester joined the roster");
    Ok(())
}

// ── TOPIC7 (M3 — any member may invite) ──────────────────────────────────────────────────────────

async fn topic7(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let member_a = Identity::generate(); // admitted by the creator
    let newcomer = Identity::generate(); // admitted by member_a (NOT the creator)
    let (meta, key) = mk_private(ctx, "m3");

    // Creator creates + admits member_a.
    let cm = seal_membership(&key, &meta.topic_id, &creator, now())?;
    let cc = ctx.connect(&creator).await?;
    publish_topic(&cc, std::slice::from_ref(&cm)).await?;
    approve_join(&cc, &creator, &member_a.public_key(), &meta, &key, now()).await?;
    cc.disconnect().await;
    settle().await;

    // member_a redeems, joins, then — as a non-creator member — invites the newcomer (M3).
    let ac = ctx.connect(&member_a).await?;
    let (ameta, akey) = fetch_invite(&ac, &member_a, &mut NonceSet::new(), now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("member_a found no invite"))?;
    join_topic(&ac, &akey, &ameta.topic_id, &member_a, now()).await?;
    approve_join(&ac, &member_a, &newcomer.public_key(), &ameta, &akey, now()).await?;
    ac.disconnect().await;
    settle().await;

    // The newcomer redeems member_a's invite + joins.
    let nc = ctx.connect(&newcomer).await?;
    let (nmeta, nkey) = fetch_invite(&nc, &newcomer, &mut NonceSet::new(), now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("the newcomer found no invite from member_a"))?;
    join_topic(&nc, &nkey, &nmeta.topic_id, &newcomer, now()).await?;
    nc.disconnect().await;
    settle().await;

    // The CREATOR (another member) sees the newcomer member_a admitted — allowed by design.
    let vc = ctx.connect(&creator).await?;
    let ros = fetch_roster(&vc, &meta.topic_id, &key, FETCH_TIMEOUT).await?;
    vc.disconnect().await;
    ensure!(ros.contains(&newcomer.public_key()), "a newcomer admitted by a NON-creator member is on the roster (M3)");
    Ok(())
}

// ── TOPIC8 (leave → shrink; auto-dissolve) ───────────────────────────────────────────────────────

async fn topic8(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let joiner = Identity::generate();
    let (meta, key) = mk_public(ctx, "leave");
    let cm = create_public(ctx, &creator, &meta, &key).await?;

    // The joiner joins → roster of 2.
    let jc = ctx.connect(&joiner).await?;
    let jm = join_topic(&jc, &key, &meta.topic_id, &joiner, now()).await?;
    jc.disconnect().await;
    settle().await;
    let vc = ctx.connect(&creator).await?;
    ensure!(fetch_roster(&vc, &meta.topic_id, &key, FETCH_TIMEOUT).await?.len() == 2, "two members joined");
    vc.disconnect().await;

    // The creator leaves (retracts) → roster shrinks to the joiner.
    let cc = ctx.connect(&creator).await?;
    leave_topic(&cc, &key, &creator.public_key(), &cm, now()).await?;
    cc.disconnect().await;
    settle().await;
    let vc = ctx.connect(&creator).await?;
    let after = fetch_roster(&vc, &meta.topic_id, &key, FETCH_TIMEOUT).await?;
    vc.disconnect().await;
    ensure!(after == vec![joiner.public_key()], "after the creator leaves, only the joiner remains: {after:?}");

    // The joiner leaves too → empty roster ⇒ dissolved (derived).
    let jc = ctx.connect(&joiner).await?;
    leave_topic(&jc, &key, &joiner.public_key(), &jm, now()).await?;
    jc.disconnect().await;
    settle().await;
    let vc = ctx.connect(&creator).await?;
    let dissolved = fetch_roster(&vc, &meta.topic_id, &key, FETCH_TIMEOUT).await?;
    vc.disconnect().await;
    ensure!(dissolved.is_empty(), "the last leave dissolves the Topic (empty roster): {dissolved:?}");
    Ok(())
}

// ── TOPIC9 (channel 24h filter) ──────────────────────────────────────────────────────────────────

async fn topic9(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "ttl");
    create_public(ctx, &creator, &meta, &key).await?;

    let cc = ctx.connect(&creator).await?;
    let post = post_to_channel(&cc, &key, &meta.topic_id, &creator, "fresh post", now()).await?;
    cc.disconnect().await;
    settle().await;

    // The published post carries a NIP-40 expiration tag (best-effort relay GC).
    let exp = post.tags.find(TagKind::Expiration).and_then(|t| t.content()).and_then(|s| s.parse::<u64>().ok());
    ensure!(exp.is_some(), "the channel post carries a NIP-40 expiration tag");

    let rc = ctx.connect(&creator).await?;
    // Fetched now: the fresh post is present.
    let fresh = fetch_channel(&rc, &meta.topic_id, &key, now(), FETCH_TIMEOUT).await?;
    ensure!(fresh.iter().any(|p| p.body == "fresh post"), "a fresh post is in the channel");
    // Fetched as if 24h+ later: the local filter drops it even though the relay still serves it.
    let later = fetch_channel(&rc, &meta.topic_id, &key, now() + POST_TTL_SECS + 60, FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(
        !later.iter().any(|p| p.body == "fresh post"),
        "the client filters a >24h post locally regardless of the relay"
    );
    Ok(())
}

// ── TOPIC10 (W4: path convergence + activity-ranked discovery) ─────────────────────────────────────

async fn topic10(ctx: &Ctx) -> Result<()> {
    let tag = ctx.tag("w4");
    // The SAME public path, typed two ways (case + spacing): both normalize to the same canonical
    // path → the same topic_id (so two creators land in the same room — Decision K/L).
    let canonical = format!("video/hbit-{}-w4/anime", ctx.run_id);
    let variant = format!("VIDEO / hbit-{}-w4 / Anime", ctx.run_id);
    let junk = format!("video/hbit-{}-w4/loner", ctx.run_id);

    // Creator A makes the populated path (announce + public-join credential + A's membership).
    let a = Identity::generate();
    let (meta_a, key_a) = new_topic(&canonical, "anime", vec![tag.clone()], false).unwrap();
    create_public(ctx, &a, &meta_a, &key_a).await?;

    // B joins via the case/space VARIANT name — the public-join keypair derives from the normalized
    // path, so B reconstructs A's credential target and redeems the SAME room (convergence).
    let b = Identity::generate();
    let bc = ctx.connect(&b).await?;
    let mut seen = NonceSet::new();
    let (rmeta, rkey) = join_public(&bc, &variant, &mut seen, now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("B could not join via the path variant — public-join derivation diverged"))?;
    ensure!(rmeta.topic_id == topic_id_for_name(&canonical), "the variant did not converge to the canonical topic_id");
    ensure!(rmeta.topic_id == meta_a.topic_id, "variant + canonical must be the same room");
    join_topic(&bc, &rkey, &rmeta.topic_id, &b, now()).await?;
    settle().await;
    let rost = fetch_roster(&bc, &meta_a.topic_id, &key_a, FETCH_TIMEOUT).await?;
    bc.disconnect().await;
    ensure!(rost.len() == 2, "the shared roster should hold both A and B (got {})", rost.len());

    // A junk singleton under the SAME discovery tag (1 member) must rank BELOW the populated path (2).
    let c = Identity::generate();
    let (meta_junk, key_junk) = new_topic(&junk, "loner", vec![tag.clone()], false).unwrap();
    create_public(ctx, &c, &meta_junk, &key_junk).await?;
    settle().await;

    let dc = ctx.connect(&a).await?;
    let ranked = discover_public_topics(&dc, &[tag.clone()], FETCH_TIMEOUT).await?;
    dc.disconnect().await;
    let pos_pop = ranked.iter().position(|(m, _)| m.topic_id == meta_a.topic_id);
    let pos_junk = ranked.iter().position(|(m, _)| m.topic_id == meta_junk.topic_id);
    let (pop, jnk) = (
        pos_pop.ok_or_else(|| anyhow!("the populated path was not discovered"))?,
        pos_junk.ok_or_else(|| anyhow!("the junk singleton was not discovered"))?,
    );
    ensure!(pop < jnk, "activity ranking: the 2-member path ({pop}) must rank above the 1-member singleton ({jnk})");
    Ok(())
}
