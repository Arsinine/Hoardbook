//! Topics relay seam (M11; spec §11) — the publish/fetch flows over the multi-relay client for the
//! `hb-core::topic` crypto. **All event construction + crypto stays in `hb-core`**; this crate only
//! moves the events on/off relays (the contract discipline the whole `hb-net` crate keeps).
//!
//! - **Announce / discover** — public Topics are addressable (`d` = topic_id) + `t`-tagged; discovery
//!   is a relay read of announces (no registry).
//! - **Membership** — replaceable per (topic_id, member-pseudonym); join = publish, leave = NIP-09
//!   retract **signed under the derived pseudonym key** (B2), dissolution = empty roster (derived).
//! - **Channel** — regular stored posts with a NIP-40 expiration; the read side filters >24h locally.
//! - **Admission** — `member_count` is the **spoofable** tagged count (no key); `fetch_roster` /
//!   `fetch_channel` need the key (members-only); private admission rides an invite (public-join is the
//!   same seal to a name-derived keypair) or a request→approve NIP-17 DM.

use std::collections::HashMap;
use std::time::Duration;

use hb_core::topic::{
    member_sign_keys, mint_invite, open_post, parse_announce, public_join_identity, redeem_invite,
    roster, seal_membership, seal_post, NonceSet, Post, TopicKey, TopicMeta, KIND_TOPIC_ANNOUNCE,
    KIND_TOPIC_MEMBER, KIND_TOPIC_POST,
};
use hb_core::Identity;
use nostr::nips::nip09::EventDeletionRequest;
use nostr::prelude::*;
use serde::{Deserialize, Serialize};

use crate::client::RelayClient;
use crate::dm::{unwrap_dm, wrap_dm};
use crate::error::NetError;

/// Publish a batch of pre-signed topic events (announce / membership / post / invite) to **all**
/// relays (F14 — every Hoardbook event is multi-published). Errors only if an event was accepted by
/// no relay (never a silent drop).
pub async fn publish_topic(client: &RelayClient, events: &[Event]) -> Result<(), NetError> {
    for ev in events {
        client.publish(ev).await?;
    }
    Ok(())
}

/// Top-N cap for activity-ranked topic discovery (M12 W4, Decision M) — mirrors the teaser/discovery
/// cap, so a flood of junk paths can't make discovery or the client-side tree unbounded.
pub const TOPIC_DISCOVERY_CAP: usize = 100;

/// Discover public Topics by tag — a relay read of `KIND_TOPIC_ANNOUNCE` events `#t`-tagged with any
/// of `tags`, parsed + verified through `hb-core`, deduped by `topic_id` keeping the newest announce,
/// then **activity-ranked** (M12 W4, Decision M): each result is paired with its best-effort,
/// **spoofable** `member_count` and the list is sorted by it **descending**, so popular shared paths
/// surface and junk singletons sink. Returns at most [`TOPIC_DISCOVERY_CAP`] entries — a truncation is
/// **logged honestly** (M9 style), never silent. An empty `tags` is refused before any query.
pub async fn discover_public_topics(
    client: &RelayClient,
    tags: &[String],
    timeout: Duration,
) -> Result<Vec<(TopicMeta, usize)>, NetError> {
    if tags.is_empty() {
        return Err(NetError::EmptyFilter);
    }
    let filter = Filter::new().kind(Kind::from_u16(KIND_TOPIC_ANNOUNCE)).hashtags(tags.iter().cloned());
    let events = client.fetch(filter, timeout).await?;
    // Keep the newest announce per topic_id (a re-announce supersedes).
    let mut best: HashMap<String, (u64, TopicMeta)> = HashMap::new();
    for ev in events {
        if let Ok(meta) = parse_announce(&ev) {
            let ts = ev.created_at.as_u64();
            match best.get(&meta.topic_id) {
                Some((prev, _)) if *prev >= ts => {}
                _ => {
                    best.insert(meta.topic_id.clone(), (ts, meta));
                }
            }
        }
    }
    // Activity-rank: pair each topic with its (spoofable) member count, sort desc, tiebreak on id.
    let mut scored: Vec<(TopicMeta, usize)> = Vec::with_capacity(best.len());
    for (_, (_, meta)) in best {
        // A member-count fetch error scores the topic 0 (best-effort + spoofable anyway); log it so a
        // relay-side failure that buries a popular topic is debuggable, not silent (chorus round-1).
        let count = match member_count(client, &meta.topic_id, timeout).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("topic discovery: member_count failed for {}: {e}", meta.topic_id);
                0
            }
        };
        scored.push((meta, count));
    }
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.topic_id.cmp(&b.0.topic_id)));
    let total = scored.len();
    if total > TOPIC_DISCOVERY_CAP {
        tracing::info!(
            "topic discovery: showing the top {TOPIC_DISCOVERY_CAP} of {total} matches by member count (activity-ranked; junk singletons sink)"
        );
        scored.truncate(TOPIC_DISCOVERY_CAP);
    }
    Ok(scored)
}

