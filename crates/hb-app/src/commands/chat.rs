//! Direct messages over NIP-17 (spec §Direct Messages).
//!
//! M4 cutover: the legacy signed-envelope DM + JCS-AAD + iroh-direct/relay
//! store-and-forward path is gone. A DM is now a NIP-17 gift wrap (`hb-net::wrap_dm`) published
//! to the configured relays; the inbox fetches kind-1059 wraps addressed to us and unwraps them
//! (`hb-net::unwrap_dm`), recovering the **real sender npub** from inside the seal. The legacy
//! DM history is intentionally **not** carried forward (decided break — pre-launch zero-user).
//!
//! `send_dm_inner` is the Tauri-free seam (a pure `_inner` fn, callable without a Tauri `State`); the
//! pure decode logic (`decode_dms`) is L1-tested without a relay (the wire is proven by `hb-it` Suite
//! DM).
//!
//! **M13 Part B (Q7 owner ruling):** a stranger's DM no longer merges into the main inbox at all — it
//! is quarantined into a separate Request bucket (backed by `dm_quarantine.rs`), seen only when the
//! user opens the Request pane. `get_messages` returns the contacts-only inbox and, as a side effect,
//! persists any newly-seen stranger messages into the Request store.
//!
//! **devtest v0.12.4 #2 (incremental at-rest cache):** `get_messages` no longer re-downloads + re-
//! unwraps the whole gift-wrap mailbox every poll. Decoded contact messages are cached (sealed,
//! `dm_cache_store`), and each poll fetches only NEW wraps (`since`-bounded, dedup by wrap id). Per-DM
//! routing (blocked ▸ inbox ▸ declined ▸ request/drop) lives in `route_dm`, driven by
//! `merge_wraps_into_cache`; the returned inbox is reclassified from the cache under the current
//! contacts/blocked sets, so blocking/removing a contact still hides their cached messages.

use std::collections::HashSet;

use chrono::{TimeZone, Utc};
use nostr::prelude::*;
use serde::Serialize;
use tauri::State;

use hb_net::{unwrap_dm, wrap_dm, RelayClient};

use crate::{
    dm_cache_store::{CachedDm, DmCache},
    dm_quarantine::{merge_into_requests, record_declined, RequestMessage},
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    store::{CachedPeer, ContactSource, DataStore},
};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A decoded, sender-attributed chat message returned to the frontend. The sender is the **real**
/// npub recovered from the NIP-17 seal — never the ephemeral wrap key.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// Real sender npub (bech32).
    pub from: String,
    /// Recipient npub (bech32) — us for inbound, the peer for our sent echo.
    pub to: String,
    pub content: String,
    /// RFC3339 timestamp from the inner rumor (the real send time).
    pub sent_at: String,
}

/// Parse a DM recipient from a pasted npub or full `hbk` share code → its public key.
fn parse_recipient(s: &str) -> Result<PublicKey, String> {
    hb_core::ShareCode::parse(s)
        .map(|sc| sc.pubkey())
        .map_err(|e| format!("Invalid recipient: {e}"))
}

/// devtest #14: a self-send is never valid — `send_message` rejects it before any network I/O.
/// `route_dm`'s `from == own_npub` inbox routing stays; it exists for the legitimate sent-echo
/// of a message you already sent, not to allow creating a self-conversation from scratch.
fn is_self_send(recipient: &PublicKey, me: &PublicKey) -> bool {
    recipient == me
}

fn npub_of(pk: &PublicKey) -> String {
    pk.to_bech32().unwrap_or_else(|_| pk.to_hex())
}

fn rfc3339_of(unix_secs: u64) -> String {
    Utc.timestamp_opt(unix_secs as i64, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

// ---------------------------------------------------------------------------
// The Tauri-free seam (composes hb-net::wrap_dm / unwrap_dm over a RelayClient)
// ---------------------------------------------------------------------------

/// Build the NIP-17 gift wrap for `content` from `identity` to `recipient` (no I/O). Thin alias
/// over `hb-net::wrap_dm`, named for the seam + its L1 conformance tests.
pub(crate) async fn build_dm(
    identity: &hb_core::Identity,
    recipient: &PublicKey,
    content: &str,
) -> Result<Event, hb_net::NetError> {
    wrap_dm(identity, recipient, content).await
}

/// Send a DM: build the gift wrap and deliver it to the **recipient's NIP-65 read-relays** (their
/// inbox) ∪ your own/seed (spec §9, M12 W2). Resolves the recipient's read relays, `ensure_relays`
/// them onto the shared pool, then **targets** the publish (`publish_to`) so the wrap reaches the
/// inbox + your own relays but **not** every accreted relay (the metadata-spread guard, chorus #3).
/// **Honest limit:** if the recipient never published a NIP-65 list, delivery falls back to own/seed
/// (best-effort — works when the two parties' sets overlap). Returns the wrap.
pub(crate) async fn send_dm_inner(
    client: &RelayClient,
    identity: &hb_core::Identity,
    recipient: &PublicKey,
    content: &str,
    own_relays: &[String],
    timeout: std::time::Duration,
) -> Result<Event, hb_net::NetError> {
    let wrap = build_dm(identity, recipient, content).await?;
    let targets =
        hb_net::resolve_recipient_relays(client, recipient, own_relays, own_relays, timeout).await;
    client.ensure_relays(&targets, timeout).await?;
    client.publish_to(&wrap, &targets).await?;
    Ok(wrap)
}

/// Decode a batch of gift-wrap events into sender-attributed messages (pure; no relay). A wrap not
/// addressed to us, tampered, or malformed is **skipped with a log, never a panic**. When
/// `contact_npubs` is `Some`, messages from npubs outside the set are dropped (the `allow_dms` off
/// case). Result is sorted oldest-first by send time.
///
/// devtest v0.12.4 #2: `get_messages` now decodes via `merge_wraps_into_cache` (it needs the gift-wrap
/// event id for both the cache dedup key and the Request bucket, which this simpler contacts-only
/// filter doesn't track) — `decode_dms` has no remaining production caller, but is kept for its own
/// NIP-17 conformance tests below.
#[allow(dead_code)]
pub(crate) async fn decode_dms(
    own_npub: &str,
    identity: &hb_core::Identity,
    gift_wraps: Vec<Event>,
    contact_npubs: Option<&HashSet<String>>,
) -> Vec<ReceivedMessage> {
    let mut out: Vec<ReceivedMessage> = Vec::new();
    // Dedup by the gift-wrap **event id** — Nostr's own uniqueness key. Deduping by
    // (sender, second-granular timestamp) would silently drop distinct same-second messages from
    // the same sender (chorus M4p2 finding); each NIP-17 wrap is a distinct event with a distinct id.
    let mut seen: HashSet<EventId> = HashSet::new();
    for wrap in gift_wraps {
        if !seen.insert(wrap.id) {
            continue;
        }
        match unwrap_dm(identity, &wrap).await {
            Ok(dm) => {
                let from = npub_of(&dm.sender);
                if contact_npubs.is_some_and(|ids| !ids.contains(&from)) {
                    continue;
                }
                out.push(ReceivedMessage {
                    from,
                    to: own_npub.to_string(),
                    content: dm.content,
                    sent_at: rfc3339_of(dm.created_at),
                });
            }
            Err(e) => tracing::debug!("skipping undecryptable/foreign gift wrap: {e}"),
        }
    }
    out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    out
}

// ---------------------------------------------------------------------------
// Q7 — the total DM classifier (M13 Part B): inbox vs quarantined Request vs dropped
// ---------------------------------------------------------------------------

/// The local state `route_dm` classifies each decoded DM against — bundled (rather than four+ loose
/// params) both to keep call sites sane and because these values always travel together (loaded once
/// per `get_messages` call).
pub(crate) struct DmClassifyCtx<'a> {
    pub contacts: &'a HashSet<String>,
    pub blocked: &'a HashSet<String>,
    pub declined: &'a HashSet<String>,
    /// The `allow_dms` setting: whether a stranger's message may land in the Request inbox at all.
    pub allow_strangers: bool,
}

/// Where a decoded DM routes, in the Q7 order — the single source of truth for that ordering, used by
/// [`merge_wraps_into_cache`] (the incremental cache path, v0.12.4 #2) that `get_messages` drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmRoute {
    /// Dropped — blocked, declined, or a stranger while `allow_dms` is off.
    Drop,
    /// The contacts-only main inbox (a contact, or your own sent echo).
    Inbox,
    /// A stranger's quarantined Request bucket.
    Request,
}

