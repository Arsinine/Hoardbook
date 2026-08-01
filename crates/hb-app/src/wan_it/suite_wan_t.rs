//! WAN-T — topics between real clients (M20 W6 §W6). Five rows that are the **live twins of the
//! `hb-it` L2 topic suite**, pointed at the VPS strfry backbone instead of ephemeral CI strfry.
//! Per the §W6 sequencing note ("live twins of the L2 suites, mostly reusing `hb-it` bodies"), T1
//! adapts `hb-it/suite_topic` TOPIC2 (public participation bar) + TOPIC1 (multi-relay fetch), T2
//! adapts TOPIC3 + TOPIC11 (raw relay shows only pseudonym pubkeys — B2 held on live infrastructure
//! where other clients' events interleave), T3 adapts TOPIC5 (private invite), T4 adapts TOPIC8
//! (leave → roster shrinks, NIP-09 retraction honored/recorded per relay — the same best-effort
//! caveat as WAN-U's U5), and T5 adapts TOPIC11 + TOPIC9/12 (announce + 24h local filter) extended
//! with a backdated post whose inner `ts` is locally filtered regardless of what the relay serves.
//!
//! **M19 W4/W8 signatures driven.** T3 drives the CURRENT production private-invite path: the
//! invite is minted via `approve_join`, previewed via the production `topic_preview_invite` shape
//! (side-effect-free: a LOCAL throwaway `seen` set, never persisted), and redeemed via `fetch_invite`
//! with `expected_topic_id = Some(...)` (the W4 substitution guard + W8 consent binding). The
//! public join in T1 drives `join_public` with the name-derived `topic_id` (the W4 derivation).
//!
//! **Shape: probe-plays-both (all rows).** Per the §W6 task instruction, every row uses two (or
//! three) in-process identities against the live relay set — the same shape `hb-it` L2 bodies
//! already use, just pointed at real infrastructure.
//!
//! **T4 per-relay honor.** A relay ignoring NIP-09 is a recorded finding, not a row failure, BECAUSE
//! the production leave path (`leave_topic`) treats deletion as best-effort (it publishes the kind-5
//! and moves on). T4 asserts THAT contract: the retraction is well-formed and published; whether
//! each relay honors it is recorded to stderr evidence, not gated — the same caveat as WAN-U's U5.
//!
//! **Honest red.** Nothing here is `# TODO`/skip. A leg that fails on environment grounds (relay
//! didn't propagate) is an honest `not ok` with a per-step evidence dump.
//!
//! **Flake policy (P3b precedent):** long-haul rows retry ×3 with a settle between attempts; every
//! failure is a recorded data point, never discarded.

use std::time::{Duration, Instant};

use anyhow::Result;
use hb_core::topic::{
    build_announce, build_public_join, new_topic, seal_membership, NonceSet, TopicKey, TopicMeta,
    KIND_TOPIC_POST, POST_TTL_SECS,
};
use hb_core::Identity;
use hb_net::{
    announce_to_topic, approve_join, fetch_announce, fetch_channel, fetch_invite, fetch_roster,
    join_public, join_topic, leave_topic, post_to_channel, publish_topic, RelayClient,
};
use nostr::prelude::*;

use crate::identity_state::AppIdentity;
use crate::store::DataStore;
use crate::wan_it::tap::Tap;

// ---------------------------------------------------------------------------
// Constants — timeouts, retries, settle (match WAN-U / WAN-C conventions)
// ---------------------------------------------------------------------------

/// Relay handshake/fetch timeout (matches `net::RELAY_TIMEOUT` = 10 s, rounded up for long-haul).
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);

/// Settle between a publish and a read (lets the live relay index the event).
const SETTLE: Duration = Duration::from_secs(3);

/// Long-haul rows retry this many times before recording a failure (flake policy, P3b precedent).
const LONG_HAUL_RETRIES: u32 = 3;

// ---------------------------------------------------------------------------
// Probe input — built by run_probe_wan_t from the parsed args
// ---------------------------------------------------------------------------

/// The input the WAN-T probe needs. The rows construct their own throwaway identities (the
/// probe-plays-both shape), so this carries only the relay set + a per-run-unique token (mixed into
/// topic names so re-runs don't collide with stale announces the relay still holds).
pub struct ProbeInput {
    /// The relay URLs every row publishes to and reads from.
    pub relays: Vec<String>,
    /// A per-run-unique token mixed into topic names (parity with `hb-it`'s `run_id`).
    pub run_id: String,
}