/// The **best-effort, spoofable** pre-join member count = the number of distinct `KIND_TOPIC_MEMBER`
/// event authors (pseudonyms) tagged with `topic_id`. **No key needed**, so it is shown to non-members
/// — but anyone can publish a fake membership event tagged to the Topic, so this is an *estimate*, not
/// an authority (Decision: member-count is deliberately spoofable). The decrypted [`fetch_roster`] is
/// the sound count, and it needs the key.
pub async fn member_count(
    client: &RelayClient,
    topic_id: &str,
    timeout: Duration,
) -> Result<usize, NetError> {
    let events = fetch_membership_events(client, topic_id, timeout).await?;
    let distinct: std::collections::HashSet<PublicKey> = events.iter().map(|e| e.pubkey).collect();
    Ok(distinct.len())
}

/// Fetch the **members-only** roster — the real npubs of the current membership events, decrypted with
/// the topic key. A caller without the key cannot call this (it takes `&TopicKey`); a non-member who
/// raw-fetches the same events gets ciphertext only. Empty ⇒ dissolved.
pub async fn fetch_roster(
    client: &RelayClient,
    topic_id: &str,
    key: &TopicKey,
    timeout: Duration,
) -> Result<Vec<PublicKey>, NetError> {
    let events = fetch_membership_events(client, topic_id, timeout).await?;
    Ok(roster(key, &events))
}

/// Raw membership events for a Topic (`KIND_TOPIC_MEMBER`, `#d` = topic_id) — the ciphertext a
/// non-member sees and the input to [`member_count`] / [`fetch_roster`].
pub async fn fetch_membership_events(
    client: &RelayClient,
    topic_id: &str,
    timeout: Duration,
) -> Result<Vec<Event>, NetError> {
    let filter = Filter::new()
        .kind(Kind::from_u16(KIND_TOPIC_MEMBER))
        .identifier(topic_id.to_string());
    client.fetch(filter, timeout).await
}

/// Join a Topic: publish a membership event (signed on the wire under the derived pseudonym, carrying
/// the member's real-key proof of participation). Takes the member's own `Identity` (you only join as
/// yourself). Returns the published event so the caller can persist it and later [`leave_topic`].
pub async fn join_topic(
    client: &RelayClient,
    key: &TopicKey,
    topic_id: &str,
    member: &Identity,
    now: u64,
) -> Result<Event, NetError> {
    let ev = seal_membership(key, topic_id, member, now)?;
    client.publish(&ev).await?;
    Ok(ev)
}

/// Leave a Topic: NIP-09-retract the membership event, **signed under the same derived pseudonym key**
/// that authored it (so a compliant relay honours the deletion). Best-effort like all NIP-09 deletion
/// (N5). Dissolution is the derived state where no membership remains.
pub async fn leave_topic(
    client: &RelayClient,
    key: &TopicKey,
    member: &PublicKey,
    membership: &Event,
    _now: u64,
) -> Result<(), NetError> {
    let signer = member_sign_keys(key, member)?;
    let req = EventDeletionRequest::new().id(membership.id);
    let deletion = EventBuilder::delete(req)
        .sign_with_keys(&signer)
        .map_err(|e| NetError::Client(e.to_string()))?;
    client.publish(&deletion).await?;
    Ok(())
}

/// Post to the 24h channel: publish a sealed post (signed on the wire under the derived pseudonym,
/// carrying the author's real-key proof + a NIP-40 expiry). Takes the author's own `Identity`.
pub async fn post_to_channel(
    client: &RelayClient,
    key: &TopicKey,
    topic_id: &str,
    author: &Identity,
    body: &str,
    now: u64,
) -> Result<Event, NetError> {
    let ev = seal_post(key, topic_id, author, body, now)?;
    client.publish(&ev).await?;
    Ok(ev)
}

/// Fetch the channel — `KIND_TOPIC_POST` for `topic_id`, opened with the key and **locally filtered to
/// the last 24h** (Decision D: a non-compliant relay can't resurrect an expired post in the UI),
/// newest first.
pub async fn fetch_channel(
    client: &RelayClient,
    topic_id: &str,
    key: &TopicKey,
    now: u64,
    timeout: Duration,
) -> Result<Vec<Post>, NetError> {
    let filter = Filter::new().kind(Kind::from_u16(KIND_TOPIC_POST)).identifier(topic_id.to_string());
    let events = client.fetch(filter, timeout).await?;
    let mut posts: Vec<Post> = Vec::new();
    for ev in events {
        if let Ok(Some(p)) = open_post(key, &ev, now) {
            posts.push(p);
        }
    }
    posts.sort_by_key(|p| std::cmp::Reverse(p.ts));
    Ok(posts)
}

// ── private admission: request → approve (NIP-17 DM) ─────────────────────────────────────────────