/// Route one decoded sender per the Q7 ruling: **blocked** supersedes everything (even a contact);
/// then **own npub / a contact** → inbox; then a **declined** stranger stays dropped; then a stranger
/// → Request when `allow_dms` is on, else dropped (the stricter behaviour).
pub(crate) fn route_dm(from: &str, own_npub: &str, ctx: &DmClassifyCtx<'_>) -> DmRoute {
    if ctx.blocked.contains(from) {
        return DmRoute::Drop;
    }
    if from == own_npub || ctx.contacts.contains(from) {
        return DmRoute::Inbox;
    }
    if ctx.declined.contains(from) {
        return DmRoute::Drop;
    }
    if ctx.allow_strangers {
        DmRoute::Request
    } else {
        DmRoute::Drop
    }
}

/// NIP-59 fuzzes a gift wrap's OUTER `created_at` up to 2 days into the past, so an incremental fetch
/// must widen its `since` by this margin or it would silently miss a just-sent message whose outer
/// stamp landed in the past. 48 h = the NIP-59 window, so any wrap newer than the last one we saw is
/// always inside `[cursor − margin, now]` (proof: a message sent at real time T has outer ≥ T−48h;
/// with `since = cursor−48h` and T ≥ cursor, its outer ≥ since).
const DM_FETCH_MARGIN_SECS: u64 = 48 * 60 * 60;

/// The inbox fetch filter: kind-1059 wraps addressed to us, bounded by `since = cursor − margin` once
/// the cache has a cursor (an incremental read — most polls then return ~nothing), or unbounded on a
/// cold cache (the one full initial pull). `since` is **bandwidth-only** — dedup + security are by
/// wrap id and the persisted block/decline sets, never this attacker-fuzzable timestamp.
fn dm_inbox_filter(me: PublicKey, newest_seen_outer: u64) -> Filter {
    let f = Filter::new().kind(Kind::GiftWrap).pubkey(me);
    if newest_seen_outer > 0 {
        f.since(Timestamp::from(newest_seen_outer.saturating_sub(DM_FETCH_MARGIN_SECS)))
    } else {
        f
    }
}

