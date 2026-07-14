//! Topics commands (M11; spec §11) — create / discover / join / leave / invite / request→approve, the
//! roster + 24h channel, and the **auto-added topic contacts**. The crypto + relay flows live in
//! `hb-core::topic` / `hb-net::topic`; this layer is the Tauri seam + the local Topic store + the
//! contact auto-add.
//!
//! **INV-2 (no listing unlock) is enforced here, both layers:** joining a Topic auto-adds each member
//! as a contact flagged [`ContactSource::Topic`] **with no browse-key** ([`upsert_topic_contact`]) —
//! so a topic contact's listings stay share-code-gated (app layer), and a browse/private-fetch keyed
//! on that contact has no browse-key to use (wire layer). Joining grants awareness + npub + teaser
//! only.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use nostr::prelude::*;
use serde::Serialize;
use tauri::State;

use hb_core::topic::{
    build_announce, build_public_join, new_topic, normalized_public_name, seal_membership,
    topic_id_for_name,
};
use hb_core::{announce_cooldown_remaining, Identity};
use hb_net::{
    announce_to_topic, approve_join, discover_public_topics, fetch_announce, fetch_channel_full,
    fetch_invite, fetch_roster, join_public, join_topic, leave_topic, member_count, post_to_channel,
    publish_topic, request_join,
};

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    store::{CachedPeer, ContactSource, DataStore, StoredTopic},
};

/// A Topic I'm in, for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct TopicView {
    pub topic_id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub private: bool,
    pub joined_at: u64,
}

impl From<&StoredTopic> for TopicView {
    fn from(t: &StoredTopic) -> Self {
        Self {
            topic_id: t.meta.topic_id.clone(),
            name: t.meta.name.clone(),
            description: t.meta.description.clone(),
            tags: t.meta.tags.clone(),
            private: t.meta.private,
            joined_at: t.joined_at,
        }
    }
}

/// A discovered public Topic (non-member view): name + description + tags + a **spoofable** member
/// count. The roster identities are NOT here — those need the key (members-only).
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredTopic {
    pub topic_id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Best-effort, **spoofable** count (Decision: anyone can publish a fake membership) — present it
    /// as approximate in the UI, never authoritative.
    pub member_count_estimate: usize,
}