/// The body of a join-request DM (carried inside the NIP-17 gift-wrap).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub topic_id: String,
    pub name: String,
}

/// Wrap a join-request as JSON for a NIP-17 DM.
pub fn join_request_message(topic_id: &str, name: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "hb_topic_join_request": JoinRequest { topic_id: topic_id.to_string(), name: name.to_string() }
    }))
    .expect("join request serializes")
}

/// Parse a DM body as a join request, if it is one (else `None`).
pub fn parse_join_request(content: &str) -> Option<JoinRequest> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    serde_json::from_value(v.get("hb_topic_join_request")?.clone()).ok()
}

/// Send a join request to a known member over a NIP-17 DM (private admission path 2 — request side).
pub async fn request_join(
    client: &RelayClient,
    requester: &Identity,
    member: &PublicKey,
    topic_id: &str,
    name: &str,
) -> Result<(), NetError> {
    let wrap = wrap_dm(requester, member, &join_request_message(topic_id, name)).await?;
    client.publish(&wrap).await?;
    Ok(())
}

/// Fetch + parse the join requests addressed to `me` (DMs whose body is a `JoinRequest`), returning
/// `(requester, request)` pairs. Mirrors the M10 post-decrypt allowlist shape: the outer wrap author is
/// ephemeral, so the real requester is recovered from inside the verified seal.
pub async fn fetch_join_requests(
    client: &RelayClient,
    me: &Identity,
    timeout: Duration,
) -> Result<Vec<(PublicKey, JoinRequest)>, NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    let mut out = Vec::new();
    for w in wraps {
        if let Ok(dm) = unwrap_dm(me, &w).await {
            if let Some(req) = parse_join_request(&dm.content) {
                out.push((dm.sender, req));
            }
        }
    }
    Ok(out)
}

/// How long a minted private invite stays valid (the seal is single-use regardless; this bounds the
/// window a leaked-but-un-redeemed invite is usable).
pub const INVITE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Approve a join request (private admission path 2 — approve side): mint an invite sealed to the
/// requester carrying the topic key, with a short expiry, and publish it. Any one member suffices
/// (M3 — any member may invite/admit, by design). The requester then [`fetch_invite`]s + redeems. The
/// nonce is derived per (requester, time) — its value is immaterial (replay is keyed `(topic_id,
/// invitee)`); it only keeps each minted event distinct.
pub async fn approve_join(
    client: &RelayClient,
    approver: &Identity,
    requester: &PublicKey,
    meta: &TopicMeta,
    key: &TopicKey,
    now: u64,
) -> Result<(), NetError> {
    let nonce = format!("{now}-{}", requester.to_hex());
    let invite = mint_invite(approver, requester, meta, key, &nonce, Some(now + INVITE_TTL_SECS), now)?;
    client.publish(&invite).await?;
    Ok(())
}

/// Fetch + redeem the **first valid** invite addressed to `me` (private admission path 1 — redeem
/// side). The relay filter is `{kinds:[1059], #p:[me]}`; redemption is post-decrypt. A foreign/junk/
/// expired/replayed wrap is skipped. Returns the redeemed `(meta, key)` and the invite event id (so the
/// caller can persist the seen-nonce), or `None` if no valid invite is found.
pub async fn fetch_invite(
    client: &RelayClient,
    me: &Identity,
    seen: &mut NonceSet,
    now: u64,
    timeout: Duration,
) -> Result<Option<(TopicMeta, TopicKey)>, NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    for w in wraps {
        // `redeem_invite` atomically records a single-use invite's seen-nonce into `seen` on success
        // (the public-join credential is exempt); the caller persists `seen` after this returns.
        if let Ok((meta, key)) = redeem_invite(me, &w, seen, now) {
            return Ok(Some((meta, key)));
        }
    }
    Ok(None)
}

/// Join a **public** Topic by name: derive the public-join keypair, fetch the public-join credential
/// (a gift-wrap `#p`-tagged to the name-derived pubkey), and redeem it → the topic key. This is the
/// participation bar (Decision A) — any joiner who knows the name can do it.
pub async fn join_public(
    client: &RelayClient,
    name: &str,
    seen: &mut NonceSet,
    now: u64,
    timeout: Duration,
) -> Result<Option<(TopicMeta, TopicKey)>, NetError> {
    let pj = public_join_identity(name)?;
    fetch_invite(client, &pj, seen, now, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_request_round_trips_through_the_dm_body() {
        let msg = join_request_message("abc123", "80s-anime");
        let req = parse_join_request(&msg).unwrap();
        assert_eq!(req.topic_id, "abc123");
        assert_eq!(req.name, "80s-anime");
    }

    #[test]
    fn a_plain_dm_is_not_a_join_request() {
        assert!(parse_join_request("hey, want to trade?").is_none());
        assert!(parse_join_request(r#"{"something_else":1}"#).is_none());
    }
}