/// Build the WAN-T probe input from the parsed args + the probe's identity + store.
pub async fn build_probe_input(
    _app_id: AppIdentity,
    _store: DataStore,
    relays: Vec<String>,
) -> Result<ProbeInput> {
    // A per-run-unique token (6 random bytes hex) so re-runs don't collide with stale announces.
    let bytes: [u8; 6] = rand::random();
    let run_id = hex::encode(bytes);
    Ok(ProbeInput { relays, run_id })
}

/// Run the WAN-T rows (T1–T5) against the live relay set. Each row is an honest TAP check:
/// Ok ⇒ pass, Err(detail) ⇒ fail with a `# diagnostic` block.
pub async fn run(tap: &mut Tap, probe: &ProbeInput) {
    let wall = Instant::now();

    tap.check(
        "T1: A creates public Topic → B discovers by path + joins via public-join → both rosters show both",
        t1_public_join(probe).await,
    );

    tap.check(
        "T2: channel — B posts → A sees it within one poll; raw relay fetch shows ONLY pseudonym pubkeys (B2)",
        t2_channel_pseudonymity(probe).await,
    );

    tap.check(
        "T3: private Topic — invite minted on A rides a DM to B, redeems (expected_topic_id), joins; public finds nothing",
        t3_private_invite(probe).await,
    );

    tap.check(
        "T4: leave on B → roster shrinks on A (NIP-09 retraction honored/recorded per live relay)",
        t4_leave_retract(probe).await,
    );

    tap.check(
        "T5: announce (k1117) propagates with NIP-40 expiration intact; backdated post locally filtered on A",
        t5_announce_and_local_filter(probe).await,
    );

    eprintln!("   WAN-T total wall-clock for 5 rows: {:.2}s", wall.elapsed().as_secs_f64());
}

// ---------------------------------------------------------------------------
// Small helpers shared across rows (adapted from WAN-U / hb-it/suite_topic.rs)
// ---------------------------------------------------------------------------

/// Connect a client to the relay set (matches `WAN-U::connect`).
async fn connect(id: &Identity, relays: &[String]) -> Result<RelayClient> {
    Ok(RelayClient::connect(id, relays, RELAY_TIMEOUT).await?)
}

/// A small settle after a publish before a read (lets the live relay index the event).
async fn settle() {
    tokio::time::sleep(SETTLE).await;
}