/// Incremental decode + cache merge (devtest v0.12.4 #2). For each fetched wrap NOT already in the
/// cache's seen-ledger it unwraps (Schnorr-verified seal), records its id, and routes it via
/// [`route_dm`] — a contact/self message is appended to the cache; a stranger's is returned for the
/// caller to merge into the Q7 quarantine. A wrap that fails to unwrap is skipped with a log, never a
/// panic. **Already-seen wraps are never re-unwrapped** — the whole point of the cache.
///
/// `now` (unix secs) is the wall clock, passed in for testability. The cursor advance is **clamped to
/// `now`** and any persisted future cursor is healed down to `now` — so a foreign wrap bearing an
/// attacker-chosen future `created_at` (NIP-59's outer stamp is arbitrary) can never push `since` past
/// the present and silently stop all future DM delivery (the codebase's future-date discipline, cf.
/// M9's count cap). Uses a `HashSet` for the seen lookup (O(1), not the old O(n) scan). Returns the
/// stranger requests plus a `changed` flag: `true` iff it mutated the cache (so the caller persists a
/// balanced push+prune the length tuple would miss).
async fn merge_wraps_into_cache(
    identity: &hb_core::Identity,
    own_npub: &str,
    wraps: Vec<Event>,
    ctx: &DmClassifyCtx<'_>,
    cache: &mut DmCache,
    now: u64,
) -> (Vec<(String, RequestMessage)>, bool) {
    let mut requests: Vec<(String, RequestMessage)> = Vec::new();
    let mut changed = false;
    // Heal a poisoned/future cursor (from a prior poll before this clamp existed, or a foreign wrap):
    // it must never exceed the present, or `since = cursor − 48h` could sit in the future and starve
    // the inbox forever (the cursor only moves forward).
    if cache.newest_seen_outer > now {
        cache.newest_seen_outer = now;
        changed = true;
    }
    // O(1) "already decoded?" lookups (was an O(n) linear scan → O(n²)/poll). The persisted `seen_wraps`
    // Vec stays the source of truth; this set mirrors it plus this batch's ids.
    let mut seen: HashSet<String> = cache.seen_wraps.iter().cloned().collect();
    for wrap in wraps {
        let outer = wrap.created_at.as_u64().min(now); // clamp: a future-dated wrap can't poison the cursor
        if outer > cache.newest_seen_outer {
            cache.newest_seen_outer = outer;
            changed = true;
        }
        let wrap_id = wrap.id.to_hex();
        if seen.contains(&wrap_id) {
            continue; // already decoded (a prior poll, or the same wrap from two relays)
        }
        match unwrap_dm(identity, &wrap).await {
            Ok(dm) => {
                // Record the id only after a successful unwrap — an undecodable/foreign wrap is never
                // remembered, so it can't fill the ledger (it may be re-tried next poll, bounded by the
                // 48 h window).
                seen.insert(wrap_id.clone());
                cache.seen_wraps.push(wrap_id.clone());
                changed = true;
                let from = npub_of(&dm.sender);
                let sent_at = rfc3339_of(dm.created_at);
                match route_dm(&from, own_npub, ctx) {
                    DmRoute::Drop => {}
                    DmRoute::Inbox => cache.messages.push(CachedDm {
                        wrap_id,
                        from,
                        to: own_npub.to_string(),
                        content: dm.content,
                        sent_at,
                    }),
                    DmRoute::Request => {
                        requests.push((from, RequestMessage { wrap_id, content: dm.content, sent_at }));
                    }
                }
            }
            Err(e) => tracing::debug!("skipping undecryptable/foreign gift wrap: {e}"),
        }
    }
    (requests, changed)
}

/// The received-contact inbox, re-derived from the cache under the CURRENT contacts/blocked sets
/// (devtest v0.12.4 #2). Reclassifying at read time — never baking classification into the cache —
/// keeps §8/Q7 authoritative in the security-critical direction: blocking or removing a contact
/// **hides** their cached messages. It is deliberately one-way (it can hide, never surface): only
/// contact/self messages are cached, so a message decoded while its sender was blocked/declined/a
/// stranger stays out (consistent with drop semantics). The Q7 accept flow migrates a newly-accepted
/// sender's history into the cache explicitly (see `dm_request_accept_inner`). Deduped by wrap id,
/// sorted oldest-first by send time.
fn cached_inbox(
    cache: &DmCache,
    own_npub: &str,
    contacts: &HashSet<String>,
    blocked: &HashSet<String>,
) -> Vec<ReceivedMessage> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<ReceivedMessage> = cache
        .messages
        .iter()
        .filter(|m| (m.from == own_npub || contacts.contains(&m.from)) && !blocked.contains(&m.from))
        .filter(|m| seen.insert(m.wrap_id.as_str()))
        .map(|m| ReceivedMessage {
            from: m.from.clone(),
            to: m.to.clone(),
            content: m.content.clone(),
            sent_at: m.sent_at.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    out
}

/// A stranger's quarantined Request bucket, for the UI (Q7) — a pure local read, no relay I/O (the
/// bucket was already populated by the last `get_messages` poll).
#[derive(Debug, Clone, Serialize)]
pub struct DmRequestView {
    pub npub: String,
    pub first_seen: u64,
    pub last_message_at: u64,
    pub message_count: usize,
    pub messages: Vec<ReceivedMessage>,
    /// The §7 word+color fingerprint, derived from the npub alone (no listing access).
    pub fingerprint: Option<hb_core::fingerprint::Fingerprint>,
}

fn request_message_to_received(npub: &str, own_npub: &str, m: &RequestMessage) -> ReceivedMessage {
    ReceivedMessage { from: npub.to_string(), to: own_npub.to_string(), content: m.content.clone(), sent_at: m.sent_at.clone() }
}

pub(crate) fn dm_requests_inner(store: &DataStore, own_npub: &str) -> Result<Vec<DmRequestView>, String> {
    let buckets = store.load_dm_requests().map_err(cmd_err)?;
    Ok(buckets
        .into_iter()
        .map(|b| {
            let fingerprint =
                hb_core::identity::parse_npub(&b.npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk));
            let messages =
                b.messages.iter().map(|m| request_message_to_received(&b.npub, own_npub, m)).collect();
            DmRequestView {
                message_count: b.messages.len(),
                messages,
                npub: b.npub,
                first_seen: b.first_seen,
                last_message_at: b.last_message_at,
                fingerprint,
            }
        })
        .collect())
}

