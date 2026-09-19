//! Suite TOPIC — Topics (M11; spec §11; TEST_PLAN §Suite TOPIC). The relay round-trip + the
//! **observable** invariants over the ephemeral strfry; the crypto negatives live at L1 (hb-core
//! `topic`). Proves: the **public participation bar both directions** (a non-member sees ciphertext
//! only; a joiner obtains the key and reads/writes), the **member-pubkey-is-a-pseudonym** raw-query
//! (no real npub leaks, m6), the **spoofable** member count, **private** invite + request→approve
//! admission (a non-member finds nothing), **M3** any-member-may-invite, **leave→shrink +
//! auto-dissolve**, the **24h** channel filter, **F14** multi-relay fetch-from-each, and
//! **QURATOR-227** issuer-bound invite redeem (an attacker's first-served wrap naming the same
//! topic_id is skipped; the genuine key wins).

use anyhow::{anyhow, ensure, Result};
use hb_core::topic::{
    build_announce, build_public_join, mint_invite, normalized_public_name, roster,
    seal_membership, topic_id_for_name, NonceSet, TopicKey, TopicMeta, KIND_TOPIC_MEMBER,
    KIND_TOPIC_POST, POST_TTL_SECS,
};
use hb_core::{new_topic, Identity};
use hb_net::{
    announce_to_topic, approve_join, discover_public_topics, fetch_announce, fetch_channel,
    fetch_channel_full, fetch_invite, fetch_join_requests, fetch_membership_events, fetch_roster,
    join_public, join_topic, leave_topic, member_count, post_to_channel, publish_topic,
    request_join, INVITE_TTL_SECS,
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
        result(
            "TOPIC11 announce: member broadcasts, member reads distinct-from-posts, non-member sees ciphertext, no npub leak, NIP-40 expiration tag present",
            topic11(ctx).await,
        ),
        result("TOPIC12 announce 24h local filter regardless of relay", topic12(ctx).await),
        result(
            "TOPIC13 same-name create is join-first: B lands in A's roster (devtest #11)",
            topic13(ctx).await,
        ),
        result(
            "TOPIC14 QURATOR-227: issuer-bound redeem skips the attacker's first-served wrap; the genuine issuer AND key win",
            topic14(ctx).await,
        ),
        result(
            "TOPIC15 QURATOR-294: junk in the shared join-request inbox breaks no poll (both polls)",
            topic15(ctx).await,
        ),
        result(
            "TOPIC16 QURATOR-294: an invite refused for the WRONG topic still redeems for the RIGHT one on the next poll",
            topic16(ctx).await,
        ),
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
    let membership = seal_membership(key, &meta.topic_id, creator, now())?;
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
    let (jmeta, jkey, _issuer) = join_public(&jc, &meta.name, &mut NonceSet::new(), now(), FETCH_TIMEOUT)
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
    let (imeta, ikey, _issuer) = fetch_invite(&ic, &invitee, &mut NonceSet::new(), now(), FETCH_TIMEOUT, None, None)
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
    let (imeta, ikey, _issuer) = fetch_invite(&rc, &requester, &mut NonceSet::new(), now(), FETCH_TIMEOUT, None, None)
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
    let (ameta, akey, _issuer) = fetch_invite(&ac, &member_a, &mut NonceSet::new(), now(), FETCH_TIMEOUT, None, None)
        .await?
        .ok_or_else(|| anyhow!("member_a found no invite"))?;
    join_topic(&ac, &akey, &ameta.topic_id, &member_a, now()).await?;
    approve_join(&ac, &member_a, &newcomer.public_key(), &ameta, &akey, now()).await?;
    ac.disconnect().await;
    settle().await;

    // The newcomer redeems member_a's invite + joins.
    let nc = ctx.connect(&newcomer).await?;
    let (nmeta, nkey, _issuer) = fetch_invite(&nc, &newcomer, &mut NonceSet::new(), now(), FETCH_TIMEOUT, None, None)
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
    let (rmeta, rkey, _issuer) = join_public(&bc, &variant, &mut seen, now(), FETCH_TIMEOUT)
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
    let ranked = discover_public_topics(&dc, std::slice::from_ref(&tag), FETCH_TIMEOUT).await?;
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

// ── TOPIC11 (M13 Part A: announce broadcast) ────────────────────────────────────────────────────

async fn topic11(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "announce");
    create_public(ctx, &creator, &meta, &key).await?;

    let cc = ctx.connect(&creator).await?;
    post_to_channel(&cc, &key, &meta.topic_id, &creator, "an ordinary post", now()).await?;
    let announce = announce_to_topic(&cc, &key, &meta.topic_id, &creator, "hear ye, hear ye", now()).await?;
    cc.disconnect().await;
    settle().await;

    // The announce carries a NIP-40 expiration tag — the same 24h lifecycle as a post.
    let exp = announce.tags.find(TagKind::Expiration).and_then(|t| t.content()).and_then(|s| s.parse::<u64>().ok());
    ensure!(exp.is_some(), "the announce carries a NIP-40 expiration tag");

    // A joiner (member) reads the full channel: one post, one announce, correctly partitioned.
    let joiner = Identity::generate();
    let jc = ctx.connect(&joiner).await?;
    join_topic(&jc, &key, &meta.topic_id, &joiner, now()).await?;
    settle().await;
    let read = fetch_channel_full(&jc, &meta.topic_id, &key, now(), FETCH_TIMEOUT).await?;
    jc.disconnect().await;
    ensure!(read.posts.iter().any(|p| p.body == "an ordinary post"), "the joiner reads the post");
    ensure!(
        read.announcements.iter().any(|a| a.body == "hear ye, hear ye"),
        "the joiner reads the announce, distinct from posts"
    );
    ensure!(!read.posts.iter().any(|p| p.body == "hear ye, hear ye"), "the announce must not also surface as a post");

    // A non-member's raw relay fetch of kind 1117 sees ciphertext only: pubkeys are pseudonyms, no
    // participant real npub leaks on the wire (m6/B2, restated for the broadcast).
    let observer = Identity::generate();
    let oc = ctx.connect(&observer).await?;
    let raw = oc.fetch(Filter::new().kind(Kind::from_u16(KIND_TOPIC_POST)).identifier(meta.topic_id.clone()), FETCH_TIMEOUT).await?;
    oc.disconnect().await;
    ensure!(raw.len() >= 2, "expected both the post and the announce on the relay, got {}", raw.len());
    for e in &raw {
        ensure!(e.pubkey != creator.public_key(), "a channel event pubkey leaked the creator's real npub");
    }
    Ok(())
}

