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

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use nostr::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::State;

use hb_core::topic::{
    build_announce, build_public_join, new_topic, normalized_public_name, seal_membership,
    topic_id_for_name, TopicMeta,
};
use hb_core::{announce_cooldown_remaining, Identity};
use hb_net::{
    announce_to_topic, approve_join, discover_public_topics, discover_public_topics_paint,
    fetch_announce, fetch_channel_full, fetch_invite, fetch_roster, join_public, join_topic,
    leave_topic, member_count, post_to_channel, publish_topic, rank_discovered_topics,
    request_join, TopicDiscoveries,
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
    /// as approximate in the UI, never authoritative. `None` on the W1 paint path: the count has not
    /// been fetched yet (ranking is lazy) — the UI orders by it but must not display a missing count.
    pub member_count_estimate: Option<usize>,
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

/// A **side-effect-free** preview of a pending private-Topic invite, for the consent gate (W8): the UI
/// shows who is vouching (`issuer_npub`) + the topic name BEFORE committing the redeem/join. The
/// follow-up `topic_redeem_invite` re-fetches and redeems the same invite (no nonce is burned here).
#[derive(Debug, Clone, Serialize)]
pub struct TopicInvitePreview {
    pub topic_id: String,
    pub name: String,
    pub description: String,
    /// The invite ISSUER's npub (bech32) — whose key sealed the invite = who is vouching for the join.
    pub issuer_npub: String,
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
        listings_state: Default::default(), // QURATOR-134 tri-state (not classified on this stub path)
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None, // W5.2: stamped by the online poll only
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

/// The discovery `#t` tags an announce carries (devtest v0.12.1 #6/#7). Topics no longer carry **user
/// tags** — a public Topic's name is descriptive enough — so a public Topic's sole discovery tag is
/// its **root category** (the first path segment, e.g. `video`); that lets Discover-by-primitive
/// (`topic_discover([root])`) enumerate every public Topic under a category with no tag search. A
/// private Topic is unlisted, so it carries none. Pure, so the "root-only, no user tags" rule is
/// unit-tested without a relay.
pub(crate) fn discovery_tags(name: &str, private: bool) -> Vec<String> {
    if private {
        return Vec::new();
    }
    hb_core::topic::topic_root(name).map(|r| vec![r.to_string()]).unwrap_or_default()
}

// ── commands ─────────────────────────────────────────────────────────────────────────────────────

/// List the Topics I'm in.
#[tauri::command]
pub async fn topic_list(store: State<'_, DataStore>) -> CmdResult<Vec<TopicView>> {
    Ok(store.load_topics().map_err(cmd_err)?.iter().map(TopicView::from).collect())
}

/// Create a Topic. A **public** Topic publishes an announce + a public-join credential + my membership;
/// a **private** Topic publishes only my membership (unlisted). I become its sole member.
///
/// devtest v0.12.1 #6: a Topic carries **no user tags** — the name is descriptive enough. A public
/// Topic's **root category** is stamped as its sole discovery tag ([`discovery_tags`]) so
/// Discover-by-primitive (#7) can list every public Topic under a category.
#[tauri::command]
pub async fn topic_create(
    name: String,
    description: String,
    private: bool,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicView> {
    let me = me(&identity).await?;
    // W4: a public name is validated here (root ∈ category + depth cap — backend-authoritative); a
    // private name stays freeform. A bad public path surfaces the clear hb-core error.
    let (mut meta, key) = new_topic(&name, &description, Vec::new(), private).map_err(cmd_err)?;
    // #6/#7: root category is the only discovery tag (new_topic already validated the public root).
    meta.tags = discovery_tags(&meta.name, private);
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

/// Edit a Topic's description after it has been created (devtest v0.12.1 #8). The **name is immutable**
/// — a public Topic's `topic_id` is derived from its name, so renaming would fork the room; only the
/// description is editable. A **public** Topic re-announces (same `topic_id`, newest-announce-wins) so
/// discovery reflects the new blurb; a **private** Topic just updates its local record (nothing is
/// published). The root discovery tag is re-derived, never dropped.
#[tauri::command]
pub async fn topic_update_meta(
    topic_id: String,
    description: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicView> {
    let me = me(&identity).await?;
    let mut stored = load_stored(&store, &topic_id)?;
    stored.meta.description = description;
    stored.meta.tags = discovery_tags(&stored.meta.name, stored.meta.private);
    if !stored.meta.private {
        let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
        // Stamp the re-announce strictly newer than any prior announce for this topic, so newest-wins
        // dedup (`prev >= ts` keeps the existing on a tie) actually supersedes even on a same-second
        // edit right after create (codex review). `joined_at` is this topic's create second; `+1`
        // guarantees a later stamp. (`created_at` is second-resolution on the wire.)
        let t = now().max(stored.joined_at + 1);
        let announce = build_announce(&me, &stored.meta, t).map_err(cmd_err)?;
        publish_topic(&client, &[announce]).await.map_err(cmd_err)?;
    }
    store_topic(&store, stored.clone())?;
    Ok(TopicView::from(&stored))
}

/// Discover public Topics by tag (non-member view: name + description + the spoofable member count).
/// The ONE-SHOT path (paint + rank in one call); the Topics page's Discover accordion still uses it
/// for a single expanded root, where the member-count wave behind the fetch is the status quo.
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
            member_count_estimate: Some(count),
        })
        .collect())
}

/// The PAINT half of discovery (QURATOR-143 W1): every public Topic under `tags` in **one relay
/// read** (all roots ride one `#t` OR-filter), with **zero `member_count` round trips** — each entry
/// carries `member_count_estimate: None` and the UI paints immediately. The lazy ranking half is
/// [`topic_rank`], which the caller runs after first render for the rows it will actually draw.
///
/// The starved-root escalation (hb-net) is already folded in: a junk-announce flood under one root
/// that evicts every other root from the shared-`limit` response pays one follow-up read per starved
/// root, only when the response actually hit its limit.
#[tauri::command]
pub async fn topic_discover_paint(
    tags: Vec<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<DiscoveredTopic>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let found: TopicDiscoveries =
        discover_public_topics_paint(&client, &tags, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    Ok(found
        .topics
        .into_iter()
        .map(|m| DiscoveredTopic {
            topic_id: m.topic_id,
            name: m.name,
            description: m.description,
            tags: m.tags,
            member_count_estimate: None,
        })
        .collect())
}

/// The LAZY RANKING half of discovery (QURATOR-143 W1): fetch the spoofable `member_count` for each
/// named `topic_id` (bounded to `TOPIC_DISCOVERY_CONCURRENCY` inside hb-net) and return
/// `(topic_id, count)` pairs, count-desc. The caller sends ONLY the rows it will actually draw —
/// bounding the wave to what is on screen is the caller's half of the relay-citizenship contract;
/// hb-net bounds the concurrency. The round-robin across roots is likewise the caller's: interleave
/// the ids so no root drains another's slots.
///
/// QURATOR-148 (owner ruling 2026-08-31): each row now also carries `alive_count` — how many roster
/// members pinged within the last 30 days. **`alive_count` gates discovery-sidebar visibility**: the
/// UI drops an un-joined public row whose Topic has no member alive in 30 days (it is not worth
/// joining, so it is not an option in the left-pane directory). `alive_count: None` means unknown
/// (not a member — the roster needs the key — or the read failed); unknown keeps the row, exactly as
/// an unknown member count never rendered as a confident "0". A member's OWN topic uses the stored
/// key; a non-member's read recovers it read-only via the reusable public-join credential — which is
/// why each row carries the topic NAME alongside its id (the name derives the credential keypair).
#[tauri::command]
pub async fn topic_rank(
    topics: Vec<TopicRankRequest>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<TopicRank>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let found = TopicDiscoveries {
        topics: topics
            .into_iter()
            .map(|t| TopicMeta { topic_id: t.topic_id, name: t.name, description: String::new(), tags: Vec::new(), private: false })
            .collect(),
        root_event_counts: BTreeMap::new(),
        hit_limit: false,
    };
    let ranked = rank_discovered_topics(&client, found, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    let mut out = Vec::with_capacity(ranked.len());
    for (m, count) in ranked {
        let topic_id = m.topic_id.clone();
        let alive = alive_count_for(&client, &store, &topic_id, &m.name).await;
        out.push(TopicRank { topic_id, member_count_estimate: count, alive_count: alive });
    }
    Ok(out)
}

/// One `topic_rank` request row (QURATOR-148): the id names WHICH topic to rank; the name is what a
/// non-member's aliveness read derives the public-join credential keypair from ("the name IS the
/// password", topic.rs Decision A). An empty name simply skips recovery (aliveness stays unknown).
#[derive(Debug, Deserialize)]
pub struct TopicRankRequest {
    pub topic_id: String,
    pub name: String,
}

/// The key half of aliveness (QURATOR-148): which stored topic, if any, supplies the roster key for
/// `topic_id`. `None` = aliveness unknown — either the user is not a member (no stored key), or the
/// topic is private (a private Topic keeps the pseudonym; the key is a genuine crypto bar, out of
/// scope by ruling, and no public-join credential exists to recover one with). Pure, so the None
/// arms are directly testable; the member arm proceeds to [`alive_count_for`]'s relay read.
/// A miss here is not the end of the road: for a topic ABSENT from the store,
/// [`public_recovery_allowed`] decides whether the non-member recovery path may try the
/// name-derived public-join credential instead.
fn alive_key_for(store: &DataStore, topic_id: &str) -> Option<hb_core::topic::TopicKey> {
    let stored = store.load_topics().ok()?.into_iter().find(|t| t.meta.topic_id == topic_id)?;
    if stored.meta.private {
        return None;
    }
    Some(stored.key)
}

/// The non-member recovery gate (QURATOR-148, the owed half): may [`alive_count_for`] try to
/// recover `topic_id`'s key via the name-derived public-join credential? Pure, so each refusal arm
/// is directly testable. Recovery is allowed only when BOTH hold:
/// - the name is non-empty — an empty name cannot derive the credential keypair, and `topic_rank`'s
///   older callers sent no name at all;
/// - the topic is NOT in the local store, in any form. A stored topic already had its chance in
///   [`alive_key_for`]; if that returned None the topic is PRIVATE, and recovery must not be a
///   bypass around the private bar (it would fail on the relay anyway — no public-join credential
///   exists — but the refusal belongs here, before any relay I/O). A store read failure counts as
///   "possibly stored": refuse, aliveness stays unknown, the row stays.
fn public_recovery_allowed(store: &DataStore, topic_id: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    match store.load_topics() {
        Ok(topics) => !topics.iter().any(|t| t.meta.topic_id == topic_id),
        Err(_) => false,
    }
}

/// Recover a public Topic's key WITHOUT joining (QURATOR-148): derive the public-join identity from
/// the name and redeem the reusable credential — exactly [`join_public`]'s read path, with a scratch
/// `NonceSet` (the public-join credential is nonce-exempt, so nothing is consumed and nothing is
/// persisted) and **no membership publish**. `join_public` binds the expected topic_id derived from
/// the name into the redeem (W4), and the final id check refuses a UI-supplied (id, name) pair that
/// disagrees — a recovered key must never be used to read a DIFFERENT topic's roster.
async fn recover_public_topic_key(
    client: &std::sync::Arc<hb_net::RelayClient>,
    topic_id: &str,
    name: &str,
) -> Option<hb_core::topic::TopicKey> {
    let mut scratch = hb_core::topic::NonceSet::new();
    let (meta, key, _issuer) =
        join_public(client, name, &mut scratch, now(), net::RELAY_TIMEOUT).await.ok()??;
    (meta.topic_id == topic_id).then_some(key)
}

/// The aliveness half of one rank row (QURATOR-148): the stored key when the user is a member, else
/// — for an un-stored public topic with a known name — the key recovered read-only via the
/// public-join credential. Best-effort: any failure — no key, a private topic, a relay error — is
/// `None` (aliveness unknown ⇒ the UI keeps the row), never `Some(0)` (a confident "dead" drop of a
/// Topic we simply could not read) and never an error that would take the whole ranking down with it.
async fn alive_count_for(
    client: &std::sync::Arc<hb_net::RelayClient>,
    store: &DataStore,
    topic_id: &str,
    name: &str,
) -> Option<usize> {
    let key = match alive_key_for(store, topic_id) {
        Some(k) => k,
        None => {
            if !public_recovery_allowed(store, topic_id, name) {
                return None;
            }
            recover_public_topic_key(client, topic_id, name).await?
        }
    };
    hb_net::topic::alive_member_count(
        client,
        topic_id,
        &key,
        hb_net::count::TOPIC_ALIVE_WINDOW_SECS,
        net::RELAY_TIMEOUT,
    )
    .await
    .ok()
}

/// One lazy-ranking result: a `topic_id` + its spoofable count (see [`topic_rank`]).
#[derive(Debug, Clone, Serialize)]
pub struct TopicRank {
    pub topic_id: String,
    pub member_count_estimate: usize,
    /// Members whose newest presence beacon is within 30 days (QURATOR-148) — gates sidebar
    /// visibility, unlike `member_count_estimate` which only orders. `None` = unknown (no key / the
    /// read failed): keep the row, never a confident drop.
    pub alive_count: Option<usize>,
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
    let (mut meta, key, _issuer) = match redeemed {
        Some(v) => v,
        None => {
            return Err("Could not find a public-join credential for that Topic — is the name right?".into());
        }
    };
    // The reusable public-join credential embeds the meta captured when the Topic was created; a later
    // description edit (`topic_update_meta`) re-announces but does NOT rewrite that credential — and
    // `join_public` returns the FIRST redeemable credential, not the newest. So take the CURRENT
    // description from the authoritative newest announce (replaceable, newest-wins) rather than the
    // possibly-stale credential (codex review HIGH). Best-effort: a fetch miss keeps the credential's.
    if let Ok(Some(current)) = fetch_announce(&client, &meta.topic_id, net::RELAY_TIMEOUT).await {
        meta.description = current.description;
    }
    let membership = join_topic(&client, &key, &meta.topic_id, &me, t).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &meta.topic_id, &key, net::RELAY_TIMEOUT).await.unwrap_or_default();

    store.save_topic_nonces(&seen).map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(TopicView::from(&stored))
}

/// Join a private Topic by redeeming an invite addressed to me (admission path 1, redeem side). The
/// `expected_topic_id` binds the redeem to the topic the user consented to in the W8 preview
/// (`topic_preview_invite`): a relay that swaps in a different valid invite at redeem is rejected by
/// `fetch_invite`'s existing topic_id check (reusing the public-join W4 substitution guard).
#[tauri::command]
pub async fn topic_redeem_invite(
    expected_topic_id: String,
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
    let redeemed = fetch_invite(&client, &me, &mut seen, t, net::RELAY_TIMEOUT, Some(&expected_topic_id))
        .await
        .map_err(cmd_err)?;
    let (meta, key, _) = match redeemed {
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

/// Preview a pending private-Topic invite **without committing** (W8 consent gate). Reveals the topic
/// name/description + the invite ISSUER's npub so the UI can ask for explicit acknowledgment BEFORE the
/// redeem/join/auto-add-roster. Crucially side-effect-free: it loads `seen` into a LOCAL throwaway,
/// never calls `save_topic_nonces`, never joins, never auto-adds, never stores — so the follow-up
/// [`topic_redeem_invite`] can re-fetch and redeem the same invite. Returns `None` if no valid invite.
#[tauri::command]
pub async fn topic_preview_invite(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Option<TopicInvitePreview>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // LOCAL throwaway seen-set: the preview must NOT burn the single-use invite's nonce, else the
    // follow-up redeem would be rejected as a replay. Never persisted.
    let mut seen = store.load_topic_nonces().map_err(cmd_err)?;
    let t = now();
    let redeemed = fetch_invite(&client, &me, &mut seen, t, net::RELAY_TIMEOUT, None)
        .await
        .map_err(cmd_err)?;
    match redeemed {
        Some((meta, _key, issuer)) => {
            let issuer_npub = issuer.to_bech32().map_err(cmd_err)?;
            Ok(Some(TopicInvitePreview {
                topic_id: meta.topic_id,
                name: meta.name,
                description: meta.description,
                issuer_npub,
            }))
        }
        None => Ok(None),
    }
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
            listings_state: Default::default(), // QURATOR-134: fixtures predate the tri-state; Fetched is the least-wrong default
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
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
    fn public_topic_is_tagged_by_root_only_no_user_tags() {
        // devtest v0.12.1 #6/#7: a public Topic carries exactly one discovery tag — its root category
        // (so Discover-by-primitive finds it); a private Topic carries none. No user tags either way.
        assert_eq!(discovery_tags("video/films/criterion", false), vec!["video".to_string()]);
        assert_eq!(discovery_tags("audio", false), vec!["audio".to_string()]);
        assert!(discovery_tags("back room", true).is_empty(), "a private Topic carries no discovery tag");
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

    /// M21 W5 property pin: topics.rs has zero references to groups or the Private audience today —
    /// it is compliant with the owner ruling ("joining a topic must never make Private collections
    /// visible") only by accident. This pins that property. The join path (`topic_join_public` →
    /// `auto_add_roster` → `upsert_topic_contact`) writes to the contact store only; it must NOT
    /// touch `private_audience.json`. Run with the exact data mutation `upsert_topic_contact`
    /// performs so a future refactor that wires topics into the audience can't pass silently.
    #[test]
    fn joining_a_topic_does_not_enrol_anyone_in_the_private_audience() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // Seed an empty audience (absent file ⇒ empty; verify both states).
        assert!(store.load_private_audience().unwrap().is_empty());

        // Simulate the join path's data effect: auto-add a co-member as a Topic contact.
        let member = Identity::generate();
        let npub = npub_of(&member);
        upsert_topic_contact(&store, &npub).unwrap();

        // The co-member is now a contact (source = Topic)…
        let c = store.load_contact(&CachedPeer::pubkey_hash(&npub)).unwrap().unwrap();
        assert_eq!(c.source, ContactSource::Topic, "join added the co-member as a Topic contact");
        // …but is NOT in the Private audience — topic membership ≠ Private recipient (owner ruling).
        let audience = store.load_private_audience().unwrap();
        assert!(
            !audience.contains(&npub),
            "joining a topic must never enrol anyone as a Private recipient (M21 W5)"
        );
        assert!(audience.is_empty(), "the audience file is untouched by the topic-join path");
    }

    // ── QURATOR-161 slice 5 — `topic_create` + `topic_join_public` driven through the commands ───
    //
    // Call order found (must be re-verified if the bodies move):
    //
    //   topic_create:       me() → new_topic() [public-name validation] → discovery_tags() →
    //                       net::client() → fetch_announce() …
    //   topic_join_public:  me() → net::client() → join_public() → fetch_announce() …
    //
    // So the guards BEFORE the first network I/O are: the "no identity loaded" refusal (both
    // commands) and `new_topic`'s public-name validation — empty path, root ∉ category, depth > 6
    // (`topic_create` only; a private name is freeform by design). Those are pinned here.
    //
    // Everything else is OWED, recorded below with the verbatim blocker.
    mod command_guards {
        use super::*;
        use crate::identity_state::AppIdentity;
        use tauri::Manager;

        /// Mock app + managed state, with the store pointed at a deliberately dead relay — the
        /// slice-2 hermetic pass-side probe. `net::client` dials `ws://127.0.0.1:9` (closed by
        /// definition) and fails at the handshake, so an input that CLEARS a pre-client guard is
        /// proven to have passed it: the error is the connect refusal, never the guard's text.
        fn guard_app(identity_loaded: bool) -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            store
                .save_settings(&crate::store::Settings {
                    relay_urls: vec!["ws://127.0.0.1:9".into()],
                    ..Default::default()
                })
                .unwrap();
            let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(
                identity_loaded.then(AppIdentity::generate),
            ));
            app.manage(identity);
            app.manage(store);
            app.manage(net::new_shared());
            app
        }

        async fn create_via_command(
            app: &tauri::App<tauri::test::MockRuntime>,
            name: &str,
            private: bool,
        ) -> CmdResult<TopicView> {
            topic_create(
                name.to_string(),
                "slice 5".into(),
                private,
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
        }

        async fn join_via_command(
            app: &tauri::App<tauri::test::MockRuntime>,
            name: &str,
        ) -> CmdResult<TopicView> {
            topic_join_public(
                name.to_string(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
        }

        /// The three public-name rules are `topic_create`'s only input guards, and they fire inside
        /// `new_topic` — BEFORE `net::client` is built. Each is asserted on both sides: the bad
        /// name is refused with the command's own error text, and a good name CLEARS the guard and
        /// fails at the relay connect (the next statement), proving the guard was actually passed
        /// rather than never reached. A PRIVATE name skips validation by design, so the freeform
        /// side of that branch is pinned too.
        #[tokio::test]
        async fn topic_create_command_rejects_invalid_public_names_and_passes_a_valid_one() {
            let app = guard_app(true);

            // Root ∉ category ("gaming" is not one of video/audio/image/text/software/other).
            let err = create_via_command(&app, "gaming/retro", false).await.unwrap_err();
            assert!(
                err.starts_with("invalid event: a public Topic's first path segment must be a category"),
                "non-category root must be refused by the validate guard, got {err}"
            );
            assert!(err.contains("got 'gaming'"), "the refusal names the offending root, got {err}");

            // Depth cap: MAX_TOPIC_DEPTH = 6, so 7 segments is over.
            let deep = ["video", "a", "b", "c", "d", "e", "f"].join("/");
            let err = create_via_command(&app, &deep, false).await.unwrap_err();
            assert!(
                err.starts_with("invalid event: a public Topic path may be at most 6 segments deep"),
                "a 7-segment path must be refused by the depth guard, got {err}"
            );
            assert!(err.contains("got 7"), "the refusal names the actual depth, got {err}");

            // Empty-after-normalization (whitespace/stray slashes only).
            let err = create_via_command(&app, "  /  ", false).await.unwrap_err();
            assert!(
                err.ends_with("a public Topic name cannot be empty"),
                "a name that normalizes to nothing must be refused, got {err}"
            );

            // The other side of all three guards at once: a VALID 6-segment public name (exactly at
            // the depth cap) clears validation and dies at the relay connect — never in the guard.
            let at_cap = ["video", "a", "b", "c", "d", "e"].join("/");
            let err = create_via_command(&app, &at_cap, false).await.unwrap_err();
            assert!(
                err.contains("Could not connect to any relay"),
                "a valid public name must clear every name guard and fail at the connect, got {err}"
            );

            // A PRIVATE create with the same bad name is NOT validated (freeform by design) — it too
            // proceeds to the connect. Pins that `private` is the seam that skips the guard.
            let err = create_via_command(&app, "gaming/retro", true).await.unwrap_err();
            assert!(
                err.contains("Could not connect to any relay"),
                "a private name is freeform and must skip the public-name guard, got {err}"
            );
        }

        /// Both commands refuse to run with no identity loaded, before any name parsing or I/O.
        #[tokio::test]
        async fn both_commands_require_a_loaded_identity() {
            for err in [
                // topic_create (a bad name is irrelevant — the identity guard fires FIRST)
                create_via_command(&guard_app(false), "gaming/retro", false)
                    .await
                    .unwrap_err(),
                // topic_join_public
                join_via_command(&guard_app(false), "video/films").await.unwrap_err(),
            ] {
                assert_eq!(err, "No identity loaded. Generate a keypair first.");
            }
        }

        /// `topic_join_public` takes the name straight to `net::client` — the join's own
        /// `normalized_public_name` validation lives INSIDE `join_public`, downstream of the
        /// connect. So the pass-side probe is the connect refusal itself: a well-formed name
        /// proceeds past the identity guard into the client build and fails there, which is what
        /// reds under an inverted identity guard. The invalid-name probe pins the PLACEMENT: a
        /// name the join would refuse still reaches the connect today, so hoisting validation
        /// ahead of `net::client` in this command reds the second assertion. The name-validation
        /// half itself is OWED (see below).
        #[tokio::test]
        async fn topic_join_public_command_proceeds_to_the_relay_with_a_well_formed_name() {
            let app = guard_app(true);
            let err = join_via_command(&app, "video/films").await.unwrap_err();
            assert!(
                err.contains("Could not connect to any relay"),
                "a loaded identity must carry the join into net::client, got {err}"
            );

            // Placement: even a name that fails public-name rules must reach the connect (the join's
            // validation is downstream of the client build, unlike topic_create's).
            let err = join_via_command(&app, "gaming/retro").await.unwrap_err();
            assert!(
                err.contains("Could not connect to any relay"),
                "the join must NOT refuse a bad name before net::client (validation is inside join_public, downstream), got {err}"
            );
        }

        // ── OWED — guards that only fire AFTER a relay is contacted ─────────────────────────────
        //
        // topic_create — the duplicate-public-name refusal (`"That topic already exists — joining
        // it instead of creating a duplicate."`): blocker, verbatim — the guard is
        // `fetch_announce(&client, …)` on the far side of `net::client(&me, &store, &relay)`, so
        // reaching it needs a live relay serving an announce for that topic_id. There is no
        // parameter, State, or injection seam carrying a fixture announce, and extracting one is a
        // production change, which a tests-only slice must not make.
        //
        // topic_join_public — the not-found refusal (`"Could not find a public-join credential for
        // that Topic — is the name right?"`): blocker, verbatim — the guard is the `None` arm of
        // `join_public(&client, …)`, which is downstream of `net::client`; every input that would
        // distinguish it requires a relay serving a public-join credential. No seam exists to
        // inject one, and the pre-connect `normalized_public_name` refusal inside `join_public`
        // shares the same downstream position (it runs after the client is built), so it is owed
        // for the same reason.
        //
        // Notably NOT pinned here, and deliberately so: QURATOR-133 (parse_announce trusting the
        // announce's own name/id/root) is a known live production defect ruled on by the owner —
        // these tests document the current command call order only and do not pin the relabel
        // behaviour as correct.
    }

    // ── QURATOR-148 — Topic aliveness gates discovery visibility (owner ruling 2026-08-31) ──────
    //
    // The network half (30-day window, the .authors() bound, the 29/31-day boundary) is pinned in
    // hb-net/src/count.rs. What is THIS layer's to pin is the key-recovery seam: aliveness reads the
    // roster's real npubs, which need the topic key, so a Topic with no stored key must report
    // alive_count = None (UNKNOWN — the UI keeps the row), never 0 (a confident "dead" drop of a
    // Topic we simply could not read). The `topic_rank` command itself needs a live relay to reach
    // `alive_count_for` (same downstream-of-net::client blocker as every guard in this module), so
    // the pure half is exercised directly.

    #[test]
    fn aliveness_reports_unknown_not_dead_when_the_topic_key_is_unavailable() {
        // P-10 MUTATION (orchestrator): in `alive_key_for` (the containing fn), rewrite the tail as
        //   let stored = store.load_topics().ok()?.into_iter().find(|t| t.meta.topic_id == topic_id);
        //   match stored {
        //       None => Some(hb_core::topic::TopicKey::generate()),   // ← the mutation: fabricate
        //       Some(t) if t.meta.private => None,                    //   a key for an absent topic
        //       Some(t) => Some(t.key),
        //   }
        // i.e. the absent-topic arm yields a fabricated key instead of falling through as None.
        // THIS test reds (`is_none()` fails). Siblings stay green: the private arm still returns
        // None, the member arm still returns the stored key.
        // ✓ PROVEN RED 2026-09-01: mutation applied → exactly this test FAILED, siblings green →
        //   reverted.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // No stored topic at all: aliveness is unknowable, and the key seam must yield None —
        // `alive_count_for` turns that into `alive_count: None` (the UI keeps the row), never
        // `Some(0)` (a confident "dead" drop of a Topic we simply could not read).
        assert!(
            alive_key_for(&store, "no-such-topic-id").is_none(),
            "no stored key ⇒ alive_count is unknown (None), never a confident dead 0"
        );
    }

    #[test]
    fn a_private_topic_reports_aliveness_unknown_never_recovering_a_key() {
        // P-10 MUTATION (orchestrator): in `alive_key_for` (the containing fn), delete the
        //   if stored.meta.private { return None; }
        // guard — the stored PRIVATE key is then returned and `is_none()` REDS. Siblings stay
        // green (the public member test expects Some either way; the absent-topic test's store is
        // empty).
        // ✓ PROVEN RED 2026-09-01: mutation applied → exactly this test FAILED, siblings green →
        //   reverted.
        // Owner ruling: private Topics keep the pseudonym — the key is a genuine crypto bar. There
        // is no public-join credential to recover one with, so the private arm must yield None
        // BEFORE any relay I/O.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("back room", "private", vec![], true).unwrap();
        store
            .save_topics(&[StoredTopic { meta: meta.clone(), key, joined_at: 0, membership_json: None }])
            .unwrap();
        assert!(
            alive_key_for(&store, &meta.topic_id).is_none(),
            "a private Topic's aliveness is unknown — no key recovery is attempted"
        );
    }

    #[test]
    fn a_member_topic_supplies_its_stored_key_for_the_aliveness_read() {
        // P-10 MUTATION (orchestrator): in `alive_key_for` (the containing fn), change the final
        // `Some(stored.key)` to `None` — `is_some()` REDS while both siblings (which assert
        // `is_none()`) stay green, proving the three arms are pinned independently.
        // ✓ PROVEN RED 2026-09-01: mutation applied → exactly this test FAILED, siblings green →
        //   reverted.
        // The member arm: the stored PUBLIC topic's key is the one `alive_count_for` passes to
        // `hb_net::topic::alive_member_count`. (The relay read itself needs a live relay — the
        // same downstream-of-net::client blocker as every command-level guard in this module; the
        // window bound and the 29/31-day boundary are pinned in hb-net's count.rs.)
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("video/films", "criterion", vec!["video".into()], false).unwrap();
        store
            .save_topics(&[StoredTopic { meta: meta.clone(), key, joined_at: 0, membership_json: None }])
            .unwrap();
        assert!(
            alive_key_for(&store, &meta.topic_id).is_some(),
            "a stored public topic supplies its key — the member arm proceeds to the roster read"
        );
    }

    // ── QURATOR-148 owed half — non-member key recovery via the public-join credential ───────────
    //
    // `recover_public_topic_key` itself is downstream of a live relay (the same blocker as every
    // command-level guard here); its network half — join_public's name→topic_id binding (W4) and the
    // credential redeem — is pinned in hb-core/hb-net. What is THIS layer's to pin is the pure gate
    // `public_recovery_allowed`: WHEN recovery may be attempted at all.

    #[test]
    fn recovery_is_refused_without_a_name() {
        // P-10 MUTATION (orchestrator): in `public_recovery_allowed` (the containing fn), delete
        // the `if name.is_empty() { return false; }` guard — an empty name on an empty store then
        // falls through to `true` and THIS test reds. Siblings stay green (both pass a real name).
        // ✓ PROVEN RED 2026-09-01 (this run): mutation applied → exactly this test FAILED, both
        //   siblings green → reverted.
        // An empty name cannot derive the public-join keypair; older callers sent no name at all.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        assert!(
            !public_recovery_allowed(&store, "some-topic-id", ""),
            "no name ⇒ no credential to derive ⇒ recovery is not attempted (aliveness stays unknown)"
        );
    }

    #[test]
    fn recovery_is_refused_for_any_stored_topic_the_private_bar_is_not_bypassed() {
        // P-10 MUTATION (orchestrator): in `public_recovery_allowed` (the containing fn), change
        // the Ok arm to `Ok(_) => true` — the stored private topic is then eligible for recovery
        // and THIS test reds. Siblings stay green (the no-name test reds on the name guard alone;
        // the absent-topic test expects true regardless).
        // ✓ PROVEN RED 2026-09-01: mutation applied → exactly this test FAILED, siblings green →
        //   reverted.
        // A stored topic already had its chance in `alive_key_for`; a None from there means PRIVATE,
        // and the name-derived recovery must not become a bypass around the private crypto bar. The
        // refusal must land BEFORE any relay I/O.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("back room", "private", vec![], true).unwrap();
        store
            .save_topics(&[StoredTopic { meta: meta.clone(), key, joined_at: 0, membership_json: None }])
            .unwrap();
        assert!(
            !public_recovery_allowed(&store, &meta.topic_id, "back room"),
            "a stored (private) topic is never re-derived via the public-join path"
        );
    }

    #[test]
    fn recovery_is_allowed_for_an_unstored_topic_with_a_name() {
        // P-10 MUTATION (orchestrator): in `public_recovery_allowed` (the containing fn), change
        // the Ok arm to `Ok(_) => false` (refuse everything the store could be read for) — THIS
        // test reds on the absent-topic affirmative, while both siblings stay green (they assert
        // refusals, which the mutation only strengthens).
        // ✓ PROVEN RED 2026-09-01: mutation applied → exactly this test FAILED, siblings green →
        //   reverted.
        // The affirmative arm: an un-joined public row with its directory name is exactly the case
        // the owed half exists for — the discovery sidebar's rows.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        assert!(
            public_recovery_allowed(&store, "unjoined-topic-id", "video/films"),
            "an un-stored topic with a name is the non-member recovery case — allowed"
        );
    }

    // ── QURATOR-182 — dispatch coverage, hermetic subset of `commands/topics.rs` ────────────────
    //
    // TRIAGE (16 commands × their real signature, no network):
    //
    //   HERMETIC (driven through the real command below):
    //     topic_list               — store.load_topics() only; no State beyond DataStore.
    //     topic_announce_status    — store.load_announce_times() + announce_cooldown_remaining; pure.
    //     topic_announce_seen      — store.load_announce_seen(); pure read.
    //     topic_announce_mark_seen — store.advance_announce_seen(); pure write.
    //     topic_update_meta (private arm) — me() → load_stored() → description/tags mutation →
    //                               store_topic(). The relay publish sits inside `if !private`, so a
    //                               PRIVATE topic never builds a client. (The public re-announce arm
    //                               is relay-bound and owed below.)
    //     topic_lookup (refusal arm) — me() → normalized_public_name() both fire BEFORE
    //                               net::client(). A name that fails validation reds at the guard's
    //                               own text, never reaching the network. (The exists/member_count
    //                               arms need fetch_announce on a live relay — owed below.)
    //     topic_invite / topic_request_join (npub-parse arm) — me() → parse_npub() →
    //                               load_stored()/net::client(). A malformed npub is refused by the
    //                               parse before any I/O; topic_invite additionally has load_stored
    //                               BEFORE its parse, so its "not in topic" refusal is also hermetic.
    //     topic_announcements (empty-store early return) — load_topics().is_empty() returns Ok(vec![])
    //                               before me()/net::client(), so an empty store drives it hermetically.
    //
    //   RELAY-BOUND (skipped, with reason — the guard is downstream of net::client()):
    //     topic_discover, topic_discover_paint — fetch immediately after client build, no pre-client
    //                               guard worth pinning (tags are passed through to hb-net).
    //     topic_post, topic_channel, topic_roster, topic_announce (publish arm), topic_redeem_invite,
    //     topic_preview_invite, topic_announcements (non-empty store), topic_lookup (exists arm),
    //     topic_update_meta (public arm) — every distinguishing behaviour is on the far side of a
    //                               live relay read/publish. Per-module convention (see the OWED
    //                               blocks in `mod command_guards` above), the pure halves are
    //                               factored out and unit-tested elsewhere in this file; a tests-only
    //                               slice must not build an injection seam.

    mod dispatch {
        use super::*;
        use crate::identity_state::AppIdentity;
        use tauri::Manager;

        /// Mock app + managed state. `identity_loaded` = false leaves SharedIdentity EMPTY so the
        /// identity guard can be exercised; true generates one. RELAY HAZARD: the store pins
        /// `ws://127.0.0.1:9` (closed by definition) so no test in this module can reach the
        /// internet even if a code path drifts toward net::client.
        fn dispatch_app(identity_loaded: bool) -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            store
                .save_settings(&crate::store::Settings {
                    relay_urls: vec!["ws://127.0.0.1:9".into()],
                    ..Default::default()
                })
                .unwrap();
            let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(
                identity_loaded.then(AppIdentity::generate),
            ));
            app.manage(identity);
            app.manage(store);
            app.manage(net::new_shared());
            app
        }

        /// Seed one stored Topic (APPENDING — `save_topics` replaces the whole list, so two
        /// single-element saves would erase each other) and return its `topic_id`.
        /// `membership_json: None` keeps every seeded topic off the wire (only `topic_leave` reads
        /// it, and that path is relay-bound).
        fn seed_topic(store: &DataStore, name: &str, description: &str, private: bool) -> String {
            let (meta, key) = new_topic(name, description, vec![], private).unwrap();
            let id = meta.topic_id.clone();
            let mut topics = store.load_topics().unwrap();
            topics.push(StoredTopic { meta, key, joined_at: 7, membership_json: None });
            store.save_topics(&topics).unwrap();
            id
        }

        /// P-10: in `topic_list` (whose body is the single expression
        /// `Ok(store.load_topics().map_err(cmd_err)?.iter().map(TopicView::from).collect())`,
        /// unique to that fn at line ~251), replace the body with `Ok(Vec::new())` — the
        /// `len() == 2` assert must go red.
        #[tokio::test]
        async fn topic_list_command_round_trips_saved_topics() {
            let app = dispatch_app(true);
            let store = app.state::<DataStore>();
            let public_id = seed_topic(&store, "video/films", "criterion", false);
            let private_id = seed_topic(&store, "back room", "secret", true);

            let got = topic_list(app.state::<DataStore>()).await.unwrap();
            assert_eq!(got.len(), 2, "both seeded topics come back");
            let pub_view = got.iter().find(|t| t.topic_id == public_id).expect("public topic present");
            assert_eq!(pub_view.name, "video/films");
            assert_eq!(pub_view.description, "criterion");
            assert!(!pub_view.private, "public flag round-trips");
            assert_eq!(pub_view.joined_at, 7, "joined_at round-trips");
            let priv_view = got.iter().find(|t| t.topic_id == private_id).expect("private topic present");
            assert!(priv_view.private, "private flag round-trips");
        }

        /// P-10: in `topic_announce_status`, change
        ///   `Ok(announce_cooldown_remaining(times.get(&topic_id).copied(), now()))`
        /// to `Ok(0)` — the `remaining > 103_000` assert must go red. (The `times.get(..)` line
        /// is unique to `topic_announce_status`; `announce_cooldown_remaining` is called from
        /// `burn_announce_cooldown` too, but never with a `times.get(..)` argument.)
        #[tokio::test]
        async fn topic_announce_status_reports_the_persisted_cooldown_and_zero_for_unknown_topics() {
            let app = dispatch_app(true);
            let store = app.state::<DataStore>();
            // A burn stamped 100_000s in the FUTURE: remaining = (t0 + 3600) - now, comfortably
            // above 103_000 and below 104_000 for any realistic wall-clock drift during the test.
            let t0 = now() + 100_000;
            let mut times = HashMap::new();
            times.insert("films".to_string(), t0);
            store.save_announce_times(&times, t0).unwrap();

            let remaining =
                topic_announce_status("films".into(), app.state::<DataStore>()).await.unwrap();
            assert!(
                remaining > 103_000,
                "the command reads the PERSISTED burn (≈103_600 minus elapsed), got {remaining}"
            );
            assert!(
                remaining <= 104_000,
                "and not the raw timestamp or anything unbounded, got {remaining}"
            );

            // No burn on record for this topic ⇒ ready now.
            let unknown = topic_announce_status("other".into(), app.state::<DataStore>()).await.unwrap();
            assert_eq!(unknown, 0, "a topic with no persisted burn reports ready (0)");
        }

        /// P-10: in `topic_announce_mark_seen`, replace the body
        ///   `store.advance_announce_seen(&topic_id, ts).map_err(cmd_err)`
        /// with `Ok(())` — the `seen["films"] == 5_000` assert must go red.
        #[tokio::test]
        async fn topic_announce_mark_seen_then_topic_announce_seen_round_trips() {
            let app = dispatch_app(true);

            topic_announce_mark_seen("films".into(), 5_000, app.state::<DataStore>())
                .await
                .unwrap();

            let seen = topic_announce_seen(app.state::<DataStore>()).await.unwrap();
            assert_eq!(seen.get("films"), Some(&5_000), "the watermark written by mark_seen is read back by topic_announce_seen");
            assert!(!seen.contains_key("other"), "an unmarked topic has no watermark entry");
        }

        /// P-10: this pins the never-rewind semantic THROUGH the command pair. The mutation is in
        /// `crates/hb-app/src/store.rs`, in `advance_announce_seen` — change
        ///   `Some(existing) => ts > *existing,`
        /// to
        ///   `Some(_) => true,`
        /// — the `seen["films"] == 9_000` assert must go red (the stale 4_000 write lands).
        #[tokio::test]
        async fn topic_announce_mark_seen_never_rewinds_the_watermark() {
            let app = dispatch_app(true);

            topic_announce_mark_seen("films".into(), 9_000, app.state::<DataStore>())
                .await
                .unwrap();
            // A STALE stamp (e.g. an out-of-order poll) must not drag the watermark backwards.
            topic_announce_mark_seen("films".into(), 4_000, app.state::<DataStore>())
                .await
                .unwrap();

            let seen = topic_announce_seen(app.state::<DataStore>()).await.unwrap();
            assert_eq!(
                seen.get("films"),
                Some(&9_000),
                "advance_announce_seen is an ADVANCE — a stale ts never rewinds the watermark"
            );
        }

        /// P-10: in `topic_update_meta`, delete the line `stored.meta.description = description;`
        /// (replace it with `let _ = description;`) — the `description == "new blurb"` assert must
        /// go red, and so must the reload assert, proving the write-through is the command's, not
        /// a leftover of the seed.
        ///
        /// The PRIVATE arm is the only hermetic one: the relay publish sits inside
        /// `if !stored.meta.private`, so a private Topic never builds a client (the public arm is
        /// relay-bound — see the triage comment at the top of this module).
        #[tokio::test]
        async fn topic_update_meta_updates_a_private_topic_description_without_any_relay() {
            let app = dispatch_app(true);
            let store = app.state::<DataStore>();
            let id = seed_topic(&store, "back room", "old blurb", true);

            let view = topic_update_meta(
                id.clone(),
                "new blurb".into(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap();

            assert_eq!(view.description, "new blurb", "the new description is returned");
            assert_eq!(view.name, "back room", "the NAME is immutable — it is not a parameter and cannot drift");
            assert!(view.private);
            // And it persisted (store_topic write-through), not just the returned view.
            let reloaded = store.load_topics().unwrap();
            assert_eq!(
                reloaded.iter().find(|t| t.meta.topic_id == id).unwrap().meta.description,
                "new blurb",
                "the edit is on disk, not only in the response"
            );
        }

        /// P-10: in `topic_lookup`, replace the line
        ///   `let normalized = normalized_public_name(&name).map_err(cmd_err)?;`
        /// with `let normalized = name.clone();` — both `starts_with("invalid event: …")`
        /// asserts must go red (each bad name then sails into net::client and fails at the
        /// connect instead). That exact line lives in `topic_lookup` only — `topic_create`'s
        /// validation is inside hb-core's `new_topic`, and `topic_join_public`'s is inside
        /// hb-net's `join_public`, both distinct call sites.
        ///
        /// Both refusals fire BEFORE `net::client` is built, so the error text is the guard's
        /// own — and the valid-name probe proves the placement by dying at the connect instead.
        #[tokio::test]
        async fn topic_lookup_refuses_invalid_public_names_before_any_relay_contact() {
            let app = dispatch_app(true);

            let err = topic_lookup(
                "gaming/retro".into(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.starts_with("invalid event: a public Topic's first path segment must be a category"),
                "the non-category root is refused by the name guard, got {err}"
            );

            let err = topic_lookup(
                "  /  ".into(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.ends_with("a public Topic name cannot be empty"),
                "a name that normalizes to nothing is refused by the name guard, got {err}"
            );

            // Pass-side probe: a well-formed name clears the guard and dies at the pinned
            // unroutable relay (ws://127.0.0.1:9), never in the guard.
            let err = topic_lookup(
                "video/films".into(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.contains("Could not connect to any relay"),
                "a valid name must clear the guard and fail at the connect, got {err}"
            );
        }

        /// Two pre-client guards of `topic_invite` (its call order is me() → parse_npub →
        /// load_stored → net::client — both refusals land before any I/O):
        ///
        /// P-10 (unknown-topic arm): in `load_stored` (the helper fn near the top of this file,
        /// NOT the similar `.find` inside `store_topic` or `alive_key_for`), change
        ///   `.find(|t| t.meta.topic_id == topic_id)`
        /// to `.find(|_| true)` — the `starts_with("You are not in topic")` assert must go red
        /// (the first stored topic is returned and the command proceeds to the connect).
        ///
        /// P-10 (bad-npub arm): in `topic_invite`, change
        ///   `hb_core::identity::parse_npub(&invitee_npub).map_err(cmd_err)?`
        /// to `hb_core::identity::parse_npub(&Identity::generate().npub()).map_err(cmd_err)?`
        /// — the bad-npub assert must go red (the garbage input is replaced by a valid key and
        /// the command proceeds to the connect).
        #[tokio::test]
        async fn topic_invite_refuses_an_unknown_topic_and_a_malformed_npub_before_the_relay() {
            let app = dispatch_app(true);
            let store = app.state::<DataStore>();
            let id = seed_topic(&store, "back room", "d", true);
            let stranger = npub_of(&Identity::generate());

            // Unknown topic, valid invitee: refused by load_stored's guard.
            let err = topic_invite(
                "no-such-topic".into(),
                stranger,
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.starts_with("You are not in topic no-such-topic"),
                "inviting into a topic I'm not in is refused before the relay, got {err}"
            );

            // Stored topic, malformed invitee npub: refused by parse_npub.
            let err = topic_invite(
                id,
                "not-an-npub".into(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.starts_with("invalid public key"),
                "a malformed invitee npub is refused before the relay, got {err}"
            );
        }

        /// P-10: in `topic_request_join`, change
        ///   `hb_core::identity::parse_npub(&member_npub).map_err(cmd_err)?`
        /// to `hb_core::identity::parse_npub(&Identity::generate().npub()).map_err(cmd_err)?`
        /// — the assert must go red (the garbage input is replaced by a valid key and the
        /// command proceeds to the connect). Distinct from `topic_invite`'s identical-shaped
        /// line: this one names `member_npub` and lives in `topic_request_join`.
        #[tokio::test]
        async fn topic_request_join_refuses_a_malformed_member_npub_before_the_relay() {
            let app = dispatch_app(true);

            let err = topic_request_join(
                "not-an-npub".into(),
                "some-topic-id".into(),
                "video/films".into(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert!(
                err.starts_with("invalid public key"),
                "a malformed member npub is refused before the relay, got {err}"
            );
        }

        /// P-10: in `topic_announcements`, delete the early-return block
        ///   `if topics.is_empty() { return Ok(Vec::new()); }`
        /// — this test must go red: with NO identity loaded the command then falls into
        /// `me(&identity)` and returns "No identity loaded…", so the `.unwrap()` panics. That
        /// pins the ORDER (empty-store check precedes the identity guard), not just the value.
        #[tokio::test]
        async fn topic_announcements_returns_empty_before_the_identity_guard_on_an_empty_store() {
            // Identity deliberately NOT loaded: an empty topic store must still return Ok(vec![]),
            // because the empty-store early return fires before me().
            let app = dispatch_app(false);

            let got = topic_announcements(
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap();
            assert!(got.is_empty(), "an empty store returns an empty summary list without needing an identity");
        }
    }
}
