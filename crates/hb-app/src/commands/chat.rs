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
//! is quarantined into a separate Request bucket (`classify_dms`, backed by `dm_quarantine.rs`), seen
//! only when the user opens the Request pane. `get_messages` keeps its signature but its semantics
//! changed: it now returns the contacts-only inbox and, as a side effect, persists any newly-seen
//! stranger messages into the Request store. `classify_dms` needs the gift-wrap event id (the
//! Request-bucket dedup key), which `decode_dms`'s output doesn't carry, so it re-implements the
//! unwrap loop rather than composing over `decode_dms`; `decode_dms` itself is kept — and still
//! covered by its own conformance tests below — as the simpler contacts-only-filter seam.

use std::collections::HashSet;

use chrono::{TimeZone, Utc};
use nostr::prelude::*;
use serde::Serialize;
use tauri::State;

use hb_net::{unwrap_dm, wrap_dm, RelayClient};

use crate::{
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
/// M13 Part B: `get_messages` now calls [`classify_dms`] instead (it needs the gift-wrap event id for
/// the Request-bucket dedup key, which this simpler contacts-only filter doesn't track) — `decode_dms`
/// has no remaining production caller, but is kept for its own NIP-17 conformance tests below.
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

/// The two destinations a batch of gift wraps splits into: the contacts-only main inbox, and the
/// stranger messages awaiting a Request-bucket merge (npub, message).
pub(crate) struct ClassifiedDms {
    pub inbox: Vec<ReceivedMessage>,
    pub requests: Vec<(String, RequestMessage)>,
}

/// The local state `classify_dms` classifies against — bundled (rather than four+ loose params) both
/// to keep the function's argument count sane and because these values always travel together (loaded
/// once per `get_messages` call).
pub(crate) struct DmClassifyCtx<'a> {
    pub contacts: &'a HashSet<String>,
    pub blocked: &'a HashSet<String>,
    pub declined: &'a HashSet<String>,
    /// The `allow_dms` setting: whether a stranger's message may land in the Request inbox at all.
    pub allow_strangers: bool,
}

/// Classify a batch of gift-wrap events (Q7 owner ruling — the message-requests pattern). Each
/// message is routed to **exactly one** destination, checked in this order:
///
/// 1. **blocked** sender → dropped, never creates a Request (blocked supersedes contact — even a
///    blocked EXISTING contact's messages are dropped here);
/// 2. **own npub or a contact** → the main inbox;
/// 3. **declined** sender → dropped (a decline is remembered permanently — see `dm_request_decline`);
/// 4. **stranger** → a Request-bucket entry when `ctx.allow_strangers` (the `allow_dms` setting), else
///    dropped (the stricter `allow_dms=false` behaviour, preserved as-is).
///
/// A wrap that fails to unwrap (foreign/tampered/malformed) is skipped with a log, never a panic —
/// the same posture as [`decode_dms`]. Deduped by the gift-wrap event id, same rationale as there.
pub(crate) async fn classify_dms(
    own_npub: &str,
    identity: &hb_core::Identity,
    gift_wraps: Vec<Event>,
    ctx: &DmClassifyCtx<'_>,
) -> ClassifiedDms {
    let mut inbox: Vec<ReceivedMessage> = Vec::new();
    let mut requests: Vec<(String, RequestMessage)> = Vec::new();
    let mut seen: HashSet<EventId> = HashSet::new();
    for wrap in gift_wraps {
        if !seen.insert(wrap.id) {
            continue;
        }
        let wrap_id = wrap.id.to_hex();
        match unwrap_dm(identity, &wrap).await {
            Ok(dm) => {
                let from = npub_of(&dm.sender);
                if ctx.blocked.contains(&from) {
                    continue; // blocked supersedes everything — never even reaches a request
                }
                if from == own_npub || ctx.contacts.contains(&from) {
                    inbox.push(ReceivedMessage {
                        from,
                        to: own_npub.to_string(),
                        content: dm.content,
                        sent_at: rfc3339_of(dm.created_at),
                    });
                    continue;
                }
                if ctx.declined.contains(&from) {
                    continue; // permanently remembered — see dm_request_decline
                }
                if ctx.allow_strangers {
                    requests.push((
                        from,
                        RequestMessage { wrap_id, content: dm.content, sent_at: rfc3339_of(dm.created_at) },
                    ));
                }
                // else: allow_dms is off — a stranger's DM is fully dropped (the stricter behaviour).
            }
            Err(e) => tracing::debug!("skipping undecryptable/foreign gift wrap: {e}"),
        }
    }
    inbox.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    ClassifiedDms { inbox, requests }
}