// ── TOPIC12 (M13 Part A: announce 24h local filter) ─────────────────────────────────────────────

async fn topic12(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let (meta, key) = mk_public(ctx, "announce-ttl");
    create_public(ctx, &creator, &meta, &key).await?;

    let cc = ctx.connect(&creator).await?;
    announce_to_topic(&cc, &key, &meta.topic_id, &creator, "fresh announce", now()).await?;
    cc.disconnect().await;
    settle().await;

    let rc = ctx.connect(&creator).await?;
    // Fetched now: the fresh announce is present.
    let fresh = fetch_channel_full(&rc, &meta.topic_id, &key, now(), FETCH_TIMEOUT).await?;
    ensure!(fresh.announcements.iter().any(|a| a.body == "fresh announce"), "a fresh announce is in the channel");
    // Fetched as if 24h+ later: the local filter drops it even though the relay still serves it.
    let later = fetch_channel_full(&rc, &meta.topic_id, &key, now() + POST_TTL_SECS + 60, FETCH_TIMEOUT).await?;
    rc.disconnect().await;
    ensure!(
        !later.announcements.iter().any(|a| a.body == "fresh announce"),
        "the client filters a >24h announce locally regardless of the relay"
    );
    Ok(())
}

// ── TOPIC13 (devtest #11: same-name create is join-first) ──────────────────────────────────────