/// Accept a stranger's Request bucket (Q7): adds them as a Manual, browse-key-less contact — built
/// locally rather than via a relay round-trip (`browse::resolve_peer` is a different module's owned
/// code and isn't a relay-free path anyway), so **acceptance never depends on network reachability**.
/// Deletes the bucket, un-declines the sender if they were previously declined, and returns the
/// drained messages so the caller can seed them straight into the conversation.
///
/// devtest v0.12.4 #2 fix: the accepted messages are also **migrated into the DM cache** (their wraps
/// are already in `seen_wraps` from when they were quarantined, so the incremental poll would never
/// re-decode them into the now-contact's inbox — without this, the accepted history would flash for
/// one poll then vanish). Needs `identity` to re-seal the cache.
pub(crate) fn dm_request_accept_inner(
    store: &DataStore,
    identity: &hb_core::Identity,
    own_npub: &str,
    npub: String,
    petname: Option<String>,
) -> Result<Vec<ReceivedMessage>, String> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let fingerprint = hb_core::identity::parse_npub(&npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk));
    let peer = CachedPeer {
        npub: npub.clone(),
        source: ContactSource::Manual,
        browse_key_hex: None,
        petname,
        profile: None,
        collections: vec![],
        online: false,
        last_fetched: chrono::Utc::now(),
        local_tags: vec![],
        fingerprint,
    };
    store.save_contact(&hash, &peer).map_err(cmd_err)?;

    let mut buckets = store.load_dm_requests().map_err(cmd_err)?;
    let (drained, cache_adds) = match buckets.iter().position(|b| b.npub == npub) {
        Some(i) => {
            let bucket = buckets.remove(i);
            let drained: Vec<ReceivedMessage> =
                bucket.messages.iter().map(|m| request_message_to_received(&npub, own_npub, m)).collect();
            let cache_adds: Vec<CachedDm> = bucket
                .messages
                .iter()
                .map(|m| CachedDm {
                    wrap_id: m.wrap_id.clone(),
                    from: npub.clone(),
                    to: own_npub.to_string(),
                    content: m.content.clone(),
                    sent_at: m.sent_at.clone(),
                })
                .collect();
            (drained, cache_adds)
        }
        None => (Vec::new(), Vec::new()),
    };
    store.save_dm_requests(&buckets).map_err(cmd_err)?;

    let declined: Vec<(String, u64)> =
        store.load_dm_declined().map_err(cmd_err)?.into_iter().filter(|(n, _)| n != &npub).collect();
    store.save_dm_declined(&declined).map_err(cmd_err)?;

    // Migrate the accepted history into the DM cache so it persists past the next poll (see doc above).
    if !cache_adds.is_empty() {
        let mut cache = store.load_dm_cache(identity).map_err(cmd_err)?;
        cache.messages.extend(cache_adds);
        cache.prune();
        store.save_dm_cache(identity, &cache).map_err(cmd_err)?;
    }

    Ok(drained)
}

/// Decline a stranger's Request bucket: delete the bucket and remember the decline **permanently**
/// (until the sender becomes a contact via a normal add). The Request inbox is re-derived from relay
/// history on every poll and the inner rumor timestamp is attacker-controlled, so a watermark-style
/// "seen up to T" can't tell "already declined" apart from "arrived after I declined" — remembering
/// the decline outright is the only reading of the ruling that stays stable across restarts/re-polls.
pub(crate) fn dm_request_decline_inner(store: &DataStore, npub: String, now: u64) -> Result<(), String> {
    let mut buckets = store.load_dm_requests().map_err(cmd_err)?;
    buckets.retain(|b| b.npub != npub);
    store.save_dm_requests(&buckets).map_err(cmd_err)?;

    let declined = store.load_dm_declined().map_err(cmd_err)?;
    store.save_dm_declined(&record_declined(declined, npub, now)).map_err(cmd_err)
}

/// Add `npub` to the local blocklist (spec §Blocked keys — the canonical local blocklist, named for
/// future Settings reuse). Deletes any Request bucket and any decline record — blocked supersedes both.
pub(crate) fn dm_block_inner(store: &DataStore, npub: String) -> Result<(), String> {
    let mut buckets = store.load_dm_requests().map_err(cmd_err)?;
    buckets.retain(|b| b.npub != npub);
    store.save_dm_requests(&buckets).map_err(cmd_err)?;

    let declined: Vec<(String, u64)> =
        store.load_dm_declined().map_err(cmd_err)?.into_iter().filter(|(n, _)| n != &npub).collect();
    store.save_dm_declined(&declined).map_err(cmd_err)?;

    let mut blocked = store.load_dm_blocked().map_err(cmd_err)?;
    if !blocked.contains(&npub) {
        blocked.push(npub);
    }
    store.save_dm_blocked(&blocked).map_err(cmd_err)
}

pub(crate) fn dm_unblock_inner(store: &DataStore, npub: String) -> Result<(), String> {
    let blocked: Vec<String> =
        store.load_dm_blocked().map_err(cmd_err)?.into_iter().filter(|n| n != &npub).collect();
    store.save_dm_blocked(&blocked).map_err(cmd_err)
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Encrypt + send a chat message to `to` (an npub or full share code) over NIP-17.
#[tauri::command]
pub async fn send_message(
    to: String,
    content: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<ReceivedMessage> {
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Err("Message cannot be empty".into());
    }
    if trimmed.len() > 4096 {
        return Err(format!("Message too long ({} chars, max 4096)", trimmed.len()));
    }

    let recipient = parse_recipient(&to)?;

    let (from, id_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.npub(), id.identity.clone())
    };

    if is_self_send(&recipient, &id_clone.public_key()) {
        return Err("You can't send a message to yourself.".into());
    }

    let own = net::relay_urls(&store);
    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    send_dm_inner(&client, &id_clone, &recipient, &trimmed, &own, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;

    Ok(ReceivedMessage {
        from,
        to: npub_of(&recipient),
        content: trimmed,
        sent_at: Utc::now().to_rfc3339(),
    })
}

/// The constant tag identifying a DM as a manifest request (`content.hb`).
const MANIFEST_REQUEST_TAG: &str = "manifest_request";

/// M16 W4 — the structured "get the rest" request a browser DMs to the hoarder. Rides an ordinary
/// NIP-17 DM as JSON `content` (one relay write); the hoarder's inbox renders it as a normal message
/// with a light hint. Hoardbook never auto-produces a manifest or a ticket — a human decides (the
/// blessed "ask by DM" seam; there is no Download button, MAS-INV-5).
#[derive(Debug, Clone, Serialize)]
struct ManifestRequest {
    /// Always `MANIFEST_REQUEST_TAG` — how the hoarder-side inbox recognises the request.
    hb: &'static str,
    slug: String,
    /// The snapshot fingerprint of the teaser the requester saw (lets the hoarder confirm the version).
    fingerprint_seen: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    teaser_event_id: Option<String>,
    /// The requester's Mascara pubkey, if any — opaque to Hoardbook (neither minted nor validated).
    #[serde(skip_serializing_if = "Option::is_none")]
    mascara_pubkey: Option<String>,
}

/// Build the manifest-request DM body (canonical JSON). Pure — unit-tested without a relay.
fn build_manifest_request(
    slug: &str,
    fingerprint_seen: &str,
    teaser_event_id: Option<String>,
    mascara_pubkey: Option<String>,
) -> Result<String, String> {
    let req = ManifestRequest {
        hb: MANIFEST_REQUEST_TAG,
        slug: slug.to_string(),
        fingerprint_seen: fingerprint_seen.to_string(),
        teaser_event_id,
        mascara_pubkey,
    };
    serde_json::to_string(&req).map_err(cmd_err)
}

/// M16 W4 — DM the hoarder a structured request for the full manifest of a truncated collection (the
/// blessed "ask by DM" seam, MASCARA_SPEC Q1). One relay write; the hoarder decides whether to export
/// + ticket it — Hoardbook never auto-produces anything.
// The 5 request fields + 3 injected Tauri `State` handles are all load-bearing (mirrors `send_message`).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn request_manifest(
    npub: String,
    slug: String,
    fingerprint_seen: String,
    teaser_event_id: Option<String>,
    mascara_pubkey: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let recipient = parse_recipient(&npub)?;
    let id_clone = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        id.identity.clone()
    };
    if is_self_send(&recipient, &id_clone.public_key()) {
        return Err("You can't request a manifest from yourself.".into());
    }
    let content = build_manifest_request(&slug, &fingerprint_seen, teaser_event_id, mascara_pubkey)?;
    let own = net::relay_urls(&store);
    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    send_dm_inner(&client, &id_clone, &recipient, &content, &own, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;
    Ok(())
}