/// The result of the join-first lookup (devtest #11): does this public Topic name already have a
/// room? `exists: false` means no announce was found — the name is free to create. `exists: true`
/// means the Create modal should offer to **join** instead of forking a same-named-but-different room
/// (same `topic_id`, but a fresh `TopicKey::generate()` — Decision C — so a fork is cryptographically
/// real, not cosmetic).
#[derive(Debug, Clone, Serialize)]
pub struct TopicLookup {
    pub topic_id: String,
    pub name: String,
    pub exists: bool,
    /// Best-effort, **spoofable** count — same caveat as [`DiscoveredTopic::member_count_estimate`].
    /// `0` when `exists` is false.
    pub member_count_estimate: usize,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// M13 Part A — announce app wiring (Q1 owner ruling): the cooldown gate + length cap.
// ---------------------------------------------------------------------------

/// Serializes an announce's check-and-record step. A plain `std::sync::Mutex` is enough — the
/// guarded section is the synchronous cooldown check + persisted timestamp write, never held across
/// the network publish (an `.await`), so it can't deadlock a Tokio worker.
pub struct AnnounceGate(pub std::sync::Mutex<()>);

/// Hard cap on a broadcast's length, checked before the cooldown gate or any relay I/O.
const ANNOUNCE_MAX_CHARS: usize = 1024;

/// Reject an announce body over [`ANNOUNCE_MAX_CHARS`] with a clear, actionable error.
pub(crate) fn validate_announce_body(body: &str) -> Result<(), String> {
    let len = body.chars().count();
    if len > ANNOUNCE_MAX_CHARS {
        return Err(format!("Announcement is too long ({len} chars, max {ANNOUNCE_MAX_CHARS})"));
    }
    Ok(())
}

/// The pure check-and-record half of the announce cooldown burn: `Err` (naming the ready-again minute
/// count) if `topic_id` is still cooling down, else records `now` and returns the PRIOR timestamp (if
/// any) so a failed publish can restore it. Split out from [`topic_announce`] so it is directly
/// testable without a live relay client, and so [`restore_announce_cooldown`] (the undo half) can be
/// exercised on its own.
pub(crate) fn burn_announce_cooldown(
    times: &mut HashMap<String, u64>,
    topic_id: &str,
    now: u64,
) -> Result<Option<u64>, String> {
    let previous = times.get(topic_id).copied();
    let remaining = announce_cooldown_remaining(previous, now);
    if remaining > 0 {
        let mins = remaining.div_ceil(60);
        return Err(format!(
            "Announcements are limited to one per topic per 60 min — ready again in {mins} min."
        ));
    }
    times.insert(topic_id.to_string(), now);
    Ok(previous)
}

/// Undo a cooldown burn after the publish turned out to be a TOTAL failure (every relay rejected it)
/// — restores the prior timestamp, or removes the key entirely if there was none, so the failed
/// attempt does not cost the user their next announce. A partial success (at least one relay
/// accepted) is NOT run through this — the announce genuinely went out, so the burn stands.
pub(crate) fn restore_announce_cooldown(times: &mut HashMap<String, u64>, topic_id: &str, previous: Option<u64>) {
    match previous {
        Some(p) => {
            times.insert(topic_id.to_string(), p);
        }
        None => {
            times.remove(topic_id);
        }
    }
}

async fn me(identity: &SharedIdentity) -> Result<Identity, String> {
    identity
        .read()
        .await
        .as_ref()
        .map(|id| id.identity.clone())
        .ok_or_else(|| "No identity loaded. Generate a keypair first.".to_string())
}

/// **INV-2 app-layer — auto-add a topic contact with NO browse-key.** Adds (or, if absent) a
/// `CachedPeer` flagged [`ContactSource::Topic`] and `browse_key_hex: None`, so the member's listings
/// stay share-code-gated. An **existing** contact is left untouched — a manual contact keeps its
/// `Manual` badge and its browse-key (you added them deliberately); we never downgrade a manual add to
/// a topic add, nor strip a browse-key you already hold.
pub(crate) fn upsert_topic_contact(store: &DataStore, npub: &str) -> Result<(), String> {
    let hash = CachedPeer::pubkey_hash(npub);
    if store.load_contact(&hash).map_err(cmd_err)?.is_some() {
        return Ok(()); // already a contact (manual or topic) — never clobber
    }
    let peer = CachedPeer {
        npub: npub.to_string(),
        source: ContactSource::Topic,
        browse_key_hex: None, // INV-2: joining a Topic unlocks NO listings
        petname: None,
        profile: None,
        collections: vec![],
        online: false,
        last_fetched: chrono::Utc::now(),
        local_tags: vec![],
        // The §7 fingerprint is derivable from the npub alone (no listing access — INV-2 holds).
        fingerprint: hb_core::identity::parse_npub(npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk)),
    };
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// Auto-add every roster member (except me) as a topic contact.
fn auto_add_roster(store: &DataStore, roster: &[PublicKey], me_pk: &PublicKey) -> Result<(), String> {
    for pk in roster {
        if pk == me_pk {
            continue;
        }
        let npub = pk.to_bech32().map_err(cmd_err)?;
        upsert_topic_contact(store, &npub)?;
    }
    Ok(())
}

fn store_topic(store: &DataStore, t: StoredTopic) -> Result<(), String> {
    let mut topics = store.load_topics().map_err(cmd_err)?;
    if let Some(existing) = topics.iter_mut().find(|x| x.meta.topic_id == t.meta.topic_id) {
        *existing = t;
    } else {
        topics.push(t);
    }
    store.save_topics(&topics).map_err(cmd_err)
}

fn load_stored(store: &DataStore, topic_id: &str) -> Result<StoredTopic, String> {
    store
        .load_topics()
        .map_err(cmd_err)?
        .into_iter()
        .find(|t| t.meta.topic_id == topic_id)
        .ok_or_else(|| format!("You are not in topic {topic_id}"))
}

// ── commands ─────────────────────────────────────────────────────────────────────────────────────

/// List the Topics I'm in.
#[tauri::command]
pub async fn topic_list(store: State<'_, DataStore>) -> CmdResult<Vec<TopicView>> {
    Ok(store.load_topics().map_err(cmd_err)?.iter().map(TopicView::from).collect())
}

/// Create a Topic. A **public** Topic publishes an announce + a public-join credential + my membership;
/// a **private** Topic publishes only my membership (unlisted). I become its sole member.
#[tauri::command]
pub async fn topic_create(
    name: String,
    description: String,
    tags: Vec<String>,
    private: bool,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicView> {
    let me = me(&identity).await?;
    // W4: a public name is validated here (root ∈ category + depth cap — backend-authoritative); a
    // private name stays freeform. A bad public path surfaces the clear hb-core error.
    let (meta, key) = new_topic(&name, &description, tags, private).map_err(cmd_err)?;
    let t = now();

    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;

    // devtest #11 follow-up: `topic_lookup` is only a UI *preflight* — a client can look up, see
    // nothing, and still race another client that does the same before either publishes. Recheck for
    // an existing announce right before minting/publishing (same normalized seam `topic_lookup` uses)
    // so a same-name PUBLIC create started after another one already landed joins instead of forking a
    // second, cryptographically distinct room (same `topic_id`, fresh key — Decision C). This narrows
    // but cannot close the race: two clients that both check and both see nothing can still both
    // create — relays are eventually consistent, so no single check-then-act is airtight without a
    // registry. The residual is accepted (Decision C's newest-announce-wins dedup is the existing
    // fallback: `topic_lookup`/discovery converge on one announce once relays propagate).
    if !private {
        if let Some(_existing) =
            fetch_announce(&client, &meta.topic_id, net::RELAY_TIMEOUT).await.map_err(cmd_err)?
        {
            return Err("That topic already exists — joining it instead of creating a duplicate.".into());
        }
    }

    let membership = seal_membership(&key, &meta.topic_id, &me, t).map_err(cmd_err)?;
    let mut events = vec![membership.clone()];
    if !private {
        events.push(build_announce(&me, &meta, t).map_err(cmd_err)?);
        events.push(build_public_join(&me, &meta, &key, t).map_err(cmd_err)?);
    }
    publish_topic(&client, &events).await.map_err(cmd_err)?;

    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(TopicView::from(&stored))
}

/// Discover public Topics by tag (non-member view: name + description + the spoofable member count).
#[tauri::command]
pub async fn topic_discover(
    tags: Vec<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<DiscoveredTopic>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // W4: discovery is activity-ranked (member_count desc, top-N capped) inside hb-net; each entry
    // already carries its spoofable count, so no second per-topic fetch is needed here.
    let ranked = discover_public_topics(&client, &tags, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    Ok(ranked
        .into_iter()
        .map(|(m, count)| DiscoveredTopic {
            topic_id: m.topic_id,
            name: m.name,
            description: m.description,
            tags: m.tags,
            member_count_estimate: count,
        })
        .collect())
}

/// Join-first lookup (devtest #11): before minting a new **public** Topic, check whether its
/// composed name already has an announce — if so, the caller should join the existing room instead
/// of forking it (Create stays mint-only; the UI branches to `topic_join_public` on `exists`). Never
/// called for a private Topic (no announce to find).
#[tauri::command]
pub async fn topic_lookup(
    name: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicLookup> {
    let me = me(&identity).await?;
    let normalized = normalized_public_name(&name).map_err(cmd_err)?;
    let topic_id = topic_id_for_name(&normalized);
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    match fetch_announce(&client, &topic_id, net::RELAY_TIMEOUT).await.map_err(cmd_err)? {
        Some(meta) => {
            let count = member_count(&client, &topic_id, net::RELAY_TIMEOUT).await.unwrap_or(0);
            Ok(TopicLookup { topic_id, name: meta.name, exists: true, member_count_estimate: count })
        }
        None => Ok(TopicLookup { topic_id, name: normalized, exists: false, member_count_estimate: 0 }),
    }
}

/// Join a public Topic by name: obtain the key via the public-join credential, publish my membership,
/// auto-add the roster as topic contacts.
#[tauri::command]
pub async fn topic_join_public(
    name: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicView> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // The public-join credential is reusable (no expiry), so `seen` is not consumed; we still pass +
    // persist it so the single-use path shares one store. `&mut` lets redeem record atomically.
    let mut seen = store.load_topic_nonces().map_err(cmd_err)?;
    let t = now();
    let redeemed = join_public(&client, &name, &mut seen, t, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    let (meta, key) = match redeemed {
        Some(v) => v,
        None => {
            return Err("Could not find a public-join credential for that Topic — is the name right?".into());
        }
    };
    let membership = join_topic(&client, &key, &meta.topic_id, &me, t).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &meta.topic_id, &key, net::RELAY_TIMEOUT).await.unwrap_or_default();

    store.save_topic_nonces(&seen).map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(TopicView::from(&stored))
}

/// Join a private Topic by redeeming an invite addressed to me (admission path 1, redeem side).
#[tauri::command]
pub async fn topic_redeem_invite(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Option<TopicView>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // `&mut seen`: redeem_invite atomically records a single-use invite's nonce on success (Decision E);
    // we persist the set afterward so a restart can't re-accept it.
    let mut seen = store.load_topic_nonces().map_err(cmd_err)?;
    let t = now();
    let redeemed = fetch_invite(&client, &me, &mut seen, t, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    let (meta, key) = match redeemed {
        Some(v) => v,
        None => {
            return Ok(None);
        }
    };
    let membership = join_topic(&client, &key, &meta.topic_id, &me, t).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &meta.topic_id, &key, net::RELAY_TIMEOUT).await.unwrap_or_default();

    store.save_topic_nonces(&seen).map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(Some(TopicView::from(&stored)))
}

/// Request to join a private Topic, sending a join-request DM to a known member.
#[tauri::command]
pub async fn topic_request_join(
    member_npub: String,
    topic_id: String,
    name: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let member = hb_core::identity::parse_npub(&member_npub).map_err(cmd_err)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    request_join(&client, &me, &member, &topic_id, &name).await.map_err(cmd_err)
}

/// Invite a peer into a Topic I'm in (member-issued invite / approve a requester). **Any** member may
/// invite (M3). Mints a sealed, single-use, expiring invite to `invitee_npub` and publishes it.
#[tauri::command]
pub async fn topic_invite(
    topic_id: String,
    invitee_npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let invitee = hb_core::identity::parse_npub(&invitee_npub).map_err(cmd_err)?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    approve_join(&client, &me, &invitee, &stored.meta, &stored.key, now()).await.map_err(cmd_err)
}

/// Leave a Topic: NIP-09-retract my membership and drop the local Topic record. **Auto-added topic
/// contacts keep their flag** (they are not removed on leave/dissolution — spec §11).
#[tauri::command]
pub async fn topic_leave(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    if let Some(json) = &stored.membership_json {
        let membership = Event::from_json(json).map_err(cmd_err)?;
        let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
        leave_topic(&client, &stored.key, &me.public_key(), &membership, now()).await.map_err(cmd_err)?;
    }
    let topics: Vec<StoredTopic> =
        store.load_topics().map_err(cmd_err)?.into_iter().filter(|t| t.meta.topic_id != topic_id).collect();
    store.save_topics(&topics).map_err(cmd_err)
}

/// Fetch a Topic's roster (members-only) and refresh the auto-added topic contacts.
#[tauri::command]
pub async fn topic_roster(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<String>> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &topic_id, &stored.key, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    roster.iter().map(|p| p.to_bech32().map_err(cmd_err)).collect()
}

/// A decrypted channel post for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelPost {
    pub author_npub: String,
    pub body: String,
    pub ts: u64,
}

/// A decrypted member broadcast, for the UI (M13 Part A app wiring).
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementView {
    pub author_npub: String,
    pub body: String,
    pub ts: u64,
}

/// The full channel read the UI renders: posts + announcements, both **newest-first** — one relay
/// fetch serves both (`hb_net::fetch_channel_full`).
#[derive(Debug, Clone, Serialize)]
pub struct ChannelView {
    pub posts: Vec<ChannelPost>,
    pub announcements: Vec<AnnouncementView>,
}

/// Read a Topic's 24h channel — posts AND announcements (M13 Part A app wiring), both locally
/// filtered to the last 24h, both newest-first.
#[tauri::command]
pub async fn topic_channel(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<ChannelView> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let read = fetch_channel_full(&client, &topic_id, &stored.key, now(), net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;
    let posts = read
        .posts
        .into_iter()
        .map(|p| Ok(ChannelPost { author_npub: p.author.to_bech32().map_err(cmd_err)?, body: p.body, ts: p.ts }))
        .collect::<Result<Vec<_>, String>>()?;
    let announcements = read
        .announcements
        .into_iter()
        .map(|a| Ok(AnnouncementView { author_npub: a.author.to_bech32().map_err(cmd_err)?, body: a.body, ts: a.ts }))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ChannelView { posts, announcements })
}

/// Broadcast an announce to a Topic's channel (M13 Part A app wiring; owner ruling Q1) — rate-limited
/// to one per topic per 60 min. The cooldown is checked-and-burned BEFORE the relay publish (never
/// held across the `.await`, so the gate can't deadlock), and restored if the publish is a TOTAL
/// failure (every relay rejected it, including a failure to even connect) — a partial success keeps
/// the burn (the announce genuinely went out).
#[tauri::command]
pub async fn topic_announce(
    topic_id: String,
    body: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
    gate: State<'_, AnnounceGate>,
) -> CmdResult<()> {
    validate_announce_body(&body)?;
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let t = now();

    let previous = {
        let _guard = gate.0.lock().map_err(|_| "announce gate poisoned".to_string())?;
        let mut times = store.load_announce_times().map_err(cmd_err)?;
        let previous = burn_announce_cooldown(&mut times, &topic_id, t)?;
        store.save_announce_times(&times, t).map_err(cmd_err)?;
        previous
    };

    let publish_result = match net::client(&me, &store, &relay).await {
        Ok(client) => announce_to_topic(&client, &stored.key, &topic_id, &me, &body, t).await.map_err(cmd_err),
        Err(e) => Err(cmd_err(e)),
    };

    if let Err(e) = publish_result {
        let _guard = gate.0.lock().map_err(|_| "announce gate poisoned".to_string())?;
        let mut times = store.load_announce_times().map_err(cmd_err)?;
        restore_announce_cooldown(&mut times, &topic_id, previous);
        store.save_announce_times(&times, t).map_err(cmd_err)?;
        return Err(e);
    }
    Ok(())
}

/// Remaining announce cooldown for `topic_id`, in seconds (0 = ready) — drives the button state. Pure
/// local read, no relay I/O.
#[tauri::command]
pub async fn topic_announce_status(topic_id: String, store: State<'_, DataStore>) -> CmdResult<u64> {
    let times = store.load_announce_times().map_err(cmd_err)?;
    Ok(announce_cooldown_remaining(times.get(&topic_id).copied(), now()))
}

/// One joined Topic's newest member-broadcast, for the background alert poll (devtest #2). `latest_ts`
/// is the newest announcement's unix-second timestamp; the UI badges/toasts it when it's past the
/// per-topic seen watermark. Topics with no announcement in the 24h window are omitted.
#[derive(Debug, Clone, Serialize)]
pub struct TopicAnnounceSummary {
    pub topic_id: String,
    pub topic_name: String,
    pub latest_ts: u64,
}

/// devtest #2 — the background announcement poll. For every joined Topic, read its 24h channel and
/// return the newest announcement (if any) so the Topics nav badge + toast can flag the ones the user
/// hasn't seen. **Best-effort per topic**: a relay failure on one topic is skipped, never fails the
/// whole sweep (a stale badge is better than a poll that always errors). Reads only — no writes, so
/// this never burns the relay-write rate limiter.
#[tauri::command]
pub async fn topic_announcements(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<TopicAnnounceSummary>> {
    let topics = store.load_topics().map_err(cmd_err)?;
    if topics.is_empty() {
        return Ok(Vec::new());
    }
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let t = now();
    let mut out = Vec::new();
    for topic in &topics {
        let read = match fetch_channel_full(&client, &topic.meta.topic_id, &topic.key, t, net::RELAY_TIMEOUT).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Some(newest) = read.announcements.iter().max_by_key(|a| a.ts) {
            out.push(TopicAnnounceSummary {
                topic_id: topic.meta.topic_id.clone(),
                topic_name: topic.meta.name.clone(),
                latest_ts: newest.ts,
            });
        }
    }
    Ok(out)
}

/// devtest #2 — the persisted per-topic announcement-seen watermarks (topic_id → newest seen ts). Pure
/// local read; seeds the nav badge on startup so an announcement that arrived while closed still shows.
#[tauri::command]
pub async fn topic_announce_seen(
    store: State<'_, DataStore>,
) -> CmdResult<std::collections::HashMap<String, u64>> {
    store.load_announce_seen().map_err(cmd_err)
}

/// devtest #2 — mark a Topic's announcements read up to `ts` (advances the watermark, never rewinds).
/// Called when the user opens the Topic's channel in Chat, clearing that topic from the nav badge.
#[tauri::command]
pub async fn topic_announce_mark_seen(
    topic_id: String,
    ts: u64,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    store.advance_announce_seen(&topic_id, ts).map_err(cmd_err)
}

/// Post to a Topic's 24h channel.
#[tauri::command]
pub async fn topic_post(
    topic_id: String,
    body: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    post_to_channel(&client, &stored.key, &topic_id, &me, &body, now()).await.map(|_| ()).map_err(cmd_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npub_of(id: &Identity) -> String {
        id.npub()
    }

    #[test]
    fn topic_store_round_trips_incl_meta_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("video/films", "criterion", vec!["video".into()], false).unwrap();
        let t = StoredTopic { meta: meta.clone(), key, joined_at: 42, membership_json: Some("{}".into()) };
        store.save_topics(&[t]).unwrap();
        let back = store.load_topics().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].meta.topic_id, meta.topic_id);
        assert_eq!(back[0].joined_at, 42);
    }

    #[test]
    fn seen_nonce_set_persists_across_reload() {
        // Decision E: the seen-nonce set survives a restart, so an old invite can't be re-accepted.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let id = Identity::generate();
        let mut seen = store.load_topic_nonces().unwrap();
        seen.insert(hb_core::topic::invite_seen_key("topic-abc", &id.public_key()));
        store.save_topic_nonces(&seen).unwrap();
        let reloaded = store.load_topic_nonces().unwrap();
        assert!(reloaded.contains(&hb_core::topic::invite_seen_key("topic-abc", &id.public_key())));
    }

    #[test]
    fn auto_added_topic_contact_is_flagged_topic_with_no_browse_key() {
        // INV-2 (app layer): a topic contact is distinguishable (source=Topic) AND carries NO
        // browse-key — joining unlocks no listings.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let member = Identity::generate();
        upsert_topic_contact(&store, &npub_of(&member)).unwrap();
        let c = store.load_contact(&CachedPeer::pubkey_hash(&npub_of(&member))).unwrap().unwrap();
        assert_eq!(c.source, ContactSource::Topic, "auto-added contact is flagged Topic");
        assert!(c.browse_key_hex.is_none(), "a topic contact has NO browse-key (INV-2 — no listing unlock)");
    }

    #[test]
    fn upsert_never_clobbers_an_existing_manual_contact() {
        // A manual contact (with a browse-key you hold) is not downgraded to a topic add nor stripped.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let member = Identity::generate();
        let npub = npub_of(&member);
        let manual = CachedPeer {
            npub: npub.clone(),
            source: ContactSource::Manual,
            browse_key_hex: Some(hex::encode([7u8; 32])),
            petname: Some("hand-added".into()),
            profile: None,
            collections: vec![],
            online: false,
            last_fetched: chrono::Utc::now(),
            local_tags: vec![],
            fingerprint: None,
        };
        store.save_contact(&CachedPeer::pubkey_hash(&npub), &manual).unwrap();
        upsert_topic_contact(&store, &npub).unwrap();
        let c = store.load_contact(&CachedPeer::pubkey_hash(&npub)).unwrap().unwrap();
        assert_eq!(c.source, ContactSource::Manual, "an existing manual contact keeps its badge");
        assert!(c.browse_key_hex.is_some(), "and keeps its browse-key");
    }

    #[test]
    fn topic_contact_default_source_is_manual_on_old_data() {
        // A pre-M11 contact JSON (no `source`) loads as Manual.
        let json = r#"{"npub":"npub1xyz","browse_key_hex":null,"profile":null,"collections":[],"online":false,"last_fetched":"2026-06-23T00:00:00Z"}"#;
        let c: CachedPeer = serde_json::from_str(json).unwrap();
        assert_eq!(c.source, ContactSource::Manual);
    }

    // ── M13 Part A — announce app wiring (Q1) ──────────────────────────────────────────────────

    #[test]
    fn announce_body_over_cap_rejected() {
        let ok = "x".repeat(ANNOUNCE_MAX_CHARS);
        assert!(validate_announce_body(&ok).is_ok(), "exactly at the cap is fine");
        let over = "x".repeat(ANNOUNCE_MAX_CHARS + 1);
        let err = validate_announce_body(&over).unwrap_err();
        assert!(err.contains("too long"), "got: {err}");
    }

    #[test]
    fn second_announce_inside_window_rejected_with_cooldown_error() {
        let mut times = HashMap::new();
        let t0 = 1_000;
        burn_announce_cooldown(&mut times, "films", t0).unwrap();
        let err = burn_announce_cooldown(&mut times, "films", t0 + 60).unwrap_err();
        assert!(err.contains("60 min"), "the cooldown error names the window, got: {err}");
    }

    #[test]
    fn announce_cooldown_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let t0 = 1_000;
        let mut times = store.load_announce_times().unwrap();
        burn_announce_cooldown(&mut times, "films", t0).unwrap();
        store.save_announce_times(&times, t0).unwrap();

        // A fresh DataStore over the SAME dir simulates a restart.
        let restarted = DataStore::new(dir.path().to_path_buf());
        let mut reloaded = restarted.load_announce_times().unwrap();
        let err = burn_announce_cooldown(&mut reloaded, "films", t0 + 60).unwrap_err();
        assert!(err.contains("60 min"), "the cooldown survives a restart, got: {err}");
    }

    #[test]
    fn topic_leave_does_not_reset_announce_cooldown() {
        // `topic_announce` and `topic_leave` persist to two DISTINCT files (`announce_times.json` vs
        // `topics.json`) — leaving a topic can't touch the cooldown store because it never opens it.
        // (`topic_leave` itself needs a live relay client to invoke end-to-end when a membership_json
        // exists, so this asserts the effect its non-relay tail — `store.save_topics(..)` — has on the
        // SEPARATE announce store: none.)
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let mut times = HashMap::new();
        times.insert("films".to_string(), 1_000u64);
        store.save_announce_times(&times, 1_000).unwrap();

        let (meta, key) = new_topic("films", "", vec![], true).unwrap();
        store
            .save_topics(&[StoredTopic { meta: meta.clone(), key, joined_at: 0, membership_json: None }])
            .unwrap();
        let remaining: Vec<StoredTopic> =
            store.load_topics().unwrap().into_iter().filter(|t| t.meta.topic_id != meta.topic_id).collect();
        store.save_topics(&remaining).unwrap(); // topic_leave's on-disk tail

        let reloaded = store.load_announce_times().unwrap();
        assert_eq!(reloaded.get("films"), Some(&1_000), "leaving a topic must not touch the announce cooldown");
    }

    #[test]
    fn failed_publish_restores_cooldown() {
        // `topic_announce`'s network publish can't be faked without a live relay client (the wire is
        // proven in hb-it Suite Topic), so the record/restore state machine it wraps around that I/O
        // is factored into pure fns (`burn_announce_cooldown` / `restore_announce_cooldown`) and
        // exercised directly here.
        let mut times: HashMap<String, u64> = HashMap::new();
        let t = 1_000;
        let previous = burn_announce_cooldown(&mut times, "films", t).unwrap();
        assert_eq!(previous, None, "no prior announce for a fresh topic");
        assert_eq!(
            announce_cooldown_remaining(times.get("films").copied(), t),
            hb_core::ANNOUNCE_MIN_INTERVAL_SECS,
            "the cooldown is burned"
        );

        restore_announce_cooldown(&mut times, "films", previous);
        assert_eq!(
            announce_cooldown_remaining(times.get("films").copied(), t),
            0,
            "a failed (TOTAL) publish restores readiness — the burn is undone"
        );
        assert!(!times.contains_key("films"), "no prior entry existed, so restore removes the key entirely");

        // A SECOND announce (a prior successful one exists) that then fails restores the PRIOR
        // timestamp, not just an absence.
        times.insert("films".to_string(), 500);
        let t2 = 500 + hb_core::ANNOUNCE_MIN_INTERVAL_SECS;
        let previous2 = burn_announce_cooldown(&mut times, "films", t2).unwrap();
        assert_eq!(previous2, Some(500));
        restore_announce_cooldown(&mut times, "films", previous2);
        assert_eq!(times.get("films"), Some(&500), "restore reinstates the PRIOR timestamp");
    }
}