/// TOPIC13 (devtest #11): A creates a public Topic under one casing/spacing of a name; B, given the
/// SAME name typed differently, runs the `topic_lookup` path (`normalized_public_name` →
/// `topic_id_for_name` → `fetch_announce`), finds A's announce, and joins the **existing** room via
/// the keyless public-join credential instead of minting a fork (Decision C — same `topic_id`, same
/// `topic_key`, not a fresh key). Proves B lands in A's roster (member_count == 2) and can decrypt a
/// channel post A published under the shared key.
async fn topic13(ctx: &Ctx) -> Result<()> {
    // A run-scoped name (so re-runs don't collide with a stale announce) typed one way by A...
    let canonical = format!("video/hbit-topic13-{}/Anime Classics", ctx.run_id);
    // ...and the SAME name typed another way by B (case + whitespace variant) — must normalize to
    // the identical topic_id.
    let variant = format!("video/hbit-topic13-{}/anime classics", ctx.run_id).to_lowercase();

    // A creates the topic the same way `topic_create` does: mint + announce + public-join + own
    // membership, all published to the relay set.
    let a = Identity::generate();
    let (meta_a, key_a) = new_topic(&canonical, "a subject group", vec![ctx.tag("topic13")], false).unwrap();
    create_public(ctx, &a, &meta_a, &key_a).await?;

    // A posts to the channel so B's later read proves it holds the REAL shared key, not just a room.
    let ac = ctx.connect(&a).await?;
    post_to_channel(&ac, &key_a, &meta_a.topic_id, &a, "welcome to anime classics", now()).await?;
    ac.disconnect().await;
    settle().await;

    // B runs the join-first lookup exactly like the `topic_lookup` command: normalize the name it
    // typed, derive the topic_id the same way `new_topic`/`topic_create` would, and check for an
    // existing announce BEFORE minting anything.
    let b = Identity::generate();
    let bc = ctx.connect(&b).await?;
    let normalized = normalized_public_name(&variant)?;
    let topic_id = topic_id_for_name(&normalized);
    ensure!(topic_id == meta_a.topic_id, "the case/space variant must derive A's topic_id");

    let found = fetch_announce(&bc, &topic_id, FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("B's join-first lookup found no announce — would have forked a new topic"))?;
    ensure!(found.topic_id == meta_a.topic_id, "the found announce must be A's room");

    // B takes the keyless public-join path (join-first, not create): redeem the public-join
    // credential derived from the SAME name, obtaining A's real topic_key (no fresh mint).
    let mut seen = NonceSet::new();
    let (jmeta, jkey, _issuer) = join_public(&bc, &variant, &mut seen, now(), FETCH_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("B found no public-join credential for A's room"))?;
    ensure!(jmeta.topic_id == meta_a.topic_id, "B's join targets A's existing room, not a fork");
    ensure!(jkey.as_bytes() == key_a.as_bytes(), "B obtains A's REAL topic key, not a fresh one (Decision C)");
    join_topic(&bc, &jkey, &jmeta.topic_id, &b, now()).await?;
    settle().await;

    // B lands in A's roster (member_count == 2: A + B, same room, same key).
    let count = member_count(&bc, &meta_a.topic_id, FETCH_TIMEOUT).await?;
    ensure!(count == 2, "join-first: A + B share one roster, got member_count {count}");
    let ros = fetch_roster(&bc, &meta_a.topic_id, &jkey, FETCH_TIMEOUT).await?;
    ensure!(ros.contains(&a.public_key()) && ros.contains(&b.public_key()), "both A and B are on the shared roster: {ros:?}");

    // B can decrypt A's channel post under the shared key — the cryptographic proof of a shared room.
    let chan = fetch_channel(&bc, &meta_a.topic_id, &jkey, now(), FETCH_TIMEOUT).await?;
    bc.disconnect().await;
    ensure!(chan.iter().any(|p| p.body == "welcome to anime classics"), "B reads A's channel post under the shared key");
    Ok(())
}

// ── TOPIC14 (QURATOR-227: issuer substitution — the attacker's first-served wrap is skipped) ────

