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
    build_announce, build_public_join, member_sign_keys, membership_sign_keys, new_topic,
    normalized_public_name, seal_membership, topic_id_for_name, TopicKey, TopicMeta,
    KIND_TOPIC_MEMBER,
};
use hb_core::{announce_cooldown_remaining, Identity};
use hb_net::{
    announce_to_topic, approve_join, discover_public_topics, discover_public_topics_paint,
    fetch_announce, fetch_channel_full, fetch_invite, fetch_roster, join_public, join_topic,
    leave_topic, member_count, post_to_channel, publish_topic, rank_discovered_topics, RelayClient,
    request_join, TopicDiscoveries,
};

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    store::{CachedPeer, ContactSource, DataStore, ListingsStatus, StoredTopic},
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
        // QURATOR-332 slice A (owner ruling 2026-09-24): this stub path enumerated NOTHING, so it
        // must claim nothing — `Pending` (never-enumerated), never the `Fetched` honest-empty the
        // old default asserted. The background enumeration queue classifies it (read-only) later.
        listings_state: ListingsStatus::Pending,
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None, // W5.2: stamped by the online poll only
        local_tags: vec![],
        // The §7 fingerprint is derivable from the npub alone (no listing access — INV-2 holds).
        fingerprint: hb_core::identity::parse_npub(npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk)),
    };
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// Cap on roster entries ingested into the local contact store per join/refresh (QURATOR-239). A
/// malicious topic can publish an oversized roster to flood a joining user's contact store; this
/// bounds the local write regardless of what a relay-side membership fetch returns (a different
/// layer's `.limit()` bounds what comes back from the relay, not what we're willing to ingest).
/// 500 is generous for any plausible real topic (a Topic roster is a niche-interest club, not a
/// mass broadcast list) while still bounding the flood to a fixed, cheap cost. Truncation takes a
/// deterministic prefix of the roster's given order, never anything iteration-order-dependent.
const MAX_AUTO_ADDED_ROSTER_CONTACTS: usize = 500;