/// Fetch + decrypt the NIP-17 inbox: contacts' messages only (Q7 — a stranger's DM never reaches the
/// main inbox at all). As a side effect, persists any newly-seen stranger messages into the quarantined
/// Request store (`dm_requests`); `allow_dms=false` preserves the stricter drop-everything behaviour.
#[tauri::command]
pub async fn get_messages(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<ReceivedMessage>> {
    let (own_npub, id_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded.")?;
        (id.npub(), id.identity.clone())
    };

    let allow_dms = store.load_settings().map_err(cmd_err)?.map(|s| s.allow_dms).unwrap_or(true);
    let contacts: HashSet<String> = store.list_contacts().map_err(cmd_err)?.into_iter().map(|c| c.npub).collect();
    let blocked: HashSet<String> = store.load_dm_blocked().map_err(cmd_err)?.into_iter().collect();
    let declined: HashSet<String> =
        store.load_dm_declined().map_err(cmd_err)?.into_iter().map(|(n, _)| n).collect();

    let ctx = DmClassifyCtx { contacts: &contacts, blocked: &blocked, declined: &declined, allow_strangers: allow_dms };

    // devtest v0.12.4 #2: load the at-rest cache and fetch only wraps newer than what we've already
    // decoded — a `since`-bounded incremental read on the persistent shared client, not the old
    // whole-mailbox pull + full re-decrypt every poll. Received contact messages come from the cache
    // (instant); the relay is touched only for genuinely-new wraps.
    let now = now_secs();
    let mut cache = store.load_dm_cache(&id_clone).map_err(cmd_err)?;
    // Heal a poisoned/future cursor before it drives the fetch window (a stale install may carry one).
    let healed = cache.newest_seen_outer > now;
    if healed {
        cache.newest_seen_outer = now;
    }
    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    let filter = dm_inbox_filter(id_clone.public_key(), cache.newest_seen_outer);
    let wraps = client.fetch(filter, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;

    let (requests, merged) = merge_wraps_into_cache(&id_clone, &own_npub, wraps, &ctx, &mut cache, now).await;
    let pruned = cache.prune();
    // Only re-seal + write when something actually changed — an idle 3s poll (all wraps already seen)
    // leaves the cache untouched, so it costs no disk write / re-encrypt. The explicit dirty flags
    // catch a balanced push+prune the length tuple alone would miss.
    if healed || merged || pruned {
        store.save_dm_cache(&id_clone, &cache).map_err(cmd_err)?;
    }

    if !requests.is_empty() {
        let existing = store.load_dm_requests().map_err(cmd_err)?;
        let merged = merge_into_requests(existing, requests, now_secs());
        store.save_dm_requests(&merged).map_err(cmd_err)?;
    }

    // Return the received-contact inbox from the cache, reclassified under the current contacts/blocked
    // sets (so a since-blocked/removed contact's cached messages are hidden — §8/Q7 stays authoritative).
    Ok(cached_inbox(&cache, &own_npub, &contacts, &blocked))
}

// ---------------------------------------------------------------------------
// Q7 — the Request-inbox Tauri command surface (thin wrappers over the `_inner` fns above)
// ---------------------------------------------------------------------------

/// List the quarantined Request buckets — a pure local read, no relay I/O.
#[tauri::command]
pub async fn dm_requests(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<DmRequestView>> {
    let own_npub = identity.read().await.as_ref().map(|id| id.npub()).ok_or("No identity loaded.")?;
    dm_requests_inner(&store, &own_npub)
}

#[tauri::command]
pub async fn dm_request_accept(
    npub: String,
    petname: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<ReceivedMessage>> {
    let (own_npub, id_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded.")?;
        (id.npub(), id.identity.clone())
    };
    dm_request_accept_inner(&store, &id_clone, &own_npub, npub, petname)
}

#[tauri::command]
pub async fn dm_request_decline(npub: String, store: State<'_, DataStore>) -> CmdResult<()> {
    dm_request_decline_inner(&store, npub, now_secs())
}

#[tauri::command]
pub async fn dm_block(npub: String, store: State<'_, DataStore>) -> CmdResult<()> {
    dm_block_inner(&store, npub)
}

#[tauri::command]
pub async fn dm_unblock(npub: String, store: State<'_, DataStore>) -> CmdResult<()> {
    dm_unblock_inner(&store, npub)
}

#[tauri::command]
pub async fn dm_blocked_list(store: State<'_, DataStore>) -> CmdResult<Vec<String>> {
    store.load_dm_blocked().map_err(cmd_err)
}

// ---------------------------------------------------------------------------
// Read state (devtest #16) — the unified per-peer last-read watermark
// ---------------------------------------------------------------------------

/// The per-peer last-read watermark (npub → RFC3339 `sent_at` of the newest seen message) — a pure
/// local read, no relay I/O.
#[tauri::command]
pub async fn get_read_state(
    store: State<'_, DataStore>,
) -> CmdResult<std::collections::HashMap<String, String>> {
    store.load_read_state().map_err(cmd_err)
}

/// Advance `npub`'s read watermark to `sent_at` (never rewinds — see `DataStore::advance_read_watermark`).
#[tauri::command]
pub async fn advance_read_watermark(
    npub: String,
    sent_at: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    store.advance_read_watermark(&npub, &sent_at).map_err(cmd_err)
}

// ---------------------------------------------------------------------------
// Tests — the DM seam (L1, no relay; the wire is proven by hb-it Suite DM)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dm_quarantine::DmRequestBucket;
    use hb_core::Identity;

    #[test]
    fn is_self_send_rejects_own_pubkey_devtest_14() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        assert!(is_self_send(&me.public_key(), &me.public_key()));
        assert!(!is_self_send(&stranger.public_key(), &me.public_key()));
    }

    #[test]
    fn manifest_request_json_is_tagged_and_omits_absent_options() {
        // M16 W4: the DM body is `{hb:"manifest_request", slug, fingerprint_seen}` — the frontend
        // detects the tag and renders a light hint. Absent optional fields are omitted (not null).
        let json = build_manifest_request("criterion", "abc123", None, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["hb"], MANIFEST_REQUEST_TAG);
        assert_eq!(v["slug"], "criterion");
        assert_eq!(v["fingerprint_seen"], "abc123");
        assert!(v.get("teaser_event_id").is_none());
        assert!(v.get("mascara_pubkey").is_none());
    }

    #[test]
    fn manifest_request_json_carries_present_options() {
        let json =
            build_manifest_request("s", "fp", Some("evt1".into()), Some("mpub".into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["teaser_event_id"], "evt1");
        assert_eq!(v["mascara_pubkey"], "mpub");
    }

    #[tokio::test]
    async fn send_dm_inner_produces_a_nip17_giftwrap() {
        // build_dm (the no-I/O half of send_dm_inner) yields a kind-1059 gift wrap signed by an
        // ephemeral key — never the sender's npub (DM2).
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = build_dm(&alice, &bob.public_key(), "back room is open").await.unwrap();
        assert_eq!(wrap.kind, Kind::GiftWrap, "DM wrap must be kind 1059");
        assert_ne!(wrap.pubkey, alice.public_key(), "wrap must not be signed by the sender");
    }

    #[tokio::test]
    async fn send_dm_inner_inner_rumor_is_kind_14() {
        // NIP-17 conformance: the sealed inner rumor is an unsigned kind-14 (PrivateDirectMessage)
        // event. A round-trip test alone could pass on a non-conformant inner event a real NIP-17
        // peer would reject. The recovered sender is the real npub, not the ephemeral wrap key.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = build_dm(&alice, &bob.public_key(), "hi").await.unwrap();
        let unwrapped = nostr::nips::nip59::extract_rumor(bob.keys(), &wrap).await.unwrap();
        assert_eq!(
            unwrapped.rumor.kind,
            Kind::PrivateDirectMessage,
            "inner rumor must be kind 14 (private direct message)"
        );
        assert_eq!(unwrapped.sender, alice.public_key(), "rumor sender is the real npub");
    }

    #[tokio::test]
    async fn fetch_dms_inner_unwraps_to_sender_and_plaintext() {
        // decode_dms recovers the REAL sender npub + plaintext from the seal.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = build_dm(&alice, &bob.public_key(), "secret tape list").await.unwrap();
        let msgs = decode_dms(&bob.npub(), &bob, vec![wrap], None).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from, alice.npub(), "from is the real sender npub");
        assert_eq!(msgs[0].to, bob.npub());
        assert_eq!(msgs[0].content, "secret tape list");
    }

    #[tokio::test]
    async fn fetch_dms_inner_rejects_malformed_giftwrap_not_panicked() {
        // A corrupt/foreign gift wrap from a hostile relay → skipped with a reason, never a panic.
        let alice = Identity::generate();
        let bob = Identity::generate();
        // A plain text note is not a gift wrap addressed to bob.
        let garbage = alice.sign(EventBuilder::new(Kind::TextNote, "not a wrap")).unwrap();
        let real = build_dm(&alice, &bob.public_key(), "real").await.unwrap();
        let msgs = decode_dms(&bob.npub(), &bob, vec![garbage, real], None).await;
        assert_eq!(msgs.len(), 1, "only the real DM decodes; the garbage is skipped");
        assert_eq!(msgs[0].content, "real");
    }

    #[tokio::test]
    async fn decode_dms_honours_contact_allow_list() {
        // allow_dms off: a stranger's DM is filtered out; a contact's is kept.
        let me = Identity::generate();
        let contact = Identity::generate();
        let stranger = Identity::generate();
        let from_contact = build_dm(&contact, &me.public_key(), "hey").await.unwrap();
        let from_stranger = build_dm(&stranger, &me.public_key(), "spam").await.unwrap();
        let allow: HashSet<String> = [contact.npub()].into_iter().collect();
        let msgs =
            decode_dms(&me.npub(), &me, vec![from_contact, from_stranger], Some(&allow)).await;
        assert_eq!(msgs.len(), 1, "only the contact's DM survives the allow-list");
        assert_eq!(msgs[0].from, contact.npub());
    }

    #[tokio::test]
    async fn decode_dms_keeps_distinct_same_sender_messages() {
        // chorus M4p2 finding: dedup must key on the gift-wrap event id, not (sender, second). Two
        // distinct DMs from the same sender (each a distinct NIP-17 wrap) must both survive, even
        // when their inner timestamps land in the same second.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let a = build_dm(&alice, &bob.public_key(), "first").await.unwrap();
        let b = build_dm(&alice, &bob.public_key(), "second").await.unwrap();
        assert_ne!(a.id, b.id, "distinct messages are distinct wraps");
        let msgs = decode_dms(&bob.npub(), &bob, vec![a.clone(), b, a], None).await;
        // Two distinct messages survive; the re-delivered duplicate of `a` is collapsed by id.
        assert_eq!(msgs.len(), 2, "both distinct messages kept; the duplicate wrap deduped");
        let contents: HashSet<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains("first") && contents.contains("second"));
    }

    #[test]
    fn dm_path_no_longer_builds_a_signed_envelope() {
        // The legacy DM payload is gone: ReceivedMessage carries only npub-attributed fields, with
        // no `encrypted` flag and no JCS-AAD concept. Asserted by the serialized shape.
        let msg = ReceivedMessage {
            from: "npub1from".into(),
            to: "npub1to".into(),
            content: "x".into(),
            sent_at: "2026-06-17T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("encrypted"), "no legacy `encrypted` flag");
        assert!(json.contains("\"from\":\"npub1from\""));
    }

    // ── Q7 / v0.12.4 #2 — DM routing + the incremental at-rest cache ────────────────────────────

    /// Test-only builder for [`DmClassifyCtx`] — most tests only vary one or two of its four fields.
    fn ctx<'a>(
        contacts: &'a HashSet<String>,
        blocked: &'a HashSet<String>,
        declined: &'a HashSet<String>,
        allow_strangers: bool,
    ) -> DmClassifyCtx<'a> {
        DmClassifyCtx { contacts, blocked, declined, allow_strangers }
    }


    #[test]
    fn route_dm_follows_the_q7_order() {
        let contacts: HashSet<String> = ["c".into()].into_iter().collect();
        let blocked: HashSet<String> = ["b".into()].into_iter().collect();
        let declined: HashSet<String> = ["d".into()].into_iter().collect();
        let on = ctx(&contacts, &blocked, &declined, true);
        assert_eq!(route_dm("b", "me", &on), DmRoute::Drop, "blocked supersedes all");
        assert_eq!(route_dm("c", "me", &on), DmRoute::Inbox, "a contact → inbox");
        assert_eq!(route_dm("me", "me", &on), DmRoute::Inbox, "own npub → inbox");
        assert_eq!(route_dm("d", "me", &on), DmRoute::Drop, "declined → drop");
        assert_eq!(route_dm("s", "me", &on), DmRoute::Request, "stranger → request when allow_dms on");
        // A blocked CONTACT still drops (blocked supersedes contact).
        let bc: HashSet<String> = ["x".into()].into_iter().collect();
        let empty: HashSet<String> = HashSet::new();
        let both = ctx(&bc, &bc, &empty, true);
        assert_eq!(route_dm("x", "me", &both), DmRoute::Drop);
        // allow_dms off → a stranger drops entirely.
        let off = ctx(&contacts, &blocked, &declined, false);
        assert_eq!(route_dm("s", "me", &off), DmRoute::Drop);
    }

    #[tokio::test]
    async fn merge_caches_contacts_quarantines_strangers_then_skips_seen_wraps() {
        let me = Identity::generate();
        let contact = Identity::generate();
        let stranger = Identity::generate();
        let from_contact = build_dm(&contact, &me.public_key(), "hey").await.unwrap();
        let from_stranger = build_dm(&stranger, &me.public_key(), "spam").await.unwrap();
        let contacts: HashSet<String> = [contact.npub()].into_iter().collect();
        let empty: HashSet<String> = HashSet::new();
        let ctxv = ctx(&contacts, &empty, &empty, true);

        let now = now_secs();
        let mut cache = DmCache::default();
        let (requests, changed) = merge_wraps_into_cache(
            &me,
            &me.npub(),
            vec![from_contact.clone(), from_stranger.clone()],
            &ctxv,
            &mut cache,
            now,
        )
        .await;
        assert!(changed, "decoding new wraps marks the cache dirty");
        assert_eq!(cache.messages.len(), 1, "the contact's DM is cached");
        assert_eq!(cache.messages[0].from, contact.npub());
        assert_eq!(requests.len(), 1, "the stranger becomes a request, not a cache entry");
        assert_eq!(requests[0].0, stranger.npub());
        assert_eq!(cache.seen_wraps.len(), 2, "both wraps recorded in the seen ledger");
        assert!(cache.newest_seen_outer > 0, "the cursor advanced from the outer timestamps");
        assert!(cache.newest_seen_outer <= now, "the cursor never exceeds the present");

        // Second pass with the SAME wraps: nothing is re-decoded — no duplicate cache entry, no new
        // request, cache reports unchanged. This is the fix for #2 (the old path re-unwrapped the
        // whole mailbox every poll).
        let (requests2, changed2) =
            merge_wraps_into_cache(&me, &me.npub(), vec![from_contact, from_stranger], &ctxv, &mut cache, now).await;
        assert!(requests2.is_empty(), "already-seen wraps are never re-decoded");
        assert!(!changed2, "an all-seen re-poll leaves the cache untouched (no needless re-seal/write)");
        assert_eq!(cache.messages.len(), 1, "no duplicate cache entry on re-poll");
        assert_eq!(cache.seen_wraps.len(), 2, "seen ledger unchanged");
    }

    #[tokio::test]
    async fn future_dated_foreign_wrap_cannot_poison_the_since_cursor() {
        // Review #2: a foreign kind-1059 with an attacker-chosen far-future outer created_at must NOT
        // push the cursor past `now` (which would make since = cursor−48h sit in the future and starve
        // the inbox forever). The clamp caps the advance to `now`; the garbage wrap fails unwrap anyway.
        let me = Identity::generate();
        let attacker = Identity::generate();
        let now = now_secs();
        // A wrap addressed to someone else (so it won't unwrap for `me`), stamped 10 days in the future.
        let future = Timestamp::from(now + 10 * 24 * 60 * 60);
        let poison = attacker
            .sign(EventBuilder::new(Kind::GiftWrap, "junk").custom_created_at(future))
            .unwrap();
        let empty: HashSet<String> = HashSet::new();
        let ctxv = ctx(&empty, &empty, &empty, true);
        let mut cache = DmCache::default();
        let (requests, _) = merge_wraps_into_cache(&me, &me.npub(), vec![poison], &ctxv, &mut cache, now).await;
        assert!(requests.is_empty(), "a foreign wrap creates no request");
        assert!(cache.newest_seen_outer <= now, "the cursor is clamped to now, not the future stamp");
        // And a pre-existing poisoned cursor is healed downward on the next merge.
        cache.newest_seen_outer = now + 999_999;
        let (_, changed) = merge_wraps_into_cache(&me, &me.npub(), vec![], &ctxv, &mut cache, now).await;
        assert!(changed, "healing the cursor marks the cache dirty (so it is persisted)");
        assert_eq!(cache.newest_seen_outer, now, "a persisted future cursor heals down to now");
    }

    #[test]
    fn cached_inbox_reclassifies_under_current_contacts_and_block() {
        let own = "npub1me";
        let mk = |id: &str, from: &str, at: &str| CachedDm {
            wrap_id: id.into(),
            from: from.into(),
            to: own.into(),
            content: "x".into(),
            sent_at: at.into(),
        };
        let cache = DmCache {
            messages: vec![
                mk("w2", "npub1a", "2026-01-02T00:00:00Z"),
                mk("w1", "npub1a", "2026-01-01T00:00:00Z"),
                mk("w3", "npub1b", "2026-01-03T00:00:00Z"),
            ],
            ..Default::default()
        };
        let contacts: HashSet<String> = ["npub1a".into(), "npub1b".into()].into_iter().collect();
        // No block: all shown, sorted oldest-first.
        let inbox = cached_inbox(&cache, own, &contacts, &HashSet::new());
        assert_eq!(
            inbox.iter().map(|m| m.sent_at.as_str()).collect::<Vec<_>>(),
            ["2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z"]
        );
        // Block npub1a AFTER their messages were cached → they vanish from the returned inbox.
        let blocked: HashSet<String> = ["npub1a".into()].into_iter().collect();
        let inbox2 = cached_inbox(&cache, own, &contacts, &blocked);
        assert_eq!(inbox2.len(), 1);
        assert_eq!(inbox2[0].from, "npub1b");
        // A non-contact's cached messages never surface (e.g. after removing them).
        assert!(cached_inbox(&cache, own, &HashSet::new(), &HashSet::new()).is_empty());
    }

    // ── Q7 — the Request-inbox `_inner` fns (no Tauri State) ────────────────────────────────────

    #[test]
    fn request_accept_adds_manual_contact_no_browse_key_and_drains_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let npub = "npub1stranger".to_string();
        store
            .save_dm_requests(&[DmRequestBucket {
                npub: npub.clone(),
                first_seen: 1,
                last_message_at: 5,
                messages: vec![
                    RequestMessage { wrap_id: "w1".into(), content: "hi".into(), sent_at: "2026-01-01T00:00:00Z".into() },
                    RequestMessage { wrap_id: "w2".into(), content: "there".into(), sent_at: "2026-01-01T00:01:00Z".into() },
                ],
            }])
            .unwrap();
        // Seed a prior decline for this sender — accept must clear it (they're no longer declined).
        store.save_dm_declined(&[(npub.clone(), 1)]).unwrap();

        let drained = dm_request_accept_inner(&store, &me, "npub1me", npub.clone(), None).unwrap();
        assert_eq!(drained.len(), 2, "both messages are drained into the conversation");
        assert_eq!(drained[0].content, "hi");

        let contact = store.load_contact(&CachedPeer::pubkey_hash(&npub)).unwrap().unwrap();
        assert_eq!(contact.source, ContactSource::Manual);
        assert!(contact.browse_key_hex.is_none(), "an accepted request contact carries no browse-key");
        assert!(contact.petname.is_none(), "petname=None leaves the default unset");
        assert!(store.load_dm_requests().unwrap().is_empty(), "the bucket is gone after accept");
        assert!(
            !store.load_dm_declined().unwrap().iter().any(|(n, _)| n == &npub),
            "accept clears any prior decline for this sender"
        );

        // devtest v0.12.4 #2 regression: the accepted history is migrated into the DM cache so it
        // survives the next incremental poll (the wraps are already in seen_wraps and would never
        // re-decode into the now-contact's inbox — without this the conversation flashes then vanishes).
        let cache = store.load_dm_cache(&me).unwrap();
        assert_eq!(cache.messages.len(), 2, "accepted messages are cached, not lost after one poll");
        assert!(cache.messages.iter().all(|m| m.from == npub && m.to == "npub1me"));
        assert!(cache.messages.iter().any(|m| m.wrap_id == "w1" && m.content == "hi"));

        // Some(petname) sets it.
        let npub2 = "npub1stranger2".to_string();
        store
            .save_dm_requests(&[DmRequestBucket { npub: npub2.clone(), first_seen: 1, last_message_at: 1, messages: vec![] }])
            .unwrap();
        dm_request_accept_inner(&store, &me, "npub1me", npub2.clone(), Some("Bob".into())).unwrap();
        let contact2 = store.load_contact(&CachedPeer::pubkey_hash(&npub2)).unwrap().unwrap();
        assert_eq!(contact2.petname.as_deref(), Some("Bob"));
    }

    #[test]
    fn request_decline_persists_and_block_removes_bucket_and_declined() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let npub = "npub1stranger".to_string();
        store
            .save_dm_requests(&[DmRequestBucket {
                npub: npub.clone(),
                first_seen: 1,
                last_message_at: 1,
                messages: vec![RequestMessage { wrap_id: "w1".into(), content: "hi".into(), sent_at: "t".into() }],
            }])
            .unwrap();

        dm_request_decline_inner(&store, npub.clone(), 100).unwrap();
        assert!(store.load_dm_requests().unwrap().is_empty(), "the bucket is gone after decline");
        let declined = store.load_dm_declined().unwrap();
        assert!(declined.iter().any(|(n, _)| n == &npub), "the decline is remembered");

        // Re-seed a bucket (as if the stranger messaged again) and block instead.
        store
            .save_dm_requests(&[DmRequestBucket { npub: npub.clone(), first_seen: 2, last_message_at: 2, messages: vec![] }])
            .unwrap();
        dm_block_inner(&store, npub.clone()).unwrap();
        assert!(store.load_dm_requests().unwrap().is_empty(), "block removes any bucket");
        assert!(store.load_dm_declined().unwrap().is_empty(), "block also clears the decline record (blocked supersedes)");
        assert!(store.load_dm_blocked().unwrap().contains(&npub));

        dm_unblock_inner(&store, npub.clone()).unwrap();
        assert!(!store.load_dm_blocked().unwrap().contains(&npub));
    }
}