/// TOPIC14 (QURATOR-227): two valid single-use invites to the SAME invitee naming the SAME
/// `topic_id` sit in one inbox — the genuine issuer's and an attacker's (both the `topic_id` and
/// the invitee npub are public, so a decryptable forged wrap is fully constructible; it carries
/// the ATTACKER's own key). Both wraps are then RE-SEALED with pinned `created_at`s — the
/// attacker's NEWER, so the fetched vector serves it FIRST (nostr's `Events` collection iterates
/// `created_at` descending) — because a run served the genuine wrap first would pass even under
/// the regression this row exists to catch (an abort-on-mismatch `fetch_invite` loop would never
/// see the attacker's wrap), and `mint_invite` alone cannot control that order: NIP-59 stamps
/// every gift wrap `now − random(0..2 days)`, so two production-minted wraps race on a coin
/// flip. The issuer-BOUND redeem (`Some(topic_id), Some(genuine_pk)`) must
/// SKIP the attacker's wrap and return the genuine issuer AND the genuine topic key — the KEY is
/// the actual harm, not just a wrong issuer label. The unbound control (`None, None`, fresh nonce
/// set) demonstrates the race the binding closes: over the same attacker-first inbox it returns
/// the ATTACKER's key (assertable precisely because the wrap order is pinned by construction,
/// not left to the relay). This is the row hb-core's
/// `issuer_bound_redeem_picks_the_genuine_wrap_regardless_of_order` defers to: that
/// test pins `redeem_invite` over both orders in isolation; this row pins the SKIP-ON-MISMATCH
/// loop (`fetch_invite`) over a live relay.
/// MUTATION (P-10) that reds this row: hb-net's `fetch_invite` loop —
/// `crates/hb-net/src/topic.rs:776`, the `if let Ok((meta, key, issuer)) = redeem_invite(..)`
/// iteration — changed from skip-on-error to abort-on-error (return `Ok(None)` on the FIRST
/// redeem error instead of continuing). With the attacker's wrap served first, the bound redeem
/// returns None and this row fails; every other row's inbox holds exactly one invite, so only
/// this row reds.
/// Re-wrap a production-minted invite's seal in a fresh kind-1059 gift wrap stamped with a
/// CHOSEN `created_at`. Needed because NIP-59 stamps every wrap `now − random(0..2 days)`
/// (`RANGE_RANDOM_TIMESTAMP_TWEAK`), so the inbox order of two production-minted wraps is a coin
/// flip this row cannot leave to chance — it must know which wrap `fetch_invite`'s loop sees
/// FIRST or it cannot prove skip-on-mismatch. The outer wrap is the UNAUTHENTICATED transport
/// layer — an ephemeral key by design; `redeem_invite` verifies its signature and then looks
/// THROUGH it to the seal — so re-wrapping touches none of the credentials under test: the
/// seal, rumor and payload stay byte-identical to what `mint_invite` produced (the invitee-side
/// unwrap here is exactly what `redeem_invite` itself does, just re-encrypted under a fresh
/// ephemeral key the row controls).
fn rewrap_invite(invitee: &Identity, invite: &Event, created_at: u64) -> Result<Event> {
    let seal_json = nip44::decrypt(invitee.keys().secret_key(), &invite.pubkey, &invite.content)
        .map_err(|e| anyhow!("rewrap: could not unwrap the minted invite: {e}"))?;
    let ek = Keys::generate();
    let content = nip44::encrypt(ek.secret_key(), &invitee.public_key(), &seal_json, nip44::Version::V2)
        .map_err(|e| anyhow!("rewrap: could not re-encrypt the seal: {e}"))?;
    EventBuilder::new(Kind::GiftWrap, content)
        .tags([Tag::public_key(invitee.public_key())])
        .custom_created_at(Timestamp::from(created_at))
        .sign_with_keys(&ek)
        .map_err(|e| anyhow!("rewrap: could not sign the fresh wrap: {e}"))
}