/// Auto-add roster members (except me) as topic contacts, up to [`MAX_AUTO_ADDED_ROSTER_CONTACTS`].
fn auto_add_roster(store: &DataStore, roster: &[PublicKey], me_pk: &PublicKey) -> Result<(), String> {
    let total = roster.len();
    let mut added = 0usize;
    for pk in roster {
        if pk == me_pk {
            continue;
        }
        if added >= MAX_AUTO_ADDED_ROSTER_CONTACTS {
            tracing::warn!(
                "topic roster has {total} entries; truncating auto-add at the \
                 {MAX_AUTO_ADDED_ROSTER_CONTACTS}-contact cap"
            );
            break;
        }
        let npub = pk.to_bech32().map_err(cmd_err)?;
        upsert_topic_contact(store, &npub)?;
        added += 1;
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
///
/// QURATOR-192: rows carrying an UNEXPIRED known-dead verdict (stamped by [`topic_rank`]'s
/// `Some(0)` aliveness result) are dropped HERE, before they reach the UI — otherwise a dead public
/// Topic is rediscovered, repainted, re-queried and re-dropped on every single page open, forever.
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
    // Fail-open: a verdict-map load failure suppresses nothing — never a confident negative.
    let dead = store.load_dead_topic_verdicts().unwrap_or_default();
    Ok(found
        .topics
        .into_iter()
        .filter(|m| !known_dead_verdict_active(&dead, &m.topic_id))
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
    // QURATOR-192: the rows that earned a CONFIDENT dead verdict (`Some(0)` only — `None` is
    // unknown and must never be stamped dead) are persisted after the loop, so the next
    // `topic_discover_paint` suppresses them instead of rediscovering and re-querying them forever.
    let mut newly_dead: Vec<String> = Vec::new();
    for (m, count) in ranked {
        let topic_id = m.topic_id.clone();
        let alive = alive_count_for(&client, &store, &topic_id, &m.name).await;
        if dead_verdict_warranted(alive) {
            newly_dead.push(topic_id.clone());
        }
        out.push(TopicRank { topic_id, member_count_estimate: count, alive_count: alive });
    }
    record_dead_verdicts(&store, &newly_dead);
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

/// Does this row's aliveness result warrant a PERSISTED dead verdict (QURATOR-192)? `Some(0)` —
/// the roster was read with a real key and every member's newest beacon is older than the window —
/// is the ONLY confident dead. `None` is UNKNOWN (no key / a private topic / a relay error):
/// persisting it would bury a LIVE Topic permanently (the never-render-a-confident-negative rule,
/// same as QURATOR-67/68/93). `Some(n > 0)` is alive. Pure, so the gate is directly testable.
fn dead_verdict_warranted(alive: Option<usize>) -> bool {
    alive == Some(0)
}

/// Stamp `topic_ids` known-dead in the persisted verdict map (QURATOR-192), keyed by the verdict's
/// unix-secs. Best-effort by design: a store failure is swallowed, never propagated — ranking must
/// not die for a bookkeeping write, and a lost verdict only costs one re-query. Callers pass only
/// ids that cleared [`dead_verdict_warranted`].
fn record_dead_verdicts(store: &DataStore, topic_ids: &[String]) {
    if topic_ids.is_empty() {
        return;
    }
    let ts = now();
    if let Ok(mut verdicts) = store.load_dead_topic_verdicts() {
        for id in topic_ids {
            verdicts.insert(id.clone(), ts);
        }
        let _ = store.save_dead_topic_verdicts(&verdicts);
    }
}

/// The suppression check [`topic_discover_paint`] applies (QURATOR-192): does `topic_id` carry an
/// UNEXPIRED known-dead verdict? A verdict is honoured for one aliveness window
/// ([`hb_net::count::TOPIC_ALIVE_WINDOW_SECS`]) past its stamp: a revival beacon sent after the
/// verdict is still inside its own window when ours lapses, so a revived Topic reappears — and a
/// still-dead one simply earns a fresh verdict on its next rank. Pure, so the expiry is testable.
fn known_dead_verdict_active(verdicts: &HashMap<String, u64>, topic_id: &str) -> bool {
    verdicts
        .get(topic_id)
        .is_some_and(|&ts| now() < ts.saturating_add(hb_net::count::TOPIC_ALIVE_WINDOW_SECS))
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
/// `fetch_invite`'s existing topic_id check (reusing the public-join W4 substitution guard). The
/// `expected_issuer_npub` (the preview's `issuer_npub`, sent back by the UI) binds it to the ISSUER
/// the user consented to as well (QURATOR-227): a forged wrap naming the same topic_id no longer wins
/// the first-valid-decrypt race.
#[tauri::command]
pub async fn topic_redeem_invite(
    expected_topic_id: String,
    expected_issuer_npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Option<TopicView>> {
    let me = me(&identity).await?;
    // The symmetric parse of `topic_preview_invite`'s `issuer.to_bech32()` — the value the UI echoes
    // back from the preview.
    let expected_issuer = hb_core::identity::parse_npub(&expected_issuer_npub).map_err(cmd_err)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // `&mut seen`: redeem_invite atomically records a single-use invite's nonce on success (Decision E);
    // we persist the set afterward so a restart can't re-accept it.
    let mut seen = store.load_topic_nonces().map_err(cmd_err)?;
    let t = now();
    let redeemed =
        fetch_invite(&client, &me, &mut seen, t, net::RELAY_TIMEOUT, Some(&expected_topic_id), Some(&expected_issuer))
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
    // expected_topic_id/issuer = None: the preview is the DISCOVERY step — it has nothing to expect
    // yet; whatever valid invite it finds is what the user then consents to (issuer included).
    let redeemed = fetch_invite(&client, &me, &mut seen, t, net::RELAY_TIMEOUT, None, None)
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
        // QURATOR-292 review: the retraction is BEST-EFFORT and must never hold the local removal
        // hostage. It was `?` until a pre-migration membership made that fatal: a topic joined
        // before the pseudonym swap has a v1 `membership_json`, whose author is the old HMAC
        // pseudonym, so `membership_deletion` correctly REFUSES to sign a deletion it cannot
        // author — and the `?` aborted before the store filter below, leaving the member stuck in
        // the topic locally AND absent from every roster remotely, with no way out. Leaving is a
        // LOCAL act; NIP-09 retraction is best-effort by nature (a non-compliant relay may ignore
        // it regardless, N5), so a failed publish is logged and the local drop proceeds.
        if let Err(e) = leave_topic(&client, &stored.key, &me, &membership).await {
            tracing::warn!(
                topic = %topic_id,
                error = %e,
                "topic_leave: retraction not published (pre-v2 membership, or relay refused); \
                 dropping the local record anyway — leaving must not depend on the publish"
            );
        }
    }
    let topics: Vec<StoredTopic> =
        store.load_topics().map_err(cmd_err)?.into_iter().filter(|t| t.meta.topic_id != topic_id).collect();
    store.save_topics(&topics).map_err(cmd_err)
}

/// One roster row for the UI (QURATOR-304): the member's npub plus their **Topic aliveness**
/// state over the same 30-day window the discovery counts fold
/// ([`hb_net::count::TOPIC_ALIVE_WINDOW_SECS`]).
///
/// `dormant` is a TRI-STATE, and the `None` half is fail-open on purpose:
/// - `Some(false)` — pinged within the window (alive): a normal row.
/// - `Some(true)` — NO beacon within the window (dormant). The row is KEPT, never dropped: a
///   member knows who is in their own room, and an absent member is not a departed one (there is
///   no leave affordance to point at) — owner ruling 2026-09-20, shown-dormant over hidden. The
///   UI dims the row and states the cue instead.
/// - `None` — the presence read FAILED (relay/connect error): unknown, never a verdict. A failed
///   read is not a negative claim about a person, so the UI renders a normal row.
///
/// This is the panel-side twin of `topic_rank`'s `alive_count`: same roster, same
/// [`hb_net::count::fetch_last_seen_for_authors`] read, same window — so the roster panel and the
/// alive count can only disagree by display, never by definition.
#[derive(Debug, Clone, Serialize)]
pub struct RosterMemberView {
    pub npub: String,
    pub dormant: Option<bool>,
}

// ── QURATOR-305: the v1→v2 membership migration (republish-on-next-launch) ───────────────────────
//
// QURATOR-292 moved the membership pseudonym to `membership_sign_keys` (HKDF over the member's own
// SECRET). The reader is v2-ONLY by owner ruling — NO dual-read, because dual-verify would keep the
// FORGEABLE v1 derivation accepted for the whole transition release, leaving both insider
// roster-eviction vectors live for as long as the migration runs. The cost of that ruling: every
// membership stored before the swap fails `open_membership`, so a member's own row falls off their
// roster (empty ⇒ the derived DISSOLUTION signal) and the topic drops out of discovery entirely —
// and "just re-join" cannot recover a PRIVATE topic without a fresh invite, which a member cannot
// request from a topic that is invisible to them. The heal is republish-on-next-launch: on the
// roster open, re-seal under the new derivation, publish, and overwrite the stored copy. One
// round-trip, self-healing, retried on every open until it lands.

/// What the stored `membership_json` is, relative to THIS member — the migration's decision,
/// split out pure so the classification is unit-testable without a relay.
enum MembershipVintage {
    /// Already sealed under the current (QURATOR-292, member-secret) derivation — the silent
    /// no-op arm. This is what makes the migration IDEMPOTENT: opening a v2 topic must not
    /// publish, so the second open costs nothing.
    V2,
    /// Sealed under the pre-292 publicly-derivable derivation, and provably THIS member's — the
    /// one arm that republishes.
    MyV1,
    /// Authored by NEITHER of this member's pseudonyms, or claims another topic: NOT this
    /// member's membership. Republishing it would assert a membership that was never theirs, so
    /// it is never touched.
    Foreign,
    /// A record exists but is not a parseable event — it cannot be positively identified, so it
    /// is left untouched (and logged: silently skipping forever would hide the corruption).
    Unreadable,
    /// No stored `membership_json` (topics seeded before the record existed) — nothing to do.
    Absent,
}

/// Classify the stored membership record. ⚠ POSITIVE identification, never mere negation: "not my
/// v2 pseudonym" is NOT enough to republish, because the v1 derivation was PUBLICLY computable —
/// any topic-key holder could author an event at any coordinate under it. `MyV1` therefore requires
/// ALL of: the membership kind, the `d` tag claiming THIS topic (the join that wrote the record
/// sealed this topic_id; a mismatch means the record was misfiled or tampered, not migrated), and
/// the author being the OLD derivation's pseudonym for THIS member (`member_sign_keys`, which
/// still signs posts — so the kind check is load-bearing: without it a misfiled POST, authored by
/// that same still-current pseudonym, would classify as a membership).
fn membership_vintage(
    key: &TopicKey,
    topic_id: &str,
    me: &Identity,
    membership_json: Option<&str>,
) -> MembershipVintage {
    let Some(json) = membership_json else { return MembershipVintage::Absent };
    let Ok(ev) = Event::from_json(json) else { return MembershipVintage::Unreadable };
    // The v2 fast path: authored by the member-secret pseudonym for THIS (key, topic_id) ⇒ already
    // current, never touched. (A derivation Err is the ~2^-128 scalar-out-of-range case. It skips
    // only THIS arm — a genuine v1 record still classifies MyV1 and attempts the republish, where
    // `seal_membership` hits the same Err and returns before publishing: a warn per open, zero
    // relay traffic, no store write. Safe, but by failing inside `join_topic` rather than by
    // falling to the Foreign tail, which an earlier version of this comment claimed.)
    let v2_author = membership_sign_keys(key, topic_id, me).ok().map(|k| k.public_key());
    if v2_author.as_ref() == Some(&ev.pubkey) {
        return MembershipVintage::V2;
    }
    if ev.kind == Kind::from_u16(KIND_TOPIC_MEMBER) && ev.tags.identifier() == Some(topic_id) {
        let v1_author = member_sign_keys(key, &me.public_key()).ok().map(|k| k.public_key());
        if v1_author.as_ref() == Some(&ev.pubkey) {
            return MembershipVintage::MyV1;
        }
    }
    MembershipVintage::Foreign
}

/// Overwrite the stored `membership_json` with the republished event — the LAST step of the
/// migration, and deliberately so: writing before the publish succeeds would flip the record to
/// v2 while the wire still holds the stale v1 event, and the v2 fast path would then never retry
/// (a one-shot migration with a poisoned flag). Everything else is preserved: meta, key, and the
/// HISTORICAL `joined_at` (the republish re-seals NOW; the join happened when it happened). Split
/// out so the store effect is testable without a relay.
fn store_republished_membership(
    store: &DataStore,
    stored: &StoredTopic,
    ev: &Event,
) -> Result<(), String> {
    // ⚠ UPDATE-ONLY — deliberately NOT `store_topic`, whose push-if-absent arm is correct for a
    // JOIN and is a resurrection vector here (QURATOR-292 review, F1). `join_topic` above is
    // awaited, so a concurrent `topic_leave` can drop this row while the publish is in flight
    // (Tauri commands run concurrently). Pushing the row back then un-leaves the member: enrolled
    // on the relay AND the topic restored to their store, with no affordance saying so. The
    // migration heals a membership the member already has; it must never CREATE one. A row that
    // vanished mid-flight means the member left, and the relay-side re-enrolment is corrected by
    // their next leave — which now works, because the republished record is v2 and
    // `membership_deletion` accepts an author-signed retraction.
    let mut topics = store.load_topics().map_err(cmd_err)?;
    let Some(row) = topics.iter_mut().find(|x| x.meta.topic_id == stored.meta.topic_id) else {
        tracing::info!(
            topic = %stored.meta.topic_id,
            "membership republished but the topic was left while the publish was in flight; not \
             restoring the row (the leave wins)"
        );
        return Ok(());
    };
    row.membership_json = Some(ev.as_json());
    store.save_topics(&topics).map_err(cmd_err)
}

/// The migration trigger, run on every roster open. Best-effort BY CONSTRUCTION: it returns `()`
/// and cannot fail the roster read — the same lesson `topic_leave` learned the hard way (a `?` on
/// a publish inside a read strands the user; see the comment there). A failed publish logs and the
/// record stays v1, so the NEXT open retries; only a successful publish followed by a successful
/// store write completes the migration.
async fn republish_pre_v2_membership(
    store: &DataStore,
    stored: &StoredTopic,
    me: &Identity,
    client: &RelayClient,
) {
    match membership_vintage(&stored.key, &stored.meta.topic_id, me, stored.membership_json.as_deref()) {
        MembershipVintage::V2 | MembershipVintage::Absent => {}
        MembershipVintage::Unreadable => tracing::warn!(
            topic = %stored.meta.topic_id,
            "stored membership record is not a parseable event; leaving it untouched (it cannot be \
             positively identified as this member's membership)"
        ),
        MembershipVintage::Foreign => tracing::warn!(
            topic = %stored.meta.topic_id,
            "stored membership record is authored by neither of this member's pseudonyms; NOT \
             republishing — that would assert a membership that was never this member's"
        ),
        MembershipVintage::MyV1 => {
            // `join_topic` IS the republish primitive — `seal_membership` under the v2 derivation
            // + publish. Re-implementing the publish is how the WAN harness drifted from
            // production before; reuse the production path.
            match join_topic(client, &stored.key, &stored.meta.topic_id, me, now()).await {
                Ok(ev) => {
                    if let Err(e) = store_republished_membership(store, stored, &ev) {
                        tracing::warn!(
                            topic = %stored.meta.topic_id,
                            error = %e,
                            "membership republished but the local record overwrite failed; the \
                             stored copy stays pre-v2 and the migration retries on the next open"
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    topic = %stored.meta.topic_id,
                    error = %e,
                    "pre-v2 membership republish not published (relay refused or offline); the \
                     roster read continues and the migration retries on the next open"
                ),
            }
        }
    }
}

/// Fetch a Topic's roster (members-only) and refresh the auto-added topic contacts. Each row
/// carries its aliveness state (QURATOR-304) — see [`RosterMemberView`].
#[tauri::command]
pub async fn topic_roster(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<RosterMemberView>> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // QURATOR-305 (republish-on-next-launch): a membership stored before the QURATOR-292 pseudonym
    // swap is sealed under the old publicly-derivable derivation, so the v2-only reader drops it —
    // this member's own row falls off the roster, an empty roster is the derived DISSOLUTION
    // signal, and the topic drops out of discovery entirely. Re-seal, publish, overwrite —
    // best-effort, BEFORE the roster read, so this open (or the next) sees the member back. This
    // is the ONLY hook: `topic_list` is a store-only read with no identity and no relay client,
    // and the member's own topic stays listed there (the store record is intact), so the roster
    // open remains reachable for exactly the members who need the heal.
    republish_pre_v2_membership(&store, &stored, &me, &client).await;
    let roster = fetch_roster(&client, &topic_id, &stored.key, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    // QURATOR-304: the SAME author-bounded presence read the aliveness count folds
    // (`alive_member_count` → `fetch_last_seen_for_authors`), over the SAME roster and window —
    // one extra relay read per roster open, never a global unbounded kind-11111 query (the
    // 2026-08-01 launch-gate defect class). A member absent from the answer has no beacon inside
    // the window ⇒ dormant; an Err'd read is UNKNOWN (every row `dormant: None`), never dormant.
    let last_seen = hb_net::count::fetch_last_seen_for_authors(
        &client,
        &roster,
        hb_net::count::TOPIC_ALIVE_WINDOW_SECS,
        net::RELAY_TIMEOUT,
    )
    .await
    .ok();
    roster
        .iter()
        .map(|p| {
            Ok(RosterMemberView {
                npub: p.to_bech32().map_err(cmd_err)?,
                dormant: last_seen.as_ref().map(|(seen, _)| !seen.contains_key(p)),
            })
        })
        .collect()
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
        let issuer = Identity::generate();
        let id = Identity::generate();
        let mut seen = store.load_topic_nonces().unwrap();
        seen.insert(hb_core::topic::invite_seen_key(&issuer.public_key(), "topic-abc", &id.public_key(), "n1"));
        store.save_topic_nonces(&seen).unwrap();
        let reloaded = store.load_topic_nonces().unwrap();
        assert!(
            reloaded.contains(&hb_core::topic::invite_seen_key(
                &issuer.public_key(),
                "topic-abc",
                &id.public_key(),
                "n1"
            ))
        );
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

    // mutation: at topics.rs, change the `added >= MAX_AUTO_ADDED_ROSTER_CONTACTS` cap check in
    // `auto_add_roster` to always-false (e.g. `false &&` prefix) — the oversized-roster test below
    // must fail (it will then ingest every roster entry, not exactly the cap).
    #[test]
    fn an_oversized_roster_ingests_exactly_the_cap_deterministic_prefix() {
        // QURATOR-239: a malicious topic with a roster far over the cap must not flood the local
        // contact store — exactly MAX_AUTO_ADDED_ROSTER_CONTACTS entries land, taken as a
        // deterministic prefix of the roster's given order (not hash-map-order-dependent).
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let over = MAX_AUTO_ADDED_ROSTER_CONTACTS + 50;
        let members: Vec<Identity> = (0..over).map(|_| Identity::generate()).collect();
        let roster: Vec<PublicKey> = members.iter().map(|id| id.public_key()).collect();
        auto_add_roster(&store, &roster, &me.public_key()).unwrap();

        let ingested = members
            .iter()
            .filter(|id| store.load_contact(&CachedPeer::pubkey_hash(&npub_of(id))).unwrap().is_some())
            .count();
        assert_eq!(ingested, MAX_AUTO_ADDED_ROSTER_CONTACTS, "exactly the cap was ingested, not the whole roster");

        // Deterministic prefix: the first MAX_AUTO_ADDED_ROSTER_CONTACTS members (in roster order)
        // are the ones present; the trailing entries were dropped.
        for id in &members[..MAX_AUTO_ADDED_ROSTER_CONTACTS] {
            assert!(
                store.load_contact(&CachedPeer::pubkey_hash(&npub_of(id))).unwrap().is_some(),
                "a member within the cap-sized prefix must be ingested"
            );
        }
        for id in &members[MAX_AUTO_ADDED_ROSTER_CONTACTS..] {
            assert!(
                store.load_contact(&CachedPeer::pubkey_hash(&npub_of(id))).unwrap().is_none(),
                "a member past the cap-sized prefix must be dropped"
            );
        }
    }

    // mutation: at topics.rs, in `auto_add_roster` change `if pk == me_pk { continue; }` to
    // `if false { continue; }` — the under-cap test below must fail (my own key would then also be
    // ingested, growing the ingested count past the roster's member count).
    #[test]
    fn an_under_cap_roster_ingests_whole_and_unchanged() {
        // QURATOR-239: an honest, small roster behaves exactly as before the cap — everyone
        // (except me) is ingested, none dropped.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let members: Vec<Identity> = (0..10).map(|_| Identity::generate()).collect();
        let mut roster: Vec<PublicKey> = members.iter().map(|id| id.public_key()).collect();
        roster.push(me.public_key()); // me is in the roster too but must be skipped, not counted
        auto_add_roster(&store, &roster, &me.public_key()).unwrap();

        for id in &members {
            assert!(
                store.load_contact(&CachedPeer::pubkey_hash(&npub_of(id))).unwrap().is_some(),
                "every under-cap member is ingested unchanged"
            );
        }
        assert!(
            store.load_contact(&CachedPeer::pubkey_hash(&npub_of(&me))).unwrap().is_none(),
            "my own key in the roster is never added as my own contact"
        );
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

    /// QURATOR-292 review (high finding): the local removal must NOT be gated on the retraction
    /// publishing. A topic joined before the pseudonym swap stores a v1 `membership_json` whose
    /// author is the old HMAC pseudonym, so `membership_deletion` rightly refuses to sign a
    /// deletion it cannot author — and while `leave_topic` carried a `?`, that refusal aborted
    /// `topic_leave` BEFORE the store filter, leaving the member stuck in the topic locally and
    /// absent from every roster remotely, with no way out. NIP-09 retraction is best-effort (N5)
    /// regardless of migration state, so the publish may always fail and leaving must still work.
    ///
    /// `topic_leave` needs a live relay client to drive end-to-end, so this pins the CONTROL FLOW
    /// structurally: the body must not apply `?` to `leave_topic`, and the store tail must follow
    /// it unconditionally. ⚠ Honest limit, stated rather than implied: this proves the shape of
    /// the code, NOT that a real refusal is survived — that belongs to the hb-it row owed on
    /// QURATOR-305. The scan strips `//` lines first, so this comment cannot satisfy it.
    ///
    /// MUTATION (P-10) — resolve by production line number at apply time, never by text:
    ///   restore the `?` form at the `leave_topic(` call inside `topic_leave` (i.e.
    ///   `leave_topic(&client, &stored.key, &me, &membership).await.map_err(cmd_err)?;`,
    ///   dropping the `if let Err(e)` wrapper) → the `is_err_wrapped` assert below reds.
    #[test]
    fn leaving_a_topic_does_not_depend_on_the_retraction_publishing() {
        let src = include_str!("topics.rs");
        let code: String =
            src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let at = code.find("pub async fn topic_leave(").expect("topic_leave must exist");
        let end = code[at..].find("\n}\n").expect("topic_leave must end") + at;
        let body = &code[at..end];

        assert!(
            body.contains("if let Err(e) = leave_topic("),
            "the retraction must be best-effort: a failed publish is logged, never propagated"
        );
        assert!(
            !body.contains("leave_topic(&client, &stored.key, &me, &membership).await.map_err(cmd_err)?"),
            "`?` on leave_topic strands a pre-migration member — they can neither retract nor leave"
        );
        let publish_at = body.find("leave_topic(").expect("the retraction call must exist");
        let drop_at = body.find("store.save_topics(").expect("the local drop must exist");
        assert!(
            publish_at < drop_at,
            "the local drop must follow the retraction attempt, and must run whether or not it failed"
        );
    }

    // ── QURATOR-305: republish-on-next-launch (pre-v2 membership migration) ────────────────────────
    //
    // ⚠ Honest scope of everything below: the roster read needs a live relay, so NO test here
    // drives `topic_roster` end-to-end — that proof belongs to the hb-it row owed on QURATOR-305.
    // What IS proved, with real keys: the classification decision the migration acts on, and the
    // store effect of a completed republish. The wiring is pinned structurally (same convention as
    // `leaving_a_topic_does_not_depend_on_the_retraction_publishing` above).

    /// The v2 fast path: a membership sealed under the CURRENT (member-secret) derivation must
    /// classify `V2` — the silent no-op arm that makes the migration IDEMPOTENT (opening a topic
    /// twice must not publish twice). Real crypto: `seal_membership` is the same production path
    /// `join_topic` seals with.
    ///
    /// MUTATION (P-10) — resolve by production line number at apply time, never by text: at
    /// topics.rs line 906 (`if v2_author.as_ref() == Some(&ev.pubkey) {`, inside
    /// `membership_vintage`), prefix the condition with `false &&` → this test reds (the v2 event
    /// falls through to `Foreign`).
    #[test]
    fn a_v2_membership_classifies_as_v2_the_idempotent_no_op() {
        let (meta, key) = new_topic("video/films", "criterion", vec![], false).unwrap();
        let me = Identity::generate();
        let v2 = seal_membership(&key, &meta.topic_id, &me, 1_000).unwrap();
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some(&v2.as_json())),
                MembershipVintage::V2
            ),
            "a membership sealed under the current derivation must be the silent no-op"
        );
    }

    /// POSITIVE v1 identification: a membership authored by the OLD publicly-derivable pseudonym
    /// for THIS member, claiming THIS topic, classifies `MyV1` — the one arm that republishes.
    /// Built the way the pre-292 code sealed it (`member_sign_keys` signer, membership kind,
    /// `d`=topic_id). The classifier never opens the event, so the (pub(crate)-gated) v1 proof
    /// payload is not reproduced here — hb-core's own
    /// `v1_membership_events_are_rejected_by_the_v2_only_reader` test owns that half; this pins
    /// the DECISION the migration acts on: author + kind + topic claim.
    ///
    /// MUTATION (P-10): at topics.rs line 911 (`if v1_author.as_ref() == Some(&ev.pubkey) {`,
    /// inside `membership_vintage`), prefix the condition with `false &&` → reds (falls through
    /// to `Foreign`).
    #[test]
    fn my_pre_292_membership_classifies_for_republish() {
        let (meta, key) = new_topic("video/films", "", vec![], true).unwrap();
        let me = Identity::generate();
        let v1_signer = member_sign_keys(&key, &me.public_key()).unwrap();
        let v1 = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), "pre-292 membership")
            .tags([Tag::identifier(meta.topic_id.clone())])
            .custom_created_at(Timestamp::from(1_000u64))
            .sign_with_keys(&v1_signer)
            .unwrap();
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some(&v1.as_json())),
                MembershipVintage::MyV1
            ),
            "the member's own pre-292 membership is the one record that republishes"
        );
    }

    /// Acceptance #4 — an event that is NOT this member's membership is never republished. Four
    /// shapes, each of which a "not v2 ⇒ republish" shortcut would get wrong: (a) another
    /// member's CURRENT v2 membership; (b) another member's v1-shaped event; (c) THIS member's
    /// v1 pseudonym but claiming a DIFFERENT topic (misfiled or forged — the old derivation was
    /// publicly computable, so anyone could author at that coordinate); (d) THIS member's
    /// still-current POST pseudonym (`member_sign_keys` still signs posts) on a non-membership
    /// kind — the kind check is what keeps a misfiled post out of the republish arm.
    ///
    /// MUTATION (P-10), three independent anchors:
    ///   (i) topics.rs line 915 (`MembershipVintage::Foreign`, the tail return of
    ///       `membership_vintage`) → change to `MembershipVintage::MyV1` → all four asserts red;
    ///   (ii) on topics.rs line 909, the `ev.tags.identifier() == Some(topic_id)` conjunct →
    ///        `false &&` it (or delete it) → sub-case (c) alone reds;
    ///   (iii) on topics.rs line 909, the `ev.kind == Kind::from_u16(KIND_TOPIC_MEMBER)` conjunct
    ///        → `false &&` it (or delete it) → sub-case (d) alone reds.
    #[test]
    fn a_foreign_membership_is_never_mine_to_republish() {
        let (meta, key) = new_topic("video/films", "", vec![], true).unwrap();
        let me = Identity::generate();
        let other = Identity::generate();

        let theirs_v2 = seal_membership(&key, &meta.topic_id, &other, 1_000).unwrap();
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some(&theirs_v2.as_json())),
                MembershipVintage::Foreign
            ),
            "another member's current membership is not mine to republish"
        );

        let their_v1_signer = member_sign_keys(&key, &other.public_key()).unwrap();
        let theirs_v1 =
            EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), "their legacy membership")
                .tags([Tag::identifier(meta.topic_id.clone())])
                .custom_created_at(Timestamp::from(1_000u64))
                .sign_with_keys(&their_v1_signer)
                .unwrap();
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some(&theirs_v1.as_json())),
                MembershipVintage::Foreign
            ),
            "another member's pre-292 event is not mine to republish"
        );

        let my_signer = member_sign_keys(&key, &me.public_key()).unwrap();
        let wrong_topic = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), "misfiled")
            .tags([Tag::identifier("video/elsewhere".to_string())])
            .custom_created_at(Timestamp::from(1_000u64))
            .sign_with_keys(&my_signer)
            .unwrap();
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some(&wrong_topic.as_json())),
                MembershipVintage::Foreign
            ),
            "my own pseudonym claiming ANOTHER topic is a misfile, not this topic's membership"
        );

        // kind 1 — any non-membership kind stands in for a channel post.
        let my_post = EventBuilder::new(Kind::Custom(1), "a channel post, not a membership")
            .tags([Tag::identifier(meta.topic_id.clone())])
            .custom_created_at(Timestamp::from(1_000u64))
            .sign_with_keys(&my_signer)
            .unwrap();
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some(&my_post.as_json())),
                MembershipVintage::Foreign
            ),
            "a misfiled POST (same still-current pseudonym, wrong kind) must not republish"
        );
    }

    /// No-action records: `None` (topics seeded before the record existed — this file's own
    /// fixtures) and an unparseable record (corrupt or tampered — it cannot be positively
    /// identified, so never republished, and the helper logs rather than skipping silently
    /// forever).
    ///
    /// MUTATION (P-10): topics.rs line 900 (`let Some(json) = membership_json else { return
    /// MembershipVintage::Absent };` in `membership_vintage`) → change the returned variant to
    /// `MyV1` → the Absent assert reds; topics.rs line 901 (the `Unreadable` arm) likewise reds
    /// the Unreadable assert.
    #[test]
    fn missing_or_unparseable_records_are_no_action() {
        let (meta, key) = new_topic("video/films", "", vec![], true).unwrap();
        let me = Identity::generate();
        assert!(
            matches!(membership_vintage(&key, &meta.topic_id, &me, None), MembershipVintage::Absent),
            "no stored record ⇒ nothing to migrate (and nothing logged)"
        );
        assert!(
            matches!(
                membership_vintage(&key, &meta.topic_id, &me, Some("{not an event")),
                MembershipVintage::Unreadable
            ),
            "an unparseable record is left untouched, never blindly republished"
        );
    }

    /// The migration's store effect, without a relay: a completed republish OVERWRITES
    /// `membership_json` and nothing else — the HISTORICAL `joined_at` is preserved (the
    /// republish re-seals now; the join happened when it happened), the reloaded record
    /// classifies `V2` under the STORED key (meta + key survived the overwrite — `TopicKey` has
    /// no `PartialEq`, so key preservation is proven functionally, not by field compare), and an
    /// unrelated sibling topic in the same store is untouched.
    ///
    /// MUTATION (P-10) — resolve by production line number at apply time, never by text:
    ///   `store_republished_membership`'s write, `store.save_topics(&topics)` (topics.rs:951 at
    ///   time of writing) — replace it with `Ok(())`, deleting the write → this test reds (the
    ///   reloaded record still holds the v1 JSON, so both the json and v2-classification asserts
    ///   fail).
    /// ⚠ This anchor previously cited line 929 and the `store_topic(` call; the QURATOR-292 review
    /// (F1) replaced that call with an update-only body, so both the line and the symbol moved.
    /// Re-read the line you are about to mutate — an anchor is a recipe a future reader follows.
    #[test]
    fn republish_overwrites_only_the_membership_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("video/films", "", vec![], true).unwrap();
        let me = Identity::generate();
        let v1_signer = member_sign_keys(&key, &me.public_key()).unwrap();
        let v1 = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), "pre-292 membership")
            .tags([Tag::identifier(meta.topic_id.clone())])
            .custom_created_at(Timestamp::from(1_000u64))
            .sign_with_keys(&v1_signer)
            .unwrap();
        let stored = StoredTopic {
            meta: meta.clone(),
            key,
            joined_at: 42,
            membership_json: Some(v1.as_json()),
        };
        let (other_meta, other_key) = new_topic("video/animes", "", vec![], true).unwrap();
        store
            .save_topics(&[
                stored.clone(),
                StoredTopic {
                    meta: other_meta.clone(),
                    key: other_key,
                    joined_at: 7,
                    membership_json: None,
                },
            ])
            .unwrap();

        let fresh = seal_membership(&stored.key, &meta.topic_id, &me, 9_999).unwrap();
        store_republished_membership(&store, &stored, &fresh).unwrap();

        let back = store.load_topics().unwrap();
        let mine = back.iter().find(|t| t.meta.topic_id == meta.topic_id).unwrap();
        assert_eq!(
            mine.membership_json,
            Some(fresh.as_json()),
            "the republished event overwrites the stored copy"
        );
        assert_eq!(mine.joined_at, 42, "the HISTORICAL join time is preserved, not reset to the republish time");
        assert!(
            matches!(
                membership_vintage(&mine.key, &meta.topic_id, &me, mine.membership_json.as_deref()),
                MembershipVintage::V2
            ),
            "the reloaded record classifies v2 under the STORED key — meta and key survived the overwrite"
        );
        let sibling = back.iter().find(|t| t.meta.topic_id == other_meta.topic_id).unwrap();
        assert_eq!(sibling.membership_json, None, "an unrelated topic's record is untouched");
        assert_eq!(sibling.joined_at, 7, "an unrelated topic's join time is untouched");
    }

    /// QURATOR-292 review, F1: the migration must never RESURRECT a topic the member left while
    /// its publish was in flight. `join_topic` is awaited and Tauri commands run concurrently, so
    /// a `topic_leave` can drop the row mid-publish; if the store write then used `store_topic`,
    /// its push-if-absent arm (correct for a JOIN) would put the row back and the member would be
    /// un-left — enrolled on the relay AND the topic restored locally, with nothing saying so.
    /// The migration heals a membership the member already has; it must never create one.
    ///
    /// MUTATION (P-10) — resolve by production line number at apply time, never by text:
    ///   in `store_republished_membership`, replace the update-only body with the old
    ///   `store_topic(store, StoredTopic { .. })` call → the absent row is pushed back and the
    ///   `is_none()` assert below reds.
    #[test]
    fn a_topic_left_mid_publish_is_not_resurrected_by_the_migration() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("video/films", "", vec![], true).unwrap();
        let me = Identity::generate();
        let stored = StoredTopic { meta: meta.clone(), key, joined_at: 42, membership_json: None };

        // The leave already won the race: the row is gone from the store, but the migration still
        // holds the `stored` snapshot it loaded before the publish.
        let (other_meta, other_key) = new_topic("video/animes", "", vec![], true).unwrap();
        store
            .save_topics(&[StoredTopic {
                meta: other_meta.clone(),
                key: other_key,
                joined_at: 7,
                membership_json: None,
            }])
            .unwrap();

        let fresh = seal_membership(&stored.key, &meta.topic_id, &me, 9_999).unwrap();
        store_republished_membership(&store, &stored, &fresh).expect("a lost race is not an error");

        let back = store.load_topics().unwrap();
        assert!(
            back.iter().find(|t| t.meta.topic_id == meta.topic_id).is_none(),
            "the left topic must NOT be restored by the in-flight migration — the leave wins"
        );
        assert_eq!(back.len(), 1, "the sibling topic is untouched: {back:?}");
    }


    /// The wiring, pinned structurally (same convention as the `topic_leave` scan above): the
    /// roster open must run the migration BEFORE the roster fetch, and the migration helper must
    /// (i) REUSE `join_topic` — the production seal+publish — never a re-implemented publish,
    /// and (ii) write the store ONLY after the publish succeeded (write-first would flip the
    /// record to v2 while the wire still holds the stale v1 event, and the v2 fast path would
    /// then never retry — a one-shot migration with a poisoned flag). The helper returns `()`, so
    /// a `?` on its publish cannot even compile; this pins the shape that keeps it that way.
    ///
    /// ⚠ Honest limit, stated rather than implied: this proves the SHAPE of the code, NOT that a
    /// real relay refusal is survived end-to-end, nor publish-once idempotency across two real
    /// opens — both belong to the hb-it row owed on QURATOR-305.
    ///
    /// MUTATION (P-10), three independent anchors, each a deletion/replacement of the scanned
    /// text itself:
    ///   (1) delete topics.rs line 1009 — the
    ///       `republish_pre_v2_membership(&store, &stored, &me, &client).await;` statement inside
    ///       `topic_roster` → the first expect reds;
    ///   (2) topics.rs line 967 — replace the `match join_topic(` form inside
    ///       `republish_pre_v2_membership` with any propagate-style form (e.g.
    ///       `let Ok(ev) = join_topic(..).await else { .. };`) → the `match join_topic(` expect
    ///       reds;
    ///   (3) topics.rs line 969 — move the `store_republished_membership(store, stored, &ev)`
    ///       call above the `join_topic(` call (line 967), or delete it → the ordering expect
    ///       reds;
    ///   (4) topics.rs line 978 — replace the `Err(e) => tracing::warn!` arm of the
    ///       `match join_topic(` with `Err(e) => panic!("{e}")` → the no-fatal-form assert reds.
    #[test]
    fn the_roster_open_runs_the_migration_best_effort_before_the_roster_read() {
        let src = include_str!("topics.rs");
        let code: String =
            src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");

        let at = code.find("pub async fn topic_roster(").expect("topic_roster must exist");
        let end = code[at..].find("\n}\n").expect("topic_roster must end") + at;
        let roster_body = &code[at..end];
        let migrate_at = roster_body
            .find("republish_pre_v2_membership(&store, &stored, &me, &client).await;")
            .expect("topic_roster must run the QURATOR-305 migration (republish-on-next-launch)");
        let fetch_at = roster_body.find("fetch_roster(").expect("the roster fetch must exist");
        assert!(
            migrate_at < fetch_at,
            "the republish must run BEFORE the roster read — the heal should land on this open, not the next"
        );

        let at = code.find("async fn republish_pre_v2_membership(").expect("the migration helper must exist");
        let end = code[at..].find("\n}\n").expect("the migration helper must end") + at;
        let helper_body = &code[at..end];
        assert!(
            helper_body.contains("match join_topic("),
            "the republish must REUSE join_topic (the production seal+publish), never a re-implemented publish"
        );
        let publish_at = helper_body.find("join_topic(").expect("the publish call must exist");
        let write_at = helper_body
            .find("store_republished_membership(store, stored, &ev)")
            .expect("the store overwrite must exist");
        assert!(
            publish_at < write_at,
            "the store overwrite must follow a successful publish — write-first poisons the retry (the v2 fast path would never re-run)"
        );
        // A failed publish (or a failed store write) must be logged and swallowed, never fatal to
        // the roster read — so the helper body may not contain any fatal form at all.
        assert!(
            !helper_body.contains("unwrap()") && !helper_body.contains("panic!")
                && !helper_body.contains(".expect("),
            "a failed republish must never break the roster read — log and continue (QURATOR-305 acceptance #3)"
        );
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

    /// QURATOR-332 slice A (owner ruling 2026-09-24): a topic-added contact has NEVER had its
    /// listings enumerated, so its stub must say `Pending` — never the `Fetched` honest-empty the
    /// old `Default::default()` asserted, which made Browse render a false "No public
    /// collections" for everyone auto-added by a roster until a manual refresh classified them.
    /// The background enumeration queue (enumeration_queue.rs) classifies the stub later.
    // P-10 mutation: in `upsert_topic_contact` (crates/hb-app/src/commands/topics.rs), change
    // `listings_state: ListingsStatus::Pending` back to `listings_state: Default::default()` —
    // this test reds (the stub regresses to the confident empty).
    #[test]
    fn topic_stub_is_pending_not_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let member = Identity::generate();
        let npub = npub_of(&member);
        upsert_topic_contact(&store, &npub).unwrap();
        let c = store
            .load_contact(&CachedPeer::pubkey_hash(&npub))
            .unwrap()
            .unwrap();
        assert_eq!(
            c.listings_state,
            ListingsStatus::Pending,
            "a topic stub is never-enumerated (Pending), never a confident Fetched empty"
        );
        assert_eq!(c.source, ContactSource::Topic);
        assert!(c.browse_key_hex.is_none(), "INV-2: a topic stub stays keyless");
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

    // ── QURATOR-192 — the dead-Topic verdict is PERSISTED, not re-derived on every page open ─────
    //
    // A dead public Topic was rediscovered, repainted, re-queried and re-dropped on every page open
    // because the aliveness verdict lived only in that render's TopicRank rows. The fix has three
    // pure seams, each pinned here: WHICH verdicts may be persisted (`dead_verdict_warranted` —
    // `Some(0)` only, never `None`), the store round-trip + suppression lookup
    // (`record_dead_verdicts` / `known_dead_verdict_active`), and the EXPIRY (one aliveness window,
    // so a revived Topic reappears). `topic_rank`'s loop and `topic_discover_paint`'s filter are
    // downstream of net::client — the same blocker as every command-level guard in this module — so
    // the pure halves are exercised directly, per-module convention.

    #[test]
    fn an_unknown_aliveness_is_never_persisted_as_dead() {
        // P-10 MUTATION (orchestrator) — THE LOAD-BEARING ONE: in `dead_verdict_warranted` (the
        // containing fn, just below `alive_count_for`), change
        //     alive == Some(0)
        // to
        //     alive.unwrap_or(0) == 0
        // — i.e. make the persist path store `None` as dead. THIS test reds on the FIRST assert
        // (unknown starts warranting a verdict); the two asserts below stay green, proving the gate
        // discriminates unknown from confident-dead rather than merely testing a boolean.
        assert!(
            !dead_verdict_warranted(None),
            "None = UNKNOWN (no key / a private topic / a relay error) must NEVER be persisted as \
             dead — it would bury a LIVE Topic permanently"
        );
        assert!(
            !dead_verdict_warranted(Some(3)),
            "an alive topic is not dead"
        );
        assert!(
            dead_verdict_warranted(Some(0)),
            "Some(0) — roster read with a real key, every member staler than the window — is the \
             ONE confident dead"
        );
    }

    #[test]
    fn a_persisted_dead_verdict_suppresses_its_topic_and_only_its_topic() {
        // P-10 MUTATION (orchestrator): in `record_dead_verdicts` (the containing fn), delete the
        //     verdicts.insert(id.clone(), ts);
        // line inside the for-loop — THIS test reds on the first assert (nothing is ever stamped,
        // so nothing is suppressed). The pure-gate sibling above is untouched by it and stays
        // green.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        record_dead_verdicts(&store, &["dead-topic-id".to_string()]);
        let verdicts = store.load_dead_topic_verdicts().unwrap();
        assert!(
            known_dead_verdict_active(&verdicts, "dead-topic-id"),
            "a freshly stamped verdict is honoured — the topic never reaches the UI"
        );
        assert!(
            !known_dead_verdict_active(&verdicts, "never-verdicted-id"),
            "a topic with NO verdict on file keeps its row — suppression needs a verdict"
        );
        // The stamp is a real unix-secs (not a placeholder): stamped within the last minute.
        let ts = *verdicts.get("dead-topic-id").unwrap();
        assert!(
            ts <= now() && ts + 60 > now(),
            "the verdict carries its stamping time (got {ts}, now {})", now()
        );
    }

    #[test]
    fn a_dead_verdict_expires_after_one_aliveness_window_so_a_revived_topic_reappears() {
        // P-10 MUTATION (orchestrator): in `known_dead_verdict_active` (the containing fn), remove
        // the expiry check — change
        //     .is_some_and(|&ts| now() < ts.saturating_add(hb_net::count::TOPIC_ALIVE_WINDOW_SECS))
        // to
        //     .is_some_and(|_| true)
        // — THIS test reds on the FIRST assert (the stale verdict is then honoured forever, so a
        // revived Topic can never reappear); the fresh-verdict assert stays green.
        let stale = now().saturating_sub(hb_net::count::TOPIC_ALIVE_WINDOW_SECS + 60);
        let fresh = now().saturating_sub(60);
        let verdicts = HashMap::from([
            ("stale-topic-id".to_string(), stale),
            ("fresh-topic-id".to_string(), fresh),
        ]);
        assert!(
            !known_dead_verdict_active(&verdicts, "stale-topic-id"),
            "a verdict one window + slack old has EXPIRED — the topic reappears and is re-queried, \
             so a revived one is found"
        );
        assert!(
            known_dead_verdict_active(&verdicts, "fresh-topic-id"),
            "a verdict stamped a minute ago is still honoured"
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
        /// `membership_json: None` keeps every seeded topic off the wire — its two readers,
        /// `topic_leave` and the QURATOR-305 migration, are both relay-bound and both treat
        /// `None` as nothing-to-do (the migration classifies it `Absent` ⇒ no publish, pinned by
        /// `missing_or_unparseable_records_are_no_action`).
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