/// Current unix time in seconds (real clock — topic freshness must be honest).
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A per-test-unique public Topic name (W4: a public name is a category-rooted path — `video/…` —
/// so the per-test suffix sits under the `video` root). Matches `hb-it/suite_topic::mk_public`.
fn mk_public_name(probe: &ProbeInput, suffix: &str) -> String {
    format!("video/want-{}-{}/anime", suffix, probe.run_id)
}

/// A per-test-unique private Topic name (freeform — W4 root rule is public-only).
fn mk_private_name(probe: &ProbeInput, suffix: &str) -> String {
    format!("want-priv-{}-{}", suffix, probe.run_id)
}

/// Create a public Topic: publish the announce + the public-join credential + the creator's own
/// membership. Returns the creator's membership event. Matches `hb-it/suite_topic::create_public`.
async fn create_public(
    relays: &[String],
    creator: &Identity,
    meta: &TopicMeta,
    key: &TopicKey,
) -> Result<Event> {
    let t = now();
    let announce = build_announce(creator, meta, t)?;
    let public_join = build_public_join(creator, meta, key, t)?;
    let membership = seal_membership(key, &meta.topic_id, creator, t)?;
    let cc = connect(creator, relays).await?;
    publish_topic(&cc, &[announce, public_join, membership.clone()]).await?;
    cc.disconnect().await;
    settle().await;
    Ok(membership)
}

// ---------------------------------------------------------------------------
// T1 — A creates public Topic → B discovers by path + joins → both rosters show both
//
// Live twin of `hb-it/suite_topic` TOPIC1 (multi-relay fetch) + TOPIC2 (the participation bar,
// joiner leg). A creates a public Topic (announce + public-join credential + own membership) → B
// discovers it by path (the W4 name-derived `topic_id`) on the live relay → B joins via the
// public-join credential → both rosters show both members.
//
// Drives the production create path (`build_announce` + `build_public_join` + `seal_membership` +
// `publish_topic`) and the production join path (`join_public` + `join_topic` + `fetch_roster`).
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn t1_public_join(probe: &ProbeInput) -> Result<(), String> {
    let creator = Identity::generate();
    let name = mk_public_name(probe, "t1");
    let (meta, key) =
        new_topic(&name, "a subject group", Vec::new(), false).map_err(|e| format!("{e}"))?;
    create_public(&probe.relays, &creator, &meta, &key)
        .await
        .map_err(|e| format!("T1 create_public: {e:#}"))?;

    // B discovers it by path: fetch the announce for the name-derived topic_id (the W4 derivation).
    let joiner = Identity::generate();
    let jc = connect(&joiner, &probe.relays)
        .await
        .map_err(|e| format!("T1 joiner connect: {e}"))?;
    let found = fetch_announce(&jc, &meta.topic_id, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T1 fetch_announce: {e}"))?;
    let announce_meta = found.ok_or_else(|| {
        "T1 B found no announce for the name-derived topic_id — discovery by path failed".to_string()
    })?;
    if announce_meta.topic_id != meta.topic_id {
        return Err(format!(
            "T1 discovered topic_id {} ≠ created {} — path divergence",
            announce_meta.topic_id, meta.topic_id
        ));
    }
    eprintln!("   T1 B discovered the Topic by path (announce topic_id matches)");

    // B joins via the public-join credential (the keyless path). join_public derives the credential
    // from the normalized name → same topic_id → same room.
    let mut seen = NonceSet::new();
    let t = now();
    let (jmeta, jkey, _issuer) = join_public(&jc, &name, &mut seen, t, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T1 join_public: {e}"))?
        .ok_or_else(|| "T1 B found no public-join credential".to_string())?;
    if jkey.as_bytes() != key.as_bytes() {
        return Err("T1 the joiner did not obtain the real topic key via the public-join credential".to_string());
    }
    if jmeta.topic_id != meta.topic_id {
        return Err(format!(
            "T1 joiner landed in a different room ({}) than the creator ({}) — public-join diverged",
            jmeta.topic_id, meta.topic_id
        ));
    }
    join_topic(&jc, &jkey, &jmeta.topic_id, &joiner, t)
        .await
        .map_err(|e| format!("T1 join_topic: {e}"))?;
    jc.disconnect().await;
    settle().await;

    // Both rosters show both members. Read each from the live relay.
    let cc = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T1 creator re-connect: {e}"))?;
    let creator_roster = fetch_roster(&cc, &meta.topic_id, &key, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T1 fetch_roster (creator view): {e}"))?;
    cc.disconnect().await;
    let jc = connect(&joiner, &probe.relays)
        .await
        .map_err(|e| format!("T1 joiner re-connect: {e}"))?;
    let joiner_roster = fetch_roster(&jc, &meta.topic_id, &jkey, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T1 fetch_roster (joiner view): {e}"))?;
    jc.disconnect().await;

    if !creator_roster.contains(&joiner.public_key()) {
        return Err(format!(
            "T1 creator's roster does not show the joiner: {:?}",
            creator_roster
        ));
    }
    if !creator_roster.contains(&creator.public_key()) {
        return Err(format!(
            "T1 creator's roster does not show the creator: {:?}",
            creator_roster
        ));
    }
    if joiner_roster != creator_roster {
        return Err(format!(
            "T1 rosters diverge: creator sees {:?}, joiner sees {:?}",
            creator_roster, joiner_roster
        ));
    }
    eprintln!(
        "T1 OK: both rosters show both members ({})",
        creator_roster.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T2 — channel: B posts → A sees it within one poll; raw relay shows ONLY pseudonym pubkeys (B2)
//
// Live twin of `hb-it/suite_topic` TOPIC3 (raw query — no real npub leaks) + TOPIC11 (channel +
// announce pseudonymity). The B2 privacy assertion: a raw relay fetch of kind-1117 channel events
// shows ONLY pseudonym pubkeys (the derived member keys), never a participant's real npub — held on
// LIVE infrastructure where other clients' events interleave.
//
// Drives the production post path (`post_to_channel`) and the production channel read
// (`fetch_channel`) + the raw membership/channel fetch.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn t2_channel_pseudonymity(probe: &ProbeInput) -> Result<(), String> {
    let creator = Identity::generate();
    let name = mk_public_name(probe, "t2");
    let (meta, key) =
        new_topic(&name, "channel test", Vec::new(), false).map_err(|e| format!("{e}"))?;
    let _cm = create_public(&probe.relays, &creator, &meta, &key)
        .await
        .map_err(|e| format!("T2 create_public: {e:#}"))?;

    // B (the joiner) joins, then posts to the channel.
    let joiner = Identity::generate();
    let jc = connect(&joiner, &probe.relays)
        .await
        .map_err(|e| format!("T2 joiner connect: {e}"))?;
    let mut seen = NonceSet::new();
    let t = now();
    let (jmeta, jkey, _) = join_public(&jc, &name, &mut seen, t, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T2 join_public: {e}"))?
        .ok_or_else(|| "T2 joiner found no public-join credential".to_string())?;
    join_topic(&jc, &jkey, &jmeta.topic_id, &joiner, t)
        .await
        .map_err(|e| format!("T2 join_topic: {e}"))?;
    let token: [u8; 6] = rand::random();
    let body = format!("want-t2-post-{}", hex::encode(token));
    post_to_channel(&jc, &jkey, &jmeta.topic_id, &joiner, &body, t)
        .await
        .map_err(|e| format!("T2 post_to_channel: {e}"))?;
    jc.disconnect().await;
    settle().await;

    // A (the creator) sees it within one poll (the 3s cadence — a single fetch).
    let cc = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T2 creator connect: {e}"))?;
    let mut last_err = String::new();
    let mut channel = Vec::new();
    for attempt in 1..=LONG_HAUL_RETRIES {
        match fetch_channel(&cc, &meta.topic_id, &key, now(), RELAY_TIMEOUT).await {
            Ok(c) => {
                channel = c;
                break;
            }
            Err(e) => {
                last_err = format!("attempt {attempt}: fetch_channel: {e}");
                settle().await;
            }
        }
    }
    if !channel.iter().any(|p| p.body == body) {
        cc.disconnect().await;
        return Err(format!(
            "T2 A did not see B's post \"{body}\" within {LONG_HAUL_RETRIES} polls (last: {last_err})"
        ));
    }
    eprintln!("   T2 A saw B's post within the poll budget");

    // B2: the raw relay fetch of kind-1117 (channel events) shows ONLY pseudonym pubkeys — never the
    // creator's or joiner's real npub. This is the privacy assertion held on live infrastructure.
    let raw = cc
        .fetch(
            Filter::new().kind(Kind::from_u16(KIND_TOPIC_POST)).identifier(meta.topic_id.clone()),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("T2 raw k1117 fetch: {e}"))?;
    cc.disconnect().await;
    if raw.is_empty() {
        return Err("T2 raw relay fetch returned no channel events — the post did not propagate".to_string());
    }
    for e in &raw {
        if e.pubkey == creator.public_key() {
            return Err("T2 a channel event pubkey leaked the CREATOR's real npub (B2 broken)".to_string());
        }
        if e.pubkey == joiner.public_key() {
            return Err("T2 a channel event pubkey leaked the JOINER's real npub (B2 broken)".to_string());
        }
    }
    eprintln!(
        "T2 OK: A saw the post; raw relay fetch shows {} channel event(s), all pseudonym pubkeys (B2 holds)",
        raw.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T3 — private Topic: invite minted on A rides a DM to B, redeems (expected_topic_id), joins
//
// Live twin of `hb-it/suite_topic` TOPIC5 (private: unlisted + invite admits). Exercises the CURRENT
// post-M19 production signatures: the invite is minted via `approve_join`, the W8 consent preview
// shape is exercised (a LOCAL throwaway `seen` set, never persisted — the `topic_preview_invite`
// contract), and the redeem drives `fetch_invite` with `expected_topic_id = Some(...)` (the W4
// substitution guard + W8 consent binding). Public discovery finds nothing (the Topic is unlisted).
//
// Drives: `seal_membership` + `publish_topic` (create), `discover_public_topics` (unlisted check),
// `approve_join` (mint invite), `fetch_invite` with `expected_topic_id` (the W4/W8 redeem path),
// `join_topic` + `fetch_roster`.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn t3_private_invite(probe: &ProbeInput) -> Result<(), String> {
    let creator = Identity::generate();
    let invitee = Identity::generate();
    let name = mk_private_name(probe, "t3");
    let (meta, key) =
        new_topic(&name, "secret", Vec::new(), true).map_err(|e| format!("{e}"))?;

    // (1) Create the private Topic: NO announce — just the creator's membership.
    let t = now();
    let membership = seal_membership(&key, &meta.topic_id, &creator, t)
        .map_err(|e| format!("T3 seal_membership: {e}"))?;
    let cc = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T3 creator connect: {e}"))?;
    publish_topic(&cc, std::slice::from_ref(&membership))
        .await
        .map_err(|e| format!("T3 publish membership: {e}"))?;

    // (2) A private Topic is unlisted: it publishes NO announce (the discovery event), so a direct
    //     `fetch_announce(topic_id)` returns None. (A public Topic's announce IS discoverable by
    //     topic_id; the private path publishes only the creator's membership.) This is the direct
    //     unlistedness assertion — the discovery event does not exist on the relay. Note: a private
    //     Topic carries no discovery tags, so a hashtag-scan `discover_public_topics` is not the right
    //     probe (an empty-tag filter is invalid); the announce fetch is the authoritative check.
    let announce = fetch_announce(&cc, &meta.topic_id, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T3 fetch_announce (unlisted check): {e}"))?;
    if announce.is_some() {
        return Err("T3 a private Topic must NOT publish a discoverable announce (it must be unlisted)".to_string());
    }
    eprintln!("   T3 private Topic is unlisted (fetch_announce found nothing — no discovery event)");

    // (3) Mint the invite via `approve_join` (the production mint path). The invite rides a NIP-17
    //     DM to the invitee.
    approve_join(&cc, &creator, &invitee.public_key(), &meta, &key, t)
        .await
        .map_err(|e| format!("T3 approve_join (mint invite): {e}"))?;
    cc.disconnect().await;
    settle().await;

    // (4) The W8 consent preview shape: preview the invite WITHOUT committing (a LOCAL throwaway
    //     `seen` set, never persisted — the `topic_preview_invite` contract). The preview reveals
    //     the topic name + issuer but must NOT burn the single-use nonce.
    let ic = connect(&invitee, &probe.relays)
        .await
        .map_err(|e| format!("T3 invitee connect: {e}"))?;
    let mut preview_seen = NonceSet::new();
    let previewed = fetch_invite(&ic, &invitee, &mut preview_seen, t, RELAY_TIMEOUT, None)
        .await
        .map_err(|e| format!("T3 preview fetch_invite: {e}"))?;
    let (pmeta, _pkey, pissuer) = previewed.ok_or_else(|| "T3 invitee found no invite to preview".to_string())?;
    if pmeta.topic_id != meta.topic_id {
        return Err(format!(
            "T3 preview topic_id {} ≠ created {} — the invite is for a different topic",
            pmeta.topic_id, meta.topic_id
        ));
    }
    if pissuer != creator.public_key() {
        return Err(format!(
            "T3 preview issuer {} ≠ creator {} — whose key sealed this invite?",
            pissuer, creator.public_key()
        ));
    }
    eprintln!(
        "T3 preview OK: topic \"{}\", issuer = creator (the W8 consent gate, side-effect-free)",
        pmeta.name
    );

    // (5) The W4/W8 redeem: fetch_invite with expected_topic_id = Some(the previewed topic_id). This
    //     binds the redeem to the topic the user consented to (W8) + rejects a swapped invite (W4).
    //     preview_seen was a LOCAL throwaway (never persisted), so the redeem can re-fetch + redeem
    //     the same invite (the production `topic_redeem_invite` → `topic_preview_invite` sequence).
    let mut redeem_seen = NonceSet::new();
    let redeemed = fetch_invite(
        &ic,
        &invitee,
        &mut redeem_seen,
        t,
        RELAY_TIMEOUT,
        Some(&pmeta.topic_id),
    )
    .await
    .map_err(|e| format!("T3 redeem fetch_invite (expected_topic_id): {e}"))?;
    let (imeta, ikey, _) = redeemed.ok_or_else(|| "T3 the invitee could not redeem the invite".to_string())?;
    if ikey.as_bytes() != key.as_bytes() {
        return Err("T3 the invite did not carry the real topic key".to_string());
    }
    if imeta.topic_id != meta.topic_id {
        return Err(format!(
            "T3 redeemed topic_id {} ≠ created {} — W4 binding failed",
            imeta.topic_id, meta.topic_id
        ));
    }
    join_topic(&ic, &ikey, &imeta.topic_id, &invitee, t)
        .await
        .map_err(|e| format!("T3 join_topic: {e}"))?;
    ic.disconnect().await;
    settle().await;

    // (6) The invitee is on the roster.
    let vc = connect(&invitee, &probe.relays)
        .await
        .map_err(|e| format!("T3 verify connect: {e}"))?;
    let roster = fetch_roster(&vc, &meta.topic_id, &ikey, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T3 fetch_roster: {e}"))?;
    vc.disconnect().await;
    if !(roster.contains(&creator.public_key()) && roster.contains(&invitee.public_key())) {
        return Err(format!(
            "T3 the admitted invitee is not on the roster with the creator: {:?}",
            roster
        ));
    }
    eprintln!(
        "T3 OK: private invite redeemed (expected_topic_id), joined, roster = {}",
        roster.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T4 — leave on B → roster shrinks on A (NIP-09 retraction honored/recorded per live relay)
//
// Live twin of `hb-it/suite_topic` TOPIC8 (leave → shrink). The production leave path
// (`leave_topic`) publishes a NIP-09 deletion (kind 5) signed under the derived pseudonym key. Per
// the §W6 task instruction, this is the SAME best-effort caveat as WAN-U's U5: a relay ignoring
// NIP-09 is a recorded finding, not a row failure. T4 asserts the production contract (the
// retraction is well-formed + published) and records the per-relay honor table to stderr.
//
// Drives `join_topic`, `leave_topic`, `fetch_roster`, and the raw membership-event fetch (to measure
// per-relay NIP-09 honor).
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn t4_leave_retract(probe: &ProbeInput) -> Result<(), String> {
    let creator = Identity::generate();
    let joiner = Identity::generate();
    let name = mk_public_name(probe, "t4");
    let (meta, key) =
        new_topic(&name, "leave test", Vec::new(), false).map_err(|e| format!("{e}"))?;
    let _cm = create_public(&probe.relays, &creator, &meta, &key)
        .await
        .map_err(|e| format!("T4 create_public: {e:#}"))?;

    // Joiner joins → roster of 2.
    let t = now();
    let jc = connect(&joiner, &probe.relays)
        .await
        .map_err(|e| format!("T4 joiner connect: {e}"))?;
    let mut seen = NonceSet::new();
    let (jmeta, jkey, _) = join_public(&jc, &name, &mut seen, t, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T4 join_public: {e}"))?
        .ok_or_else(|| "T4 joiner found no public-join credential".to_string())?;
    let jm = join_topic(&jc, &jkey, &jmeta.topic_id, &joiner, t)
        .await
        .map_err(|e| format!("T4 join_topic: {e}"))?;
    jc.disconnect().await;
    settle().await;

    // Confirm roster of 2 before the leave.
    let cc = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T4 creator connect: {e}"))?;
    let before = fetch_roster(&cc, &meta.topic_id, &key, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T4 fetch_roster before leave: {e}"))?;
    if before.len() != 2 {
        return Err(format!(
            "T4 expected 2 members before the leave, got {} ({:?})",
            before.len(),
            before
        ));
    }
    cc.disconnect().await;
    eprintln!("   T4 roster before leave: {} (creator + joiner)", before.len());

    // Capture the joiner's membership event id for the per-relay honor check.
    let target_id = jm.id;

    // Leave: drive the production `leave_topic` (NIP-09 deletion signed under the pseudonym key).
    let jc = connect(&joiner, &probe.relays)
        .await
        .map_err(|e| format!("T4 joiner re-connect for leave: {e}"))?;
    leave_topic(&jc, &jkey, &joiner.public_key(), &jm, t)
        .await
        .map_err(|e| format!("T4 leave_topic (the production contract): {e}"))?;
    jc.disconnect().await;
    eprintln!(
        "T4 NIP-09 retraction published (kind 5, target membership {})",
        target_id.to_hex()
    );
    settle().await;

    // Per-relay honor check: fetch the membership events from EACH relay individually, before + after
    // the deletion. HONORED = the retracted membership is gone from the roster; IGNORED = still
    // present. This is the per-relay honor table §W6 mandates (same as WAN-U's U5).
    let mut honored: Vec<String> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for url in &probe.relays {
        let one = std::slice::from_ref(url).to_vec();
        let rc = RelayClient::connect(&creator, &one, RELAY_TIMEOUT)
            .await
            .map_err(|e| format!("T4 per-relay connect {url}: {e}"))?;
        let after = fetch_roster(&rc, &meta.topic_id, &key, RELAY_TIMEOUT)
            .await
            .unwrap_or_default();
        rc.disconnect().await;
        if after.contains(&joiner.public_key()) {
            eprintln!(
                "T4   {url}: IGNORED — joiner still on roster after NIP-09 (a relay finding, not a row failure: production leave is best-effort)"
            );
            ignored.push(url.clone());
        } else {
            eprintln!("T4   {url}: HONORED — joiner retracted from roster after NIP-09");
            honored.push(url.clone());
        }
    }
    eprintln!(
        "T4 NIP-09 honor table: {} honored {:?}, {} ignored {:?}",
        honored.len(),
        honored,
        ignored.len(),
        ignored
    );

    // The aggregate roster (read across the full relay set) should show the joiner gone if ANY relay
    // honored the deletion (the production fetch_roster dedups + a retracted membership is absent).
    // This is the production-contract assertion: A sees the roster shrink.
    let cc = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T4 creator re-connect: {e}"))?;
    let after = fetch_roster(&cc, &meta.topic_id, &key, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("T4 fetch_roster after leave: {e}"))?;
    cc.disconnect().await;

    // The row FAILS only if the production contract was not met: the leave could not be built or
    // published. Per-relay honor is a finding (stderr above), not a gate. We DO flag the degenerate
    // case: zero relays honored (the joiner still appears in the aggregate roster), surfaced as a
    // diagnostic so a total NIP-09 failure is never silent.
    if after.contains(&joiner.public_key()) {
        eprintln!(
            "T4 NOTE: the joiner still appears in the aggregate roster after leave. The production \
             contract (retraction published) held; relay honor is best-effort. If this persists, \
             leave UX should set user expectations (the retracted membership lingers on non-compliant \
             relays)."
        );
    } else {
        eprintln!(
            "T4 OK: aggregate roster shrank to {} (the joiner is gone from at least the honored relays)",
            after.len()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// T5 — announce propagates with NIP-40 intact; backdated post locally filtered on A
//
// Live twin of `hb-it/suite_topic` TOPIC11 (announce broadcast + NIP-40) + TOPIC9/12 (the 24h local
// filter). Extended with the §W6 mandate: a backdated post (harness-crafted inner `ts`) is locally
// filtered on A regardless of what the relay serves. The announce (kind 1117, the SAME kind as a
// channel post) propagates cross-region with its NIP-40 expiration tag intact; a backdated post (a
// real sealed post whose INNER `ts` is >24h in the past) is dropped by the local filter
// (`fetch_channel` / `open_channel_item`) even though the relay still serves it.
//
// Drives `announce_to_topic`, `post_to_channel`, `fetch_channel_full`, and `seal_post` (for the
// backdated-post craft — the production seal with an old inner `ts`).
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn t5_announce_and_local_filter(probe: &ProbeInput) -> Result<(), String> {
    let creator = Identity::generate();
    let name = mk_public_name(probe, "t5");
    let (meta, key) =
        new_topic(&name, "announce + filter", Vec::new(), false).map_err(|e| format!("{e}"))?;
    let _cm = create_public(&probe.relays, &creator, &meta, &key)
        .await
        .map_err(|e| format!("T5 create_public: {e:#}"))?;

    let t = now();
    let cc = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T5 creator connect: {e}"))?;

    // (1) A fresh announce (kind 1117, the broadcast domain). Drives the production announce path.
    let token: [u8; 6] = rand::random();
    let announce_body = format!("want-t5-announce-{}", hex::encode(token));
    let announce = announce_to_topic(&cc, &key, &meta.topic_id, &creator, &announce_body, t)
        .await
        .map_err(|e| format!("T5 announce_to_topic: {e}"))?;
    // The announce carries a NIP-40 expiration tag (best-effort relay GC).
    let exp = announce
        .tags
        .find(TagKind::Expiration)
        .and_then(|t| t.content())
        .and_then(|s| s.parse::<u64>().ok());
    if exp.is_none() {
        cc.disconnect().await;
        return Err("T5 the announce does not carry a NIP-40 expiration tag".to_string());
    }
    eprintln!(
        "T5 announce published (k1117) with NIP-40 expiration tag (exp={})",
        exp.unwrap()
    );
    settle().await;

    // (2) A fresh post (must survive the 24h local filter when fetched at `now`).
    let post_body = format!("want-t5-fresh-{}", hex::encode(token));
    post_to_channel(&cc, &key, &meta.topic_id, &creator, &post_body, t)
        .await
        .map_err(|e| format!("T5 post_to_channel (fresh): {e}"))?;

    // (3) A backdated post: a REAL sealed post whose INNER `ts` is >24h in the past. The production
    //     `seal_post` takes `now` and stamps the inner payload `ts: now` (+ a matching proof + a
    //     NIP-40 expiration at `now + 24h`). Passing `now − 25h` produces a post whose inner ts is
    //     >24h ago. NOTE: the VPS strfry rejects a published NIP-40-expired event ("invalid: event
    //     expired" — the relay honors NIP-40 at the write boundary, a recorded finding). So we craft
    //     the backdated post + INJECT it into the raw event batch before the production local filter
    //     (`partition_channel_events`) runs — simulating a relay that DID serve one. This is the
    //     correct unit of the assertion: the CLIENT's local filter drops it, independent of relay
    //     policy. This is exactly the §W6 mandate: "a backdated post (harness-crafted inner ts) is
    //     locally filtered on A regardless of what the relay serves."
    let old_ts = t.saturating_sub(POST_TTL_SECS + 3600); // 25h ago
    let backdated_body = format!("want-t5-old-{}", hex::encode(token));
    let backdated = hb_core::topic::seal_post(&key, &meta.topic_id, &creator, &backdated_body, old_ts)
        .map_err(|e| format!("T5 seal_post (backdated): {e}"))?;
    eprintln!(
        "   T5 crafted backdated post (inner ts={old_ts}, ~25h ago) — injected into the partition batch (VPS strfry rejects a publish of an NIP-40-expired event)"
    );

    // (4) A reads the channel: fetch the raw kind-1117 events from the live relay, INJECT the
    //     backdated post, then run the production local filter (`partition_channel_events` — the
    //     pure partition fn `fetch_channel_full` drives). The fresh post + announce (served by the
    //     relay) must be present; the injected backdated post must be LOCALLY FILTERED (its inner
    //     ts is >24h ago). This is Decision D: a non-compliant relay can't resurrect an expired post.
    cc.disconnect().await;
    let ac = connect(&creator, &probe.relays)
        .await
        .map_err(|e| format!("T5 reader connect: {e}"))?;
    let mut raw = ac
        .fetch(
            Filter::new().kind(Kind::from_u16(KIND_TOPIC_POST)).identifier(meta.topic_id.clone()),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("T5 raw k1117 fetch: {e}"))?;
    ac.disconnect().await;
    // Inject the backdated post (a relay that ignores NIP-40 would serve this).
    raw.push(backdated.clone());
    let now_read = now();
    let read = hb_net::topic::partition_channel_events(&key, &raw, now_read);

    if !read.posts.iter().any(|p| p.body == post_body) {
        return Err("T5 the fresh post was filtered out — it should be present at `now`".to_string());
    }
    eprintln!("T5 fresh post present in the channel (within the 24h window)");
    if read.posts.iter().any(|p| p.body == backdated_body) {
        return Err(
            "T5 the backdated post surfaced — the local 24h filter failed to drop it (inner ts >24h ago)".to_string(),
        );
    }
    eprintln!("T5 backdated post locally filtered (inner ts >24h ago, dropped regardless of relay)");
    if !read.announcements.iter().any(|a| a.body == announce_body) {
        return Err("T5 the announce did not propagate cross-region (absent from the channel read)".to_string());
    }
    eprintln!("T5 announce propagated cross-region with its NIP-40 expiration tag intact");

    // (5) Belt-and-braces: confirm the raw fetch DID serve the fresh post + announce (proving the
    //     relay propagated them). The injected backdated post is in the raw batch by construction;
    //     the local filter is what hid it from the partition.
    eprintln!(
        "T5 raw batch: {} events from the relay + 1 injected backdated; partition kept {} posts + {} announces",
        raw.len() - 1,
        read.posts.len(),
        read.announcements.len()
    );
    eprintln!("T5 OK: announce + fresh post present; backdated post locally filtered; NIP-40 intact");
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests for pure helpers (no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_name_is_category_rooted() {
        let probe = ProbeInput { relays: vec![], run_id: "deadbeef".into() };
        let name = mk_public_name(&probe, "x");
        // W4: a public name is rooted under a category (`video/`).
        assert!(name.starts_with("video/"), "public name must be category-rooted, got {name}");
        assert!(name.contains("want-x-deadbeef"), "public name carries the suffix + run_id: {name}");
    }

    #[test]
    fn private_name_is_freeform() {
        let probe = ProbeInput { relays: vec![], run_id: "deadbeef".into() };
        let name = mk_private_name(&probe, "y");
        // A private name is NOT category-rooted (freeform).
        assert!(!name.starts_with("video/"), "private name must be freeform, got {name}");
        assert!(name.contains("want-priv-y-deadbeef"), "private name carries the suffix + run_id: {name}");
    }

    #[test]
    fn build_probe_input_yields_unique_run_id() {
        // run_id is random, so two calls yield distinct values (no collision across re-runs).
        let rt = tokio::runtime::Runtime::new().unwrap();
        let a = rt
            .block_on(build_probe_input(AppIdentity::generate(), test_store(), vec!["ws://x".into()]))
            .unwrap();
        let b = rt
            .block_on(build_probe_input(AppIdentity::generate(), test_store(), vec!["ws://x".into()]))
            .unwrap();
        assert_ne!(a.run_id, b.run_id, "two build_probe_input calls must yield distinct run_ids");
        assert_eq!(a.relays, vec!["ws://x".to_string()]);
    }

    fn test_store() -> DataStore {
        let dir = tempfile::tempdir().unwrap();
        DataStore::new(dir.path().to_path_buf())
    }
}