async fn topic14(ctx: &Ctx) -> Result<()> {
    let genuine = Identity::generate();
    let attacker = Identity::generate();
    let invitee = Identity::generate();
    let (meta, key) = mk_private(ctx, "iss");

    // Both mint a valid single-use invite to the SAME invitee naming the SAME topic_id. The
    // attacker's forged-but-VALID wrap carries the GENUINE `meta` (topic_id is public by the
    // time a preview names it) with the ATTACKER's own key, single-use + unexpired — so every
    // `redeem_invite` check except the issuer binding passes for it.
    let attack_key = TopicKey::generate();
    let attack_minted = mint_invite(
        &attacker, &invitee.public_key(), &meta, &attack_key, "a227",
        Some(now() + INVITE_TTL_SECS), now(),
    )?;
    let genuine_minted = mint_invite(
        &genuine, &invitee.public_key(), &meta, &key, "g227",
        Some(now() + INVITE_TTL_SECS), now(),
    )?;

    // Re-seal both with pinned wrap timestamps: the attacker's NEWER (60s), so the fetched
    // vector — nostr's `Events` collection iterates `created_at` DESCENDING — serves the
    // attacker's wrap FIRST. Neither is future-dated (strfry rejects future events at write).
    let attack_wrap = rewrap_invite(&invitee, &attack_minted, now())?;
    let genuine_wrap = rewrap_invite(&invitee, &genuine_minted, now() - 60)?;

    let ac = ctx.connect(&attacker).await?;
    publish_topic(&ac, std::slice::from_ref(&attack_wrap)).await?;
    ac.disconnect().await;
    let gc = ctx.connect(&genuine).await?;
    publish_topic(&gc, std::slice::from_ref(&genuine_wrap)).await?;
    gc.disconnect().await;
    settle().await;

    // The invitee reads its inbox RAW (the same `{kinds:[1059], #p:[me]}` shape `fetch_invite`
    // filters on; the invitee identity is fresh, so the inbox holds exactly these two wraps) to
    // pin the ordering precondition: the attacker's wrap is served FIRST. Without this check a
    // client-side ordering change would silently hollow the row out — it would pass even under
    // an abort-on-mismatch loop that never reached the genuine wrap.
    let ic = ctx.connect(&invitee).await?;
    let inbox = ic
        .fetch(Filter::new().kind(Kind::GiftWrap).pubkey(invitee.public_key()), FETCH_TIMEOUT)
        .await?;
    let pos = |id: EventId| inbox.iter().position(|e| e.id == id);
    let pa = pos(attack_wrap.id).ok_or_else(|| anyhow!("the attacker's wrap is not in the inbox"))?;
    let pg = pos(genuine_wrap.id).ok_or_else(|| anyhow!("the genuine wrap is not in the inbox"))?;
    ensure!(
        pa < pg,
        "ordering precondition unmet: the fetch served the genuine wrap first (attacker at {pa}, genuine at {pg}) — this run cannot prove skip-on-mismatch"
    );

    // The issuer-BOUND redeem (the QURATOR-227 preview→redeem shape): skips the attacker's wrap,
    // finds the genuine one behind it. Assert the returned KEY, not just the issuer.
    let (bmeta, bkey, issuer) =
        fetch_invite(&ic, &invitee, &mut NonceSet::new(), now(), FETCH_TIMEOUT, Some(&meta.topic_id), Some(&genuine.public_key()))
            .await?
            .ok_or_else(|| anyhow!("the issuer-bound redeem found no invite — did it abort on the attacker's wrap?"))?;
    ensure!(issuer == genuine.public_key(), "the bound redeem returned the ATTACKER's issuer");
    ensure!(bkey.as_bytes() == key.as_bytes(), "the bound redeem returned the attacker's topic KEY — issuer substitution succeeded");
    ensure!(bmeta.topic_id == meta.topic_id, "the bound redeem returned a different topic");

    // The unbound control over the SAME inbox: without the binding, the first wrap wins — and
    // the first wrap is the ATTACKER's, so the unbound redeem hands over the attacker's key.
    // Which-wrap is assertable here only because the wrap order is pinned by construction.
    let (_cmeta, ckey, cissuer) =
        fetch_invite(&ic, &invitee, &mut NonceSet::new(), now(), FETCH_TIMEOUT, None, None)
            .await?
            .ok_or_else(|| anyhow!("the unbound control found no invite at all"))?;
    ensure!(cissuer == attacker.public_key(), "the unbound control redeemed the wrong issuer — is the pinned order still attacker-first?");
    ensure!(ckey.as_bytes() == attack_key.as_bytes(), "the unbound control did not return the attacker's key — the race this row exists for");
    ic.disconnect().await;
    Ok(())
}

// ── TOPIC15 (QURATOR-294: join-request negative cache over a real relay) ─────────────────────────