/// Fetch + classify the NIP-17 inbox in one relay read: the same filter/fetch shape [`send_dm_inner`]'s
/// sibling used to have, now composed over [`classify_dms`] instead of [`decode_dms`].
pub(crate) async fn classify_dms_inner(
    client: &RelayClient,
    identity: &hb_core::Identity,
    own_npub: &str,
    ctx: &DmClassifyCtx<'_>,
    timeout: std::time::Duration,
) -> Result<ClassifiedDms, hb_net::NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(identity.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    Ok(classify_dms(own_npub, identity, wraps, ctx).await)
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
pub(crate) fn dm_request_accept_inner(
    store: &DataStore,
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
    let drained = match buckets.iter().position(|b| b.npub == npub) {
        Some(i) => buckets
            .remove(i)
            .messages
            .iter()
            .map(|m| request_message_to_received(&npub, own_npub, m))
            .collect(),
        None => Vec::new(),
    };
    store.save_dm_requests(&buckets).map_err(cmd_err)?;

    let declined: Vec<(String, u64)> =
        store.load_dm_declined().map_err(cmd_err)?.into_iter().filter(|(n, _)| n != &npub).collect();
    store.save_dm_declined(&declined).map_err(cmd_err)?;

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
    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    let classified = classify_dms_inner(&client, &id_clone, &own_npub, &ctx, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;

    if !classified.requests.is_empty() {
        let existing = store.load_dm_requests().map_err(cmd_err)?;
        let merged = merge_into_requests(existing, classified.requests, now_secs());
        store.save_dm_requests(&merged).map_err(cmd_err)?;
    }

    Ok(classified.inbox)
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
    let own_npub = identity.read().await.as_ref().map(|id| id.npub()).ok_or("No identity loaded.")?;
    dm_request_accept_inner(&store, &own_npub, npub, petname)
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
// Tests — the DM seam (L1, no relay; the wire is proven by hb-it Suite DM)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dm_quarantine::DmRequestBucket;
    use hb_core::Identity;

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

    // ── Q7 — classify_dms (M13 Part B) ─────────────────────────────────────────────────────────

    /// Test-only builder for [`DmClassifyCtx`] — most tests only vary one or two of its four fields.
    fn ctx<'a>(
        contacts: &'a HashSet<String>,
        blocked: &'a HashSet<String>,
        declined: &'a HashSet<String>,
        allow_strangers: bool,
    ) -> DmClassifyCtx<'a> {
        DmClassifyCtx { contacts, blocked, declined, allow_strangers }
    }

    #[tokio::test]
    async fn blocked_sender_never_creates_a_request() {
        let me = Identity::generate();
        let blocked_id = Identity::generate();
        let wrap = build_dm(&blocked_id, &me.public_key(), "spam").await.unwrap();
        let blocked: HashSet<String> = [blocked_id.npub()].into_iter().collect();
        let classified =
            classify_dms(&me.npub(), &me, vec![wrap], &ctx(&HashSet::new(), &blocked, &HashSet::new(), true)).await;
        assert!(classified.inbox.is_empty());
        assert!(classified.requests.is_empty(), "a blocked sender must never create a request, even with allow_dms on");
    }

    #[tokio::test]
    async fn blocked_contact_is_dropped_from_inbox() {
        // Blocked supersedes contact — a blocked CONTACT's messages are dropped too.
        let me = Identity::generate();
        let contact = Identity::generate();
        let wrap = build_dm(&contact, &me.public_key(), "hi").await.unwrap();
        let contacts: HashSet<String> = [contact.npub()].into_iter().collect();
        let blocked: HashSet<String> = [contact.npub()].into_iter().collect();
        let classified =
            classify_dms(&me.npub(), &me, vec![wrap], &ctx(&contacts, &blocked, &HashSet::new(), true)).await;
        assert!(classified.inbox.is_empty(), "blocked supersedes contact");
        assert!(classified.requests.is_empty());
    }

    #[tokio::test]
    async fn declined_sender_stays_dropped_across_reclassification() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let wrap = build_dm(&stranger, &me.public_key(), "hello again").await.unwrap();
        let declined: HashSet<String> = [stranger.npub()].into_iter().collect();
        let classified =
            classify_dms(&me.npub(), &me, vec![wrap], &ctx(&HashSet::new(), &HashSet::new(), &declined, true)).await;
        assert!(classified.inbox.is_empty());
        assert!(classified.requests.is_empty(), "a declined sender is dropped even with allow_dms on");
    }

    #[tokio::test]
    async fn stranger_lands_in_requests_never_inbox_when_allow_dms_on() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let wrap = build_dm(&stranger, &me.public_key(), "first contact").await.unwrap();
        let classified = classify_dms(
            &me.npub(),
            &me,
            vec![wrap],
            &ctx(&HashSet::new(), &HashSet::new(), &HashSet::new(), true),
        )
        .await;
        assert!(classified.inbox.is_empty(), "a stranger never lands in the main inbox");
        assert_eq!(classified.requests.len(), 1);
        assert_eq!(classified.requests[0].0, stranger.npub());
        assert_eq!(classified.requests[0].1.content, "first contact");
    }

    #[tokio::test]
    async fn stranger_fully_dropped_when_allow_dms_off() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let wrap = build_dm(&stranger, &me.public_key(), "spam").await.unwrap();
        let classified = classify_dms(
            &me.npub(),
            &me,
            vec![wrap],
            &ctx(&HashSet::new(), &HashSet::new(), &HashSet::new(), false),
        )
        .await;
        assert!(classified.inbox.is_empty());
        assert!(classified.requests.is_empty(), "allow_dms off preserves the stricter drop-everything-non-contact behaviour");
    }

    #[tokio::test]
    async fn contact_and_self_land_in_inbox_never_requests() {
        let me = Identity::generate();
        let contact = Identity::generate();
        let from_contact = build_dm(&contact, &me.public_key(), "hey").await.unwrap();
        let from_self = build_dm(&me, &me.public_key(), "note to self").await.unwrap();
        let contacts: HashSet<String> = [contact.npub()].into_iter().collect();
        let classified = classify_dms(
            &me.npub(),
            &me,
            vec![from_contact, from_self],
            &ctx(&contacts, &HashSet::new(), &HashSet::new(), true),
        )
        .await;
        assert_eq!(classified.inbox.len(), 2, "both the contact and my own npub land in the inbox");
        assert!(classified.requests.is_empty());
    }

    // ── Q7 — the Request-inbox `_inner` fns (no Tauri State) ────────────────────────────────────

    #[test]
    fn request_accept_adds_manual_contact_no_browse_key_and_drains_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
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

        let drained = dm_request_accept_inner(&store, "npub1me", npub.clone(), None).unwrap();
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

        // Some(petname) sets it.
        let npub2 = "npub1stranger2".to_string();
        store
            .save_dm_requests(&[DmRequestBucket { npub: npub2.clone(), first_seen: 1, last_message_at: 1, messages: vec![] }])
            .unwrap();
        dm_request_accept_inner(&store, "npub1me", npub2.clone(), Some("Bob".into())).unwrap();
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