/// TOPIC15 (QURATOR-294, hb-it half): the join-request scope of the failed-open negative cache,
/// over a real relay. A junk kind-1059 wrap `#p`-tagged to the creator (a real, relay-acceptable
/// event whose content is not a NIP-44 ciphertext under any key the creator holds — anyone who
/// knows the pubkey can put it there, since the shared `{kinds:[1059], #p:[me]}` inbox is
/// world-writable) sits alongside a genuine production `request_join` wrap, and two consecutive
/// `fetch_join_requests` polls BOTH return the real request, unchanged, with no error. The
/// join-request scope caches the FULL verdict (unwrap + parse), so the junk is remembered after
/// poll 1 and answered from the cache on poll 2 — which must be output-identical to poll 1.
///
/// ⚠ What this row can and cannot prove, said plainly: the cache SKIP itself is not observable
/// through `fetch_join_requests`' public surface — a junk wrap is skipped silently whether it
/// fails a fresh open (cache deleted) or is answered from the negative cache (cache hit), so the
/// poll assertions below would pass with the cache deleted. The observable teeth are (a) junk in
/// the polled inbox breaks nothing and errors nowhere, pinned over BOTH polls so the cache-hit
/// path of whatever build is under test cannot regress it, and (b) non-vacuousness — the raw inbox
/// is checked to hold the junk wrap BY ID, so both polls demonstrably ran past it. The skip/record
/// behaviour is pinned where it is observable, in hb-net `topic`'s unit tests
/// (`join_request_open_failures_are_negatively_cached_per_identity` and siblings).
///
/// MUTATION (P-10, for the orchestrator to apply): in `open_join_request_wraps` (hb-net
/// `crates/hb-net/src/topic.rs`), the line
///     `            continue; // failed under THIS identity + scope on a prior poll — deterministic, skip`
/// (the skip branch inside the `JOIN_REQUEST_OPEN_SCOPE` `failed_open_seen` check — line 826 when
/// this row was written, line 800 after a sibling lane's in-flight `failed_wrap_cache` refactor;
/// re-locate by the quoted text on the settled tree) — replace `continue;` with
/// `return Vec::new();` so one remembered junk wrap starves the WHOLE fetch. Poll 1 is unaffected
/// (first sight of the junk id: it attempts the open, fails, is recorded), but poll 2 hits the
/// cache, aborts, and returns 0 requests — this row reds on `got2.len() == 1`. The same edit
/// leaves TOPIC16 green (different scope, `INVITE_OPEN_SCOPE`) and every other TOPIC row green
/// (their inboxes hold no junk).
async fn topic15(ctx: &Ctx) -> Result<()> {
    let creator = Identity::generate();
    let requester = Identity::generate();
    let spammer = Identity::generate();
    let (meta, _key) = mk_private(ctx, "jr15");

    // The genuine traffic: a production `request_join` DM — the exact producer whose output
    // `fetch_join_requests` exists to parse (the harness never re-implements its body).
    let qc = ctx.connect(&requester).await?;
    request_join(&qc, &requester, &creator.public_key(), &meta.topic_id, &meta.name).await?;
    qc.disconnect().await;

    // The junk wrap: a real, relay-acceptable kind-1059 event p-tagged to the creator whose
    // content is not a NIP-44 ciphertext under any key the creator holds — it fails `unwrap_dm`
    // at decrypt. The spammer publishes it directly: a flood wrap never came from the requester.
    let junk = spammer
        .sign(
            EventBuilder::new(Kind::GiftWrap, "not-a-real-nip44-ciphertext")
                .tags([Tag::public_key(creator.public_key())])
                .custom_created_at(Timestamp::from(now())),
        )
        .map_err(|e| anyhow!("{e}"))?;
    let junk_id = junk.id;
    let sc = ctx.connect(&spammer).await?;
    publish_topic(&sc, std::slice::from_ref(&junk)).await?;
    sc.disconnect().await;
    settle().await;

    let cc = ctx.connect(&creator).await?;
    // Non-vacuousness: the inbox the creator is about to poll holds BOTH the real request wrap and
    // the junk one — without this, a silently-rejected junk publish would make both polls below
    // pass for nothing.
    let raw =
        cc.fetch(Filter::new().kind(Kind::GiftWrap).pubkey(creator.public_key()), FETCH_TIMEOUT).await?;
    ensure!(raw.len() >= 2, "expected the request wrap AND the junk wrap in the creator's inbox, got {}", raw.len());
    ensure!(raw.iter().any(|e| e.id == junk_id), "the junk wrap is in the very inbox the polls read");

    // Poll 1: the junk wrap fails unwrap + parse, is skipped + remembered; the real request arrives.
    let got1 = fetch_join_requests(&cc, &creator, FETCH_TIMEOUT).await?;
    ensure!(
        got1.len() == 1 && got1[0].0 == requester.public_key() && got1[0].1.topic_id == meta.topic_id,
        "poll 1: the junk wrap must neither hide nor corrupt the real join request, got {} request(s)",
        got1.len()
    );
    // Poll 2 — the cache-hit path in the current build: same inbox, same identity, output-identical.
    let got2 = fetch_join_requests(&cc, &creator, FETCH_TIMEOUT).await?;
    cc.disconnect().await;
    ensure!(
        got2.len() == 1 && got2[0].0 == requester.public_key() && got2[0].1.topic_id == meta.topic_id,
        "poll 2: a remembered junk wrap never swallows the real join request, got {} request(s)",
        got2.len()
    );
    Ok(())
}

// ── TOPIC16 (QURATOR-294: the invite scope's policy-refusal boundary over a real relay) ──────────

/// TOPIC16 (QURATOR-294, hb-it half): the invite scope's correctness boundary, over a real relay.
/// The invite scope deliberately caches ONLY the deterministic open verdict (`hb_core`'s
/// `open_invite` — crypto, inner-kind pin, tags, payload parse and version consistency since
/// QURATOR-298, plus the 32-byte topic_key decode since QURATOR-301; it was just unwrap + the kind
/// pin when this row was written), never the full redeem
/// verdict, because the policy half consults CALLER context — a fresh
/// `seen`, a caller `now`, and this caller's `expected_topic_id`/`expected_issuer`. The boundary:
/// one and the same wrap, p-tagged to one invitee, is refused on poll 1 because the caller expects
/// a DIFFERENT topic, and must still redeem on poll 2 when the caller expects the topic it
/// actually names. A naive full-verdict cache (record the refusal like the gate failure) breaks
/// exactly this — the wrap would be skipped on poll 2 and the join for the RIGHT topic silently
/// denied. Junk sits in the same inbox and is served FIRST (pinned below), so the row also
/// carries the PRIV8 shape for this scope: the cache-hit skip of the junk must not starve the
/// redeem queued behind it.
///
/// ⚠ What this row can and cannot prove, said plainly: it proves the OBSERVABLE semantic boundary
/// — a policy-refused wrap stays redeemable — which is precisely the property a naive cache
/// regression destroys, and the reason the two scopes have different semantics at all. It cannot
/// observe the junk wrap's cache HIT as such (skipped-on-cache and re-decrypted-and-failed look
/// identical through `fetch_invite`'s public surface), so the skip/record behaviour itself is
/// pinned in hb-net `topic`'s unit tests (`invite_policy_refusals_are_not_negatively_cached`,
/// `a_production_invite_passes_the_gate_and_is_never_negatively_cached`), not here.
/// Non-vacuousness is by-ID: the raw inbox is checked to hold BOTH the invite wrap and the junk
/// wrap before either poll, and their order is pinned (junk first) so "junk does not starve the
/// redeem" is deterministic rather than a coin flip of NIP-59 wrap timestamps.
///
/// MUTATION (P-10, for the orchestrator to apply): in `redeem_first_invite` (hb-net
/// `crates/hb-net/src/topic.rs`), the line
///     `        // Refused on POLICY grounds — NOT recorded (see this fn's doc).`
/// (the refusal tail after `redeem_invite`'s failed `if let` — line 918 when this row was
/// written, line 892 after a sibling lane's in-flight `failed_wrap_cache` refactor; re-locate by
/// the quoted text on the settled tree) — insert
/// `record_failed_open(&TOPIC_FAILED_OPENS, &me_npub, INVITE_OPEN_SCOPE, id);` beside that
/// comment (the naive full-verdict cache; pass the cache handle the surrounding code uses after
/// the refactor). Poll 1 then remembers the invite wrap under the invite scope, poll 2's
/// `failed_open_seen` skip check (the `INVITE_OPEN_SCOPE` branch at the top of the same loop)
/// skips it, `fetch_invite` returns `None`, and this row reds on the `ok_or_else`. Expect
/// TOPIC14's unbound control to redden under the SAME edit (its issuer-refused attacker wrap
/// would be skipped on the second redeem) — corroboration, not a problem. TOPIC15 stays green
/// under this edit (different scope).
async fn topic16(ctx: &Ctx) -> Result<()> {
    let issuer = Identity::generate();
    let invitee = Identity::generate();
    let spammer = Identity::generate();
    let (meta, key) = mk_private(ctx, "inv16a"); // the topic the invite names (the RIGHT one)
    let (other, _okey) = mk_private(ctx, "inv16b"); // a different private topic (the WRONG expectation)

    let minted = mint_invite(
        &issuer, &invitee.public_key(), &meta, &key, "n295",
        Some(now() + INVITE_TTL_SECS), now(),
    )?;
    // Pinned wrap timestamp 60s in the past (never future-dated — strfry rejects future events at
    // the write boundary) so the junk wrap, stamped now(), is served FIRST (nostr's `Events`
    // iterates `created_at` DESCENDING): the adversarial order for "a cached skip must not starve
    // the redeem behind it".
    let wrap = rewrap_invite(&invitee, &minted, now() - 60)?;

    let junk = spammer
        .sign(
            EventBuilder::new(Kind::GiftWrap, "not-a-real-nip44-ciphertext")
                .tags([Tag::public_key(invitee.public_key())])
                .custom_created_at(Timestamp::from(now())),
        )
        .map_err(|e| anyhow!("{e}"))?;
    let junk_id = junk.id;

    let sc = ctx.connect(&spammer).await?;
    publish_topic(&sc, std::slice::from_ref(&junk)).await?;
    sc.disconnect().await;
    let gc = ctx.connect(&issuer).await?;
    publish_topic(&gc, std::slice::from_ref(&wrap)).await?;
    gc.disconnect().await;
    settle().await;

    let ic = ctx.connect(&invitee).await?;
    // Non-vacuousness + ordering: the inbox holds BOTH wraps BY ID, junk FIRST.
    let inbox = ic
        .fetch(Filter::new().kind(Kind::GiftWrap).pubkey(invitee.public_key()), FETCH_TIMEOUT)
        .await?;
    let pos = |id: EventId| inbox.iter().position(|e| e.id == id);
    let pj = pos(junk_id).ok_or_else(|| anyhow!("the junk wrap is not in the inbox"))?;
    let pw = pos(wrap.id).ok_or_else(|| anyhow!("the invite wrap is not in the very inbox the polls read"))?;
    ensure!(
        pj < pw,
        "ordering precondition unmet: junk at {pj}, invite at {pw} — this run cannot prove a cached skip spares the redeem behind it"
    );

    // Poll 1 — bound to the WRONG topic (and the RIGHT issuer, so the topic expectation is the
    // only differing variable): the invite passes the deterministic gate, is refused on POLICY
    // grounds, and that refusal must NOT be remembered. A fresh `NonceSet` per poll keeps the
    // polls independent of single-use burn bookkeeping.
    let refused = fetch_invite(
        &ic, &invitee, &mut NonceSet::new(), now(), FETCH_TIMEOUT,
        Some(&other.topic_id), Some(&issuer.public_key()),
    )
    .await?;
    ensure!(refused.is_none(), "an invite naming topic A redeemed while topic B was expected");

    // Poll 2 — SAME wrap, SAME identity, fresh NonceSet, bound to the RIGHT topic: it must still
    // redeem, past the junk the cache now answers for.
    let (bmeta, bkey, bissuer) = fetch_invite(
        &ic, &invitee, &mut NonceSet::new(), now(), FETCH_TIMEOUT,
        Some(&meta.topic_id), Some(&issuer.public_key()),
    )
    .await?
    .ok_or_else(|| anyhow!("the wrap refused for topic B no longer redeems for topic A — the policy refusal WAS cached (the exact QURATOR-294 boundary)"))?;
    ensure!(bissuer == issuer.public_key(), "the right-topic redeem returned the wrong issuer");
    ensure!(bkey.as_bytes() == key.as_bytes(), "the right-topic redeem returned the wrong topic KEY");
    ensure!(bmeta.topic_id == meta.topic_id, "the right-topic redeem returned a different topic");
    ic.disconnect().await;
    Ok(())
}
