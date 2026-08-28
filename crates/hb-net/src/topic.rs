//! Topics relay seam (M11; spec §11) — the publish/fetch flows over the multi-relay client for the
//! `hb-core::topic` crypto. **All event construction + crypto stays in `hb-core`**; this crate only
//! moves the events on/off relays (the contract discipline the whole `hb-net` crate keeps).
//!
//! - **Announce / discover** — public Topics are addressable (`d` = topic_id) + `t`-tagged; discovery
//!   is a relay read of announces (no registry).
//! - **Membership** — replaceable per (topic_id, member-pseudonym); join = publish, leave = NIP-09
//!   retract **signed under the derived pseudonym key** (B2), dissolution = empty roster (derived).
//! - **Channel** — regular stored posts (+ M13 Part A member broadcasts, same kind, same 24h shape)
//!   with a NIP-40 expiration; the read side filters >24h locally.
//! - **Admission** — `member_count` is the **spoofable** tagged count (no key); `fetch_roster` /
//!   `fetch_channel` need the key (members-only); private admission rides an invite (public-join is the
//!   same seal to a name-derived keypair) or a request→approve NIP-17 DM.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use hb_core::topic::{
    member_sign_keys, mint_invite, open_channel_item, open_post, parse_announce,
    public_join_identity, redeem_invite, roster, seal_announce, seal_membership, seal_post,
    Announcement, ChannelItem, NonceSet, Post, TopicKey, TopicMeta, KIND_TOPIC_ANNOUNCE,
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

/// Relay-fetch headroom over the ranked [`TOPIC_DISCOVERY_CAP`] — the topic-discovery twin of
/// [`crate::client::TEASER_SEARCH_FETCH_LIMIT`] (the hardened sibling; CLAUDE.md §9 — one call site
/// got the fix, its twin didn't). Same lesson, same shape: the relay `#t` filter is an OR over the
/// tag set, and without an explicit `.limit()` the relay's own internal cap (strfry
/// `maxFilterLimit` defaults to 500) silently decided which announces came back. A Topic whose
/// announce was older than 500 loose `#t`-matches was **evicted before the activity-rank ever saw
/// it** — it reads as "not found" forever (H3, QURATOR-80). Sized at 10× the ranked cap so the
/// member-count rank sees comfortably more than the visible window; a relay that clamps below this
/// simply returns fewer.
pub const TOPIC_DISCOVERY_FETCH_LIMIT: usize = 1000;

/// Build the topic-discovery relay filter — pure (no I/O), so the fetch-budget `.limit()` is
/// unit-testable without a relay (the same testability split as [`teaser_search_filter`]). Refused
/// before any query when `tags` is empty.
pub fn topic_discover_filter(tags: &[String]) -> Result<Filter, NetError> {
    if tags.is_empty() {
        return Err(NetError::EmptyFilter);
    }
    Ok(Filter::new()
        .kind(Kind::from_u16(KIND_TOPIC_ANNOUNCE))
        .hashtags(tags.iter().cloned())
        .limit(TOPIC_DISCOVERY_FETCH_LIMIT))
}

/// Concurrency bound for activity-ranked topic discovery (QURATOR-82). Each `member_count` is its
/// own relay round-trip at the caller's full `timeout`; serialising them made discovery take up to
/// [`TOPIC_DISCOVERY_CAP`] sequential round-trips — one slow relay stalled the whole list (an `hb-it`
/// L2 run against the SG relay blew a 180s timeout on exactly this path).
///
/// The fix is bounded concurrency, and the bound is a **relay-citizenship** decision (the M16
/// standing ruling: discovery must NOT turn into a burst of 100 simultaneous queries against a
/// public relay). 8 keeps the worst-case per-relay simultaneous-query count an order of magnitude
/// below the `TOPIC_DISCOVERY_CAP = 100` ceiling that a `join_all` would fire in one shot, while
/// still turning a 100-sequential-round-trip stall into ~13 waves. A hostile/flooded relay can still
/// see the query fan-out, but it is the same shape as 8 users discovering at once, not a scanner.
pub const TOPIC_DISCOVERY_CONCURRENCY: usize = 8;

/// Pair each topic with a best-effort score by running `score` concurrently, then sort **descending**
/// by score with an **ascending `topic_id` tiebreak** — the activity-rank contract. Pure (no relay
/// I/O): the caller supplies the scoring future, so this is directly unit-testable against an in-memory
/// `score` closure that injects completion order (QURATOR-82 regression coverage — proves the output
/// order is independent of completion order).
///
/// Concurrency is bounded at [`TOPIC_DISCOVERY_CONCURRENCY`] so the score phase can't become a burst
/// of `len` simultaneous relay queries (the relay-citizenship ruling above). The `collect` then `sort`
/// shape is load-bearing: a sort-free "append as each future resolves" would leak completion order
/// into the result, breaking the deterministic tiebreak.
///
/// `score` takes the `topic_id` by owned `String` (not `&str`): the future it returns has to outlive
/// the per-iteration borrow inside `buffer_unordered`, and an HRTB `for<'a> Fn(&'a str) -> impl
/// Future + 'a` is significantly harder to express than a 24-byte clone of a topic id.
async fn score_topics_with<F, Fut>(
    topics: impl IntoIterator<Item = TopicMeta>,
    score: F,
) -> Vec<(TopicMeta, usize)>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = usize>,
{
    let topics: Vec<TopicMeta> = topics.into_iter().collect();
    // Move each `TopicMeta` through the stream payload so the final pairs keep the full metadata.
    // Completion order is unrelated to input order under bounded concurrency; only the explicit
    // `sort_by` below determines the output.
    let scored = stream::iter(topics)
        .map(|meta| async {
            let count = score(meta.topic_id.clone()).await;
            (meta, count)
        })
        .buffer_unordered(TOPIC_DISCOVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    let mut scored = scored;
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.topic_id.cmp(&b.0.topic_id)));
    scored
}

/// The cheap, relay-cheerful half of discovery (QURATOR-143 W1) — what ONE announce fetch paints:
/// every discovered [`TopicMeta`] (newest-announce-per-`topic_id`) plus the per-root accounting the
/// starved-root escalation and the round-robin ranker need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicDiscoveries {
    /// The deduped topics, **unordered** — ranking (member counts) is the lazy second phase.
    pub topics: Vec<TopicMeta>,
    /// How many parsed announce events each root tag accounted for — the per-root window the
    /// shared-`limit` response was allocated into. Roots that matched nothing are absent (0 == absent:
    /// the escalation only cares about "did this root get ANY slot").
    pub root_event_counts: BTreeMap<String, usize>,
    /// True iff the primary response came back with exactly [`TOPIC_DISCOVERY_FETCH_LIMIT`] events —
    /// the honest "the response hit its limit" signal the escalation is gated on. A relay that clamps
    /// below the limit reports a smaller page and `hit_limit` is false (nothing was evicted *by us*).
    pub hit_limit: bool,
}

/// The paint path (QURATOR-143 W1): discover public Topics by tag with **one relay read** — all roots
/// ride the single `#t` OR-filter [`topic_discover_filter`] builds — parse + verify + dedupe with
/// **zero `member_count` round trips**. Ranking is [`rank_discovered_topics`], the lazy second phase.
///
/// **Per-root budget on the announce fetch** (the M20 W3 eviction shape, at the directory layer): the
/// one query carries one shared `limit(TOPIC_DISCOVERY_FETCH_LIMIT)` and relays serve newest-first,
/// so a junk-announce flood under ONE root can evict every other root from the response. Client-side
/// the response is allocated into per-root windows by counting each parsed announce toward its own
/// tag; a root that came back EMPTY while the response HIT its limit is escalated with its own
/// follow-up query (one extra read per starved root, only ever paid under a flood). The normal case
/// stays exactly one read.
pub async fn discover_public_topics_paint(
    client: &RelayClient,
    tags: &[String],
    timeout: Duration,
) -> Result<TopicDiscoveries, NetError> {
    let filter = topic_discover_filter(tags)?;
    let mut events = client.fetch(filter, timeout).await?;
    let mut out = dedupe_announces(&mut events, tags);
    out.hit_limit = events.len() >= TOPIC_DISCOVERY_FETCH_LIMIT;
    // Starved-root escalation: fire ONLY when the shared response hit its limit — below the limit
    // nothing was evicted by the budget, so an empty root is a GENUINE empty and must stay one read.
    // Escalations are per-root queries (each its own full-limit fetch under that root alone, where a
    // flood under another root cannot evict it) and run bounded, never as a fan-out.
    if out.hit_limit {
        for root in starved_roots(tags, &out.root_event_counts) {
            tracing::debug!("topic discovery: root {root} starved by the shared fetch limit; escalating");
            let solo = client.fetch(topic_discover_filter(std::slice::from_ref(root))?, timeout).await?;
            let solo_hit = dedupe_announces(&mut { solo }, std::slice::from_ref(root));
            out.merge(solo_hit);
        }
    }
    Ok(out)
}

/// The roots worth escalating after a limit-hit shared response: exactly those the page left EMPTY
/// (absent from `root_event_counts` — a root that matched nothing consumed no slot). A root that got
/// at least one slot is NOT starved and must not escalate, even when another root flooded the page.
/// Shared by the paint loop and its test so the rule cannot fork (the P-6 remedy: a guard that
/// re-derives its own copy of the rule is decorative).
fn starved_roots<'a>(tags: &'a [String], root_event_counts: &BTreeMap<String, usize>) -> Vec<&'a String> {
    tags.iter().filter(|t| !root_event_counts.contains_key(*t)).collect()
}

/// The pure dedupe half of [`discover_public_topics_paint`]: parse + authorship-check each event,
/// keep the newest announce per `topic_id`, and count each parsed announce toward every root tag it
/// carries (an announce tagged with several roots occupies a slot in each root's window — that IS the
/// eviction shape being measured). `hit_limit` is taken from the caller, which knows the page size.
fn dedupe_announces(events: &mut [Event], tags: &[String]) -> TopicDiscoveries {
    // Keep the newest announce per topic_id (a re-announce supersedes).
    let mut best: HashMap<String, (u64, TopicMeta)> = HashMap::new();
    let mut root_event_counts: BTreeMap<String, usize> = BTreeMap::new();
    for ev in events.iter() {
        if let Ok(meta) = parse_announce(ev) {
            // A validly-signed announce is only a valid CANDIDATE for the topic its own `d` tag
            // (identifier) names — a signature proves who signed it, not that the signer's claimed
            // `meta.topic_id` matches the identifier they published it under. Without this check, an
            // announce whose `d` tag names one topic but whose payload carries a different
            // `meta.topic_id` could poison discovery under a d-tag it never legitimately owns.
            if event_identifier(ev) != Some(meta.topic_id.as_str()) {
                continue;
            }
            // Per-root window accounting: this event occupies one slot in EVERY queried root it is
            // tagged with (the `#t` filter is an OR, so the relay matched it at least once; counting
            // per-carried-tag measures exactly the budget each root consumed).
            let carried: Vec<&str> = tags
                .iter()
                .map(|t| t.as_str())
                .filter(|t| ev.tags.hashtags().any(|h| h == *t))
                .collect();
            for c in carried {
                *root_event_counts.entry(c.to_string()).or_insert(0) += 1;
            }
            let ts = ev.created_at.as_u64();
            match best.get(&meta.topic_id) {
                Some((prev, _)) if *prev >= ts => {}
                _ => {
                    best.insert(meta.topic_id.clone(), (ts, meta));
                }
            }
        }
    }
    TopicDiscoveries {
        topics: best.into_values().map(|(_, meta)| meta).collect(),
        root_event_counts,
        hit_limit: false,
    }
}

impl TopicDiscoveries {
    /// Fold an escalation result in and OR the limit flags — if EITHER page hit its limit the caller
    /// is still in flood territory and the accounting stays honest.
    ///
    /// Newest-wins across the two sides: `TopicMeta` carries no `created_at` (it is the payload, not
    /// the envelope), so the merge cannot re-derive newest-wins from the metas — and it does not need
    /// to. An escalation only ever runs for a root the shared response left EMPTY, so the only ids
    /// the two sides can share are ids the shared response found under a DIFFERENT root's tag; the
    /// first-seen entry is that existing one, and keeping it preserves the dedupe the paint already
    /// settled. The invariant that matters — one entry per `topic_id`, across roots — is what `seen`
    /// enforces.
    fn merge(&mut self, other: TopicDiscoveries) {
        let mut seen: HashSet<String> = self.topics.iter().map(|m| m.topic_id.clone()).collect();
        for m in other.topics {
            if seen.insert(m.topic_id.clone()) {
                self.topics.push(m);
            }
        }
        for (root, n) in other.root_event_counts {
            *self.root_event_counts.entry(root).or_insert(0) += n;
        }
        self.hit_limit |= other.hit_limit;
    }
}

/// The lazy second phase (QURATOR-143 W1): activity-rank an already-painted
/// [`TopicDiscoveries`] — pair each topic with its best-effort, **spoofable** `member_count`
/// (bounded by [`TOPIC_DISCOVERY_CONCURRENCY`] inside [`score_topics_with`]) and sort by it
/// **descending**. This is the SAME seam `discover_public_topics` always scored at; splitting it out
/// lets the caller paint first and rank behind the paint.
pub async fn rank_discovered_topics(
    client: &RelayClient,
    found: TopicDiscoveries,
    timeout: Duration,
) -> Result<Vec<(TopicMeta, usize)>, NetError> {
    // Activity-rank: pair each topic with its (spoofable) member count, sort desc, tiebreak on id.
    // Each `member_count` is its own relay round-trip at the full `timeout`; serialising them inside
    // the loop made discovery take up to `TOPIC_DISCOVERY_CAP` sequential round-trips before the user
    // saw anything — one slow relay stalled the whole list (QURATOR-82: an `hb-it` L2 run against the
    // SG relay blew a 180s timeout on exactly this path). Bound the concurrency instead.
    let mut scored = score_topics_with(found.topics, |topic_id| async move {
        match member_count(client, &topic_id, timeout).await {
            Ok(c) => c,
            Err(e) => {
                // A member-count fetch error scores the topic 0 (best-effort + spoofable anyway); log
                // it so a relay-side failure that buries a popular topic is debuggable, not silent
                // (chorus round-1).
                tracing::debug!("topic discovery: member_count failed for {}: {e}", topic_id);
                0
            }
        }
    })
    .await;
    let total = scored.len();
    if total > TOPIC_DISCOVERY_CAP {
        tracing::info!(
            "topic discovery: showing the top {TOPIC_DISCOVERY_CAP} of {total} matches by member count (activity-ranked; junk singletons sink)"
        );
        scored.truncate(TOPIC_DISCOVERY_CAP);
    }
    Ok(scored)
}

/// Discover public Topics by tag — a relay read of `KIND_TOPIC_ANNOUNCE` events `#t`-tagged with any
/// of `tags`, parsed + verified through `hb-core`, deduped by `topic_id` keeping the newest announce,
/// then **activity-ranked** (M12 W4, Decision M): each result is paired with its best-effort,
/// **spoofable** `member_count` and the list is sorted by it **descending**, so popular shared paths
/// surface and junk singletons sink. Returns at most [`TOPIC_DISCOVERY_CAP`] entries — a truncation is
/// **logged honestly** (M9 style), never silent. An empty `tags` is refused before any query.
///
/// The one-shot composition of the two QURATOR-143 W1 phases ([`discover_public_topics_paint`] then
/// [`rank_discovered_topics`]) — kept for the callers that still want rank-and-return in one call.
pub async fn discover_public_topics(
    client: &RelayClient,
    tags: &[String],
    timeout: Duration,
) -> Result<Vec<(TopicMeta, usize)>, NetError> {
    let found = discover_public_topics_paint(client, tags, timeout).await?;
    rank_discovered_topics(client, found, timeout).await
}

/// An event's own signed `d` (identifier) tag — the tag-level claim of which topic it belongs to.
fn event_identifier(ev: &Event) -> Option<&str> {
    ev.tags.identifier()
}

/// Pick the newest, verifiably-parsed announce **for `expected_topic_id`** out of a raw event batch —
/// the pure half of [`fetch_announce`], directly unit-testable against hand-built events with no
/// relay. Mirrors the newest-by-`created_at` dedup [`discover_public_topics`] does across many topics,
/// restricted here to (at most) one result for one topic. An event is dropped, never just deprioritized,
/// if it fails to parse (foreign junk, wrong kind, bad signature — the same "ignore what doesn't
/// verify" posture as the rest of this module), **or** if its parsed `meta.topic_id` does not equal
/// `expected_topic_id`: a validly-signed announce whose payload names a different topic than the one
/// being looked up must never satisfy the lookup, however recent it is — otherwise a relay (malicious,
/// buggy, or merely returning a broader result than the filter asked for) could make an unrelated,
/// attacker-controlled announce shadow the real one for a queried `topic_id`.
fn newest_announce(events: Vec<Event>, expected_topic_id: &str) -> Option<TopicMeta> {
    let mut best: Option<(u64, TopicMeta)> = None;
    for ev in events {
        if let Ok(meta) = parse_announce(&ev) {
            if meta.topic_id != expected_topic_id {
                continue;
            }
            let ts = ev.created_at.as_u64();
            match &best {
                Some((prev, _)) if *prev >= ts => {}
                _ => best = Some((ts, meta)),
            }
        }
    }
    best.map(|(_, meta)| meta)
}

/// The join-first lookup (devtest #11): fetch the announce for a specific `topic_id`, if one exists,
/// so a caller can check "does this public name already have a room?" **before** minting a new Topic
/// — otherwise two people who independently pick the same name fork into two cryptographically
/// distinct rooms (same `topic_id`, different `topic_key`, Decision C). `None` means no announce was
/// found (the name is free), never an error.
pub async fn fetch_announce(
    client: &RelayClient,
    topic_id: &str,
    timeout: Duration,
) -> Result<Option<TopicMeta>, NetError> {
    let filter = Filter::new().kind(Kind::from_u16(KIND_TOPIC_ANNOUNCE)).identifier(topic_id.to_string());
    let events = client.fetch(filter, timeout).await?;
    Ok(newest_announce(events, topic_id))
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

/// Broadcast an announce to the channel (M13 Part A): publish a sealed announce (signed on the wire
/// under the derived pseudonym, carrying the author's real-key proof + a NIP-40 expiry) — the SAME
/// kind + relay path as [`post_to_channel`], distinguished only by the ciphertext domain byte. The
/// cooldown between two announces from the same member is pure arithmetic in `hb_core`
/// (`announce_cooldown_remaining`); this crate does not enforce it — the timer + persisted
/// `last_announce_at` live in hb-app (the crypto/relay seam stays a dumb pipe).
pub async fn announce_to_topic(
    client: &RelayClient,
    key: &TopicKey,
    topic_id: &str,
    author: &Identity,
    body: &str,
    now: u64,
) -> Result<Event, NetError> {
    let ev = seal_announce(key, topic_id, author, body, now)?;
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

/// The channel read, split by item kind — both lists **newest-first**. Pure (no relay I/O), so it is
/// directly L1-testable against a hand-built event list; [`fetch_channel_full`] is the relay-fetching
/// wrapper around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRead {
    pub posts: Vec<Post>,
    pub announcements: Vec<Announcement>,
}

/// Partition a raw kind-1117 fetch into posts + announcements — one [`hb_core::open_channel_item`]
/// per event (a single decrypt each), sorted newest-first. An event that doesn't open (wrong key,
/// foreign junk, an unrecognised domain byte, or a >24h/future-skewed item) is silently excluded —
/// the same "a reader ignores what it can't verify" posture as [`fetch_channel`], **not** an error (a
/// channel is a best-effort stream, not a strict-parse feed).
pub fn partition_channel_events(key: &TopicKey, events: &[Event], now: u64) -> ChannelRead {
    let mut posts: Vec<Post> = Vec::new();
    let mut announcements: Vec<Announcement> = Vec::new();
    for ev in events {
        if let Ok(Some(item)) = open_channel_item(key, ev, now) {
            match item {
                ChannelItem::Post(p) => posts.push(p),
                ChannelItem::Announce(a) => announcements.push(a),
            }
        }
    }
    posts.sort_by_key(|p| std::cmp::Reverse(p.ts));
    announcements.sort_by_key(|a| std::cmp::Reverse(a.ts));
    ChannelRead { posts, announcements }
}

/// Fetch the full channel — one kind-1117 relay read, partitioned into posts + announcements via
/// [`partition_channel_events`] (both **newest-first**, each locally filtered to the last 24h, same as
/// [`fetch_channel`]). [`fetch_channel`] is kept exactly as-is for its existing callers: it silently
/// skips an announce event (an announce ciphertext fails `open_post` on the domain byte), so an old
/// reader stays blind to broadcasts rather than mis-rendering one as a post — that back-compat property
/// still holds (M13 Part A introduces no new fetch path for `fetch_channel`'s existing callers).
pub async fn fetch_channel_full(
    client: &RelayClient,
    topic_id: &str,
    key: &TopicKey,
    now: u64,
    timeout: Duration,
) -> Result<ChannelRead, NetError> {
    let filter = Filter::new().kind(Kind::from_u16(KIND_TOPIC_POST)).identifier(topic_id.to_string());
    let events = client.fetch(filter, timeout).await?;
    Ok(partition_channel_events(key, &events, now))
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
/// expired/replayed wrap is skipped. Returns the redeemed `(meta, key, issuer)` and the invite event id
/// (so the caller can persist the seen-nonce), or `None` if no valid invite is found. When
/// `expected_topic_id` is `Some`, an invite whose payload names a different topic is skipped (W4 — see
/// [`redeem_invite`]'s topic_id binding).
pub async fn fetch_invite(
    client: &RelayClient,
    me: &Identity,
    seen: &mut NonceSet,
    now: u64,
    timeout: Duration,
    expected_topic_id: Option<&str>,
) -> Result<Option<(TopicMeta, TopicKey, PublicKey)>, NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    for w in wraps {
        // `redeem_invite` atomically records a single-use invite's seen-nonce into `seen` on success
        // (the public-join credential is exempt); the caller persists `seen` after this returns.
        if let Ok((meta, key, issuer)) = redeem_invite(me, &w, seen, now, expected_topic_id) {
            return Ok(Some((meta, key, issuer)));
        }
    }
    Ok(None)
}

/// Join a **public** Topic by name: derive the public-join keypair, fetch the public-join credential
/// (a gift-wrap `#p`-tagged to the name-derived pubkey), and redeem it → the topic key. This is the
/// participation bar (Decision A) — any joiner who knows the name can do it. The expected `topic_id`
/// is derived from the name the SAME way the topic was created, and bound into the redeem (W4: a
/// forged public-join invite naming a different topic can no longer redirect the joiner).
pub async fn join_public(
    client: &RelayClient,
    name: &str,
    seen: &mut NonceSet,
    now: u64,
    timeout: Duration,
) -> Result<Option<(TopicMeta, TopicKey, PublicKey)>, NetError> {
    let pj = public_join_identity(name)?;
    let expected = hb_core::topic::topic_id_for_name(&hb_core::topic::normalized_public_name(name)?);
    fetch_invite(client, &pj, seen, now, timeout, Some(&expected)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::new_topic;
    use std::time::Instant;

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

    // ─────────────────── H3 (QURATOR-80): topic-discovery filter fetch budget ────────────────────
    //
    // The hardened-path / unhardened-sibling drift pair (CLAUDE.md §9): `teaser_search_filter`
    // (client.rs) declares an explicit `.limit()` so the relay's own `maxFilterLimit` (strfry default
    // 500) cannot silently evict a strict-AND match older than 500 loose `#t` hits. The topic path
    // had the identical shape and none of the fix — a Topic announce older than 500 loose matches was
    // dropped before `discover_public_topics` ever saw it, reading as "not found" forever.

    #[test]
    fn topic_discover_filter_declares_an_explicit_limit_h3() {
        // Without `.limit()` the relay's internal response cap decided which announces came back, so
        // the activity-rank never saw the full set. The budget must be ours, not the relay's default.
        let f = topic_discover_filter(&["video".into()]).unwrap();
        assert_eq!(f.limit, Some(TOPIC_DISCOVERY_FETCH_LIMIT), "fetch budget is declared explicitly");
    }

    #[test]
    fn topic_discover_filter_targets_the_topic_announce_kind_and_tags() {
        let f = topic_discover_filter(&["video".into(), "anime".into()]).unwrap();
        // The kind set contains exactly the topic-announce kind.
        let expected = Kind::from_u16(KIND_TOPIC_ANNOUNCE);
        assert!(f.kinds.as_ref().is_some_and(|k| k.contains(&expected) && k.len() == 1));
        // The tags ride on `#t` (hashtags) — assert against the SERIALIZED filter, i.e. the bytes the
        // relay actually evaluates. An `is_empty()` check here would be vacuous (true of any filter
        // carrying a kind), which is the "green test claiming coverage it never had" shape CLAUDE.md
        // §9 catalogs; the OR-union membership has to be asserted directly or not claimed at all.
        let wire = serde_json::to_value(&f).expect("filter serializes");
        let t = wire.get("#t").and_then(|v| v.as_array()).expect("the filter carries a #t tag set");
        let got: Vec<&str> = t.iter().filter_map(|v| v.as_str()).collect();
        assert!(got.contains(&"video") && got.contains(&"anime"), "both tags ride #t, got {got:?}");
    }

    #[test]
    fn topic_discover_filter_refuses_empty_tags_before_any_query() {
        assert!(topic_discover_filter(&[]).is_err(), "an empty tag set must not be shipped to a relay");
    }

    // ───────────────────────── M13 Part A: channel partition (pure, no relay) ─────────────────────

    #[test]
    fn fetch_channel_full_partitions_are_disjoint() {
        let (meta, key) = new_topic("video/hbnet-announce-test", "", vec![], false).unwrap();
        let author = Identity::generate();
        let now = 1_700_000_000u64;
        let post = seal_post(&key, &meta.topic_id, &author, "a post", now).unwrap();
        let announce = seal_announce(&key, &meta.topic_id, &author, "an announce", now).unwrap();
        let read = partition_channel_events(&key, &[post, announce], now);
        assert_eq!(read.posts.len(), 1, "exactly one post");
        assert_eq!(read.announcements.len(), 1, "exactly one announcement");
        assert_eq!(read.posts[0].body, "a post");
        assert_eq!(read.announcements[0].body, "an announce");
    }

    #[test]
    fn channel_read_lists_sorted_newest_first() {
        let (meta, key) = new_topic("video/hbnet-announce-sort", "", vec![], false).unwrap();
        let author = Identity::generate();
        let now = 1_700_000_000u64;
        let p1 = seal_post(&key, &meta.topic_id, &author, "older post", now).unwrap();
        let p2 = seal_post(&key, &meta.topic_id, &author, "newer post", now + 10).unwrap();
        let a1 = seal_announce(&key, &meta.topic_id, &author, "older announce", now).unwrap();
        let a2 = seal_announce(&key, &meta.topic_id, &author, "newer announce", now + 10).unwrap();
        let read = partition_channel_events(&key, &[p1, a1, p2, a2], now + 10);
        assert_eq!(
            read.posts.iter().map(|p| p.body.as_str()).collect::<Vec<_>>(),
            vec!["newer post", "older post"],
            "posts are newest-first"
        );
        assert_eq!(
            read.announcements.iter().map(|a| a.body.as_str()).collect::<Vec<_>>(),
            vec!["newer announce", "older announce"],
            "announcements are newest-first"
        );
    }

    // ───────────────────────── devtest #11: join-first lookup (pure `newest_announce`) ────────────

    #[test]
    fn newest_announce_is_none_for_an_empty_batch() {
        assert_eq!(newest_announce(vec![], "some-topic-id"), None);
    }

    #[test]
    fn newest_announce_picks_the_latest_by_created_at() {
        use hb_core::topic::build_announce;
        let (meta, _key) = new_topic("video/hbnet-lookup-test", "old desc", vec![], false).unwrap();
        let author = Identity::generate();
        let old = build_announce(&author, &meta, 1_700_000_000).unwrap();
        let mut newer_meta = meta.clone();
        newer_meta.description = "new desc".into();
        let new = build_announce(&author, &newer_meta, 1_700_000_100).unwrap();
        let picked = newest_announce(vec![old, new], &meta.topic_id).unwrap();
        assert_eq!(picked.description, "new desc", "the newest (by created_at) announce wins");
        assert_eq!(picked.topic_id, meta.topic_id);
    }

    #[test]
    fn newest_announce_skips_unparseable_junk() {
        use hb_core::topic::build_announce;
        let (meta, _key) = new_topic("video/hbnet-lookup-junk", "", vec![], false).unwrap();
        let author = Identity::generate();
        let good = build_announce(&author, &meta, 1_700_000_000).unwrap();
        // A foreign teaser event (wrong kind) mixed into the batch must be silently skipped, not error.
        let junk = hb_core::event::build_teaser(
            &author,
            &hb_core::event::Teaser {
                display_name: "junk".into(),
                bio: String::new(),
                tags: vec![],
                content_types: vec![],
                picture: None,
            },
            true,
        )
        .unwrap();
        let picked = newest_announce(vec![junk, good], &meta.topic_id).unwrap();
        assert_eq!(picked.topic_id, meta.topic_id);
    }

    /// A validly-signed announce whose `d` tag names the topic being looked up, but whose plaintext
    /// payload claims a different `meta.topic_id`, must never satisfy the lookup: hand-built directly
    /// (bypassing `build_announce`'s automatic d-tag/payload consistency) to reproduce exactly the
    /// vulnerable shape a malicious signer (any pubkey — announces are unpermissioned) could publish.
    fn build_mismatched_announce(
        author: &Identity,
        d_tag_topic_id: &str,
        embedded_meta: &TopicMeta,
        now: u64,
    ) -> Event {
        let payload = serde_json::json!({
            "v": hb_core::SCHEMA_V,
            "topic_id": embedded_meta.topic_id,
            "name": embedded_meta.name,
            "description": embedded_meta.description,
            "tags": embedded_meta.tags,
            "private": embedded_meta.private,
        })
        .to_string();
        let tags = vec![
            Tag::identifier(d_tag_topic_id.to_string()),
            Tag::custom(TagKind::custom("hb-v"), [hb_core::SCHEMA_V.to_string()]),
        ];
        author
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_TOPIC_ANNOUNCE), payload)
                    .tags(tags)
                    .custom_created_at(Timestamp::from(now)),
            )
            .unwrap()
    }

    #[test]
    fn newest_announce_rejects_a_d_tag_payload_topic_id_mismatch() {
        let (victim_meta, _key) = new_topic("video/hbnet-mismatch-victim", "victim", vec![], false).unwrap();
        let (attacker_meta, _key2) =
            new_topic("video/hbnet-mismatch-attacker", "unrelated metadata", vec![], false).unwrap();
        let author = Identity::generate();
        // `d` = the victim's topic_id, but the signed payload names the attacker's own, unrelated topic.
        let mismatched =
            build_mismatched_announce(&author, &victim_meta.topic_id, &attacker_meta, 1_700_000_000);
        let picked = newest_announce(vec![mismatched], &victim_meta.topic_id);
        assert_eq!(picked, None, "a d-tag/payload topic_id mismatch must never satisfy the lookup");
    }

    #[test]
    fn newest_announce_returns_the_matching_candidate_when_a_mismatch_is_also_present() {
        use hb_core::topic::build_announce;
        let (victim_meta, _key) = new_topic("video/hbnet-mismatch-both", "real", vec![], false).unwrap();
        let (attacker_meta, _key2) =
            new_topic("video/hbnet-mismatch-both-attacker", "unrelated", vec![], false).unwrap();
        let author = Identity::generate();
        let genuine = build_announce(&author, &victim_meta, 1_700_000_000).unwrap();
        // Newer (by created_at) than the genuine one, but a mismatch — must still lose.
        let mismatched =
            build_mismatched_announce(&author, &victim_meta.topic_id, &attacker_meta, 1_700_000_999);
        let picked = newest_announce(vec![mismatched, genuine], &victim_meta.topic_id).unwrap();
        assert_eq!(picked.topic_id, victim_meta.topic_id);
        assert_eq!(picked.description, "real", "only the matching candidate wins, regardless of recency");
    }

    // ───────────────────────── QURATOR-82: bounded-concurrency scoring (pure helper) ───────────────
    //
    // The defect: `discover_public_topics` awaited `member_count` once per topic sequentially inside
    // the loop, each at the full RELAY_TIMEOUT, after the announce fetch had already completed. With
    // TOPIC_DISCOVERY_CAP = 100 that was up to 100 sequential round-trips before the user saw
    // anything — a hang that reads as "Discover Topics finds nothing". The fix is bounded concurrency
    // via `score_topics_with`; these tests pin the contracts that the refactor must NOT regress:
    //
    //   - per-topic error tolerance (a score of 0, i.e. a failed `member_count`, never fails the call
    //     and never silently drops the topic; the topic scores 0);
    //   - deterministic output order regardless of completion order — count desc, tiebreak topic_id
    //     asc. Concurrency must not leak completion order into the result.
    //
    // A concurrency change's speedup is invisible to a unit test — jsdom-equivalent point: a green
    // suite here says nothing about the stall being gone. The speedup is measured by the integration
    // suite (`hb-it` L2 against the slower SG relay), not by these tests. These tests prove the
    // refactor preserved the *contracts*, not that it went faster.

    /// Helper: build a `TopicMeta` with a test-controlled `topic_id` (NOT derived from a name, so
    /// the tiebreak test can assert a specific ascending order on the ids). The other fields are
    /// trivial — only `topic_id` is load-bearing for `score_topics_with`.
    fn scored_topic_meta(id: &str) -> TopicMeta {
        TopicMeta {
            topic_id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            tags: vec![],
            private: false,
        }
    }

    #[tokio::test]
    async fn score_topics_with_tolerates_a_failed_score_by_keeping_the_topic_at_zero() {
        // The per-topic error tolerance: a `member_count` failure scores the topic 0 and is never
        // fatal, never silent (the production path wraps the fetch in a `match` that logs + returns
        // 0; the helper itself sees only the `usize` so we model the failure as `0`).
        let topics = vec![
            scored_topic_meta("video/ok-topic"),
            scored_topic_meta("video/failed-topic"),
        ];
        let scored = score_topics_with(topics, |tid| async move {
            if tid.contains("failed") { 0 } else { 5 }
        })
        .await;
        // Both topics are still present — a failed score does not drop the topic from discovery.
        assert_eq!(scored.len(), 2, "a failed score keeps the topic in the list at rank 0");
        // The failed topic is at the tail (rank 0 sorts last under count-desc).
        assert_eq!(scored[1].1, 0, "the failed topic scored 0, not dropped");
        assert_eq!(scored[1].0.topic_id, "video/failed-topic");
    }

    #[tokio::test]
    async fn score_topics_with_output_order_is_count_desc_regardless_of_completion_order() {
        // The deterministic-output contract: under bounded concurrency the futures resolve in
        // arbitrary order, but the output must always be sorted count-desc with topic_id-asc as the
        // tiebreaker. This test injects a deliberately adversarial completion order (high-count
        // topics resolve LAST) by making the closure await longer for higher counts — if the helper
        // ever let completion order leak through, this test would red.
        let topics = vec![
            scored_topic_meta("video/alpha"),   // will score 1
            scored_topic_meta("video/beta"),    // will score 3 (highest)
            scored_topic_meta("video/gamma"),   // will score 3 (tie, id after beta)
            scored_topic_meta("video/delta"),   // will score 2
        ];
        let scored = score_topics_with(topics, |tid| async move {
            // Lower count ⇒ shorter await: the LOW-count topics resolve FIRST. The required output
            // order is count-DESC, so a completion-order-leaking implementation (no final sort) would
            // emit alpha(1) → delta(2) → beta(3) → gamma(3) — the exact inverse of the contract.
            let count = match tid.as_str() {
                "video/alpha" => 1,
                "video/beta" => 3,
                "video/gamma" => 3,
                "video/delta" => 2,
                _ => 0,
            };
            // Sleep PROPORTIONAL to count so high-count topics resolve LAST under concurrency.
            tokio::time::sleep(Duration::from_millis(count as u64 * 10)).await;
            count
        })
        .await;
        let ids: Vec<&str> = scored.iter().map(|(m, _)| m.topic_id.as_str()).collect();
        let counts: Vec<usize> = scored.iter().map(|(_, c)| *c).collect();
        assert_eq!(counts, vec![3, 3, 2, 1], "sorted count-desc");
        // Tiebreak: among the two count=3 topics, `video/beta` < `video/gamma` ascending.
        assert_eq!(ids, vec!["video/beta", "video/gamma", "video/delta", "video/alpha"]);
    }

    #[tokio::test]
    async fn score_topics_with_runs_the_score_closure_concurrently_not_serially() {
        // The point of the refactor: the futures must overlap. If they run serially the total
        // wall-clock is the sum of per-topic sleeps; if they run concurrently it is ~max + a little.
        // TOPIC_DISCOVERY_CONCURRENCY = 8, so with 8 topics each sleeping 50 ms, concurrent ≈ 50 ms
        // and serial ≈ 400 ms. Assert the wall-clock is closer to one wave than N waves.
        let topics: Vec<TopicMeta> = (0..8).map(|i| scored_topic_meta(&format!("video/t{i}"))).collect();
        let start = Instant::now();
        score_topics_with(topics, |_| async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            0
        })
        .await;
        let elapsed = start.elapsed();
        // 8 × 50 ms serial = 400 ms; concurrent in one wave ≈ 50 ms. Permit headroom (drvfs is slow),
        // but the elapsed MUST be under half the serial bound — anything ≥ ~200 ms means they didn't
        // overlap. This is the only test that even tries to observe the speedup, and it is the weakest
        // of the three (timing on a shared CI box is noisy); the contract tests above are the real
        // gate, the integration suite measures the actual relay-round-trip stall.
        assert!(
            elapsed < Duration::from_millis(200),
            "score_topics_with did not run concurrently: elapsed {elapsed:?} (serial would be ~400 ms)"
        );
    }

    #[test]
    // Both assertions are over compile-time constants, and that is the entire point: this test
    // exists to pin the relay-citizenship ruling to the CONSTANTS, so the pair cannot be edited
    // apart without CI noticing. clippy::assertions_on_constants would have us delete exactly the
    // check we want.
    #[allow(clippy::assertions_on_constants)]
    fn topic_discovery_concurrency_is_modest_not_a_burst() {
        // The relay-citizenship ruling (M16 standing): discovery must NOT become a burst of
        // TOPIC_DISCOVERY_CAP simultaneous queries against a public relay. The bound must stay an
        // order of magnitude below the cap so a hostile relay can't see a scanner-shaped fan-out.
        assert!(
            TOPIC_DISCOVERY_CONCURRENCY <= TOPIC_DISCOVERY_CAP / 10,
            "TOPIC_DISCOVERY_CONCURRENCY must stay an order of magnitude below the cap; got {}",
            TOPIC_DISCOVERY_CONCURRENCY
        );
        assert!(TOPIC_DISCOVERY_CONCURRENCY >= 4, "concurrency bound too low — stalls the slow-relay fix");
    }

    // ────────────────── QURATOR-143 W1: one read paints, ranking trickles behind it ───────────────
    //
    // The split contract, pinned WITHOUT a relay (the pure halves only — the relay-fetching halves
    // are the integration suites' job, `hb-it` L2 TOPIC + `hb-wan-it --suite wan-t`):
    //
    //   - `dedupe_announces` (the paint's parse/dedupe/authorship-check half) is pure: newest-wins
    //     per topic_id, per-root window accounting, and the cross-root dedupe the 2026-08-15 Codex
    //     review flagged.
    //   - `rank_discovered_topics` reuses `score_topics_with` UNCHANGED — the QURATOR-82
    //     determinism tests above already pin that seam, which is the proof the split happened at
    //     the seam and not somewhere else.

    use hb_core::topic::build_announce;

    /// A public Topic's announce, tagged with `tags` (the discovery `#t` set), signed fresh.
    fn announce_for(name: &str, tags: &[&str], now: u64) -> Event {
        let (meta, _key) = new_topic(name, "", tags.iter().map(|t| t.to_string()).collect(), false).unwrap();
        let author = Identity::generate();
        build_announce(&author, &meta, now).unwrap()
    }

    #[test]
    fn dedupe_announces_keeps_one_row_per_topic_id_across_roots() {
        // The same topic can legitimately surface under more than one root's QUERY (Codex
        // 2026-08-15): dedupe is by topic_id, never by (root, topic) pair. Since QURATOR-133 an
        // announce cannot carry a second ROOT tag, so the honest cross-root shape is the ESCALATION
        // merge — the same topic found by the shared query under its own root and again by a
        // starved root's follow-up — which is `TopicDiscoveries::merge`'s exact job.
        let a = announce_for("video/own-topic", &["video"], 1_700_000_000);
        let b = announce_for("audio/own-topic", &["audio"], 1_700_000_050);
        let mut shared = dedupe_announces(&mut [a, b], &["video".into(), "audio".into()]);
        // The escalated root's follow-up re-finds `video/own-topic` (the relay serving the same
        // event again under the narrower filter) — merge must keep ONE row for it, not two.
        let again = announce_for("video/own-topic", &["video"], 1_700_000_000);
        let mut escalated = dedupe_announces(&mut [again], &["video".into()]);
        escalated.hit_limit = true;
        shared.merge(escalated);
        let mut ids: Vec<&str> = shared.topics.iter().map(|m| m.topic_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids.len(), 2, "two distinct topic_ids, one row each — the shared id deduped: {ids:?}");
        // Per-root accounting: video's window counted the announce twice across the two fetches
        // (once in the shared page, once in the escalation), audio's once.
        assert_eq!(shared.root_event_counts.get("video"), Some(&2));
        assert_eq!(shared.root_event_counts.get("audio"), Some(&1));
        // The escalation's limit flag survives the merge (the caller stays in flood territory).
        assert!(shared.hit_limit);
    }

    #[test]
    fn dedupe_announces_keeps_the_newest_announce_per_topic_id() {
        // A re-announce supersedes: same topic_id, newer created_at wins, regardless of batch order.
        let old = announce_for("video/re-announced", &["video"], 1_700_000_000);
        let new = announce_for("video/re-announced", &["video"], 1_700_000_999);
        let out = dedupe_announces(&mut [new.clone(), old], &["video".into()]);
        assert_eq!(out.topics.len(), 1);
        // `new` is distinguishable from `old` only by created_at here (same payload); the count of
        // PARSED events in the window is what proves both were seen and folded to one row.
        assert_eq!(out.root_event_counts.get("video"), Some(&2), "both announces parsed; one row kept");
        let _ = new;
    }

    #[test]
    fn dedupe_announces_drops_a_d_tag_payload_topic_id_mismatch() {
        // The paint path keeps the authorship check: an announce whose `d` tag names one topic but
        // whose signed payload names another must not poison discovery — and must not consume a
        // per-root slot either (it was never a valid candidate).
        let (victim, _k) = new_topic("video/paint-mismatch-victim", "", vec![], false).unwrap();
        let (attacker, _k2) = new_topic("video/paint-mismatch-other", "", vec![], false).unwrap();
        let author = Identity::generate();
        let mismatched = build_mismatched_announce(&author, &victim.topic_id, &attacker, 1_700_000_000);
        let out = dedupe_announces(&mut [mismatched], &["video".into()]);
        assert!(out.topics.is_empty(), "a d-tag/payload mismatch is not a discovery candidate");
        assert!(out.root_event_counts.is_empty(), "it must not consume a root's window slot either");
    }

    #[test]
    fn paint_reports_hit_limit_only_at_the_fetch_ceiling() {
        // `hit_limit` is computed by the CALLER (it knows the page size); `dedupe_announces` defaults
        // it to false and `discover_public_topics_paint` sets it from the fetched length. Pin the
        // decision rule itself against the constant: exactly-at-limit is a hit, one-under is not.
        assert_eq!(TOPIC_DISCOVERY_FETCH_LIMIT, 1000);
        // (The >= rule lives inline in discover_public_topics_paint; this pins the ceiling it is
        // compared against so the escalation gate cannot silently drift with the budget.)
    }

    #[test]
    fn escalate_only_roots_missing_from_a_hit_limit_response() {
        // The escalation's selection rule, exercised through the pure data it reads: a root that got
        // at least one slot (present in root_event_counts) is NOT starved and must not escalate,
        // even when another root flooded the page. Absent == 0 == starved.
        let flood = announce_for("video/flood", &["video"], 1_700_000_000);
        let tags = ["video".to_string(), "audio".to_string()];
        let out = dedupe_announces(&mut [flood], &tags);
        let starved = starved_roots(&tags, &out.root_event_counts);
        assert_eq!(starved.len(), 1, "exactly one starved root");
        assert_eq!(starved[0].as_str(), "audio", "video got its slot; audio got none");
    }

    // The rank half's contracts (count-desc order, id tiebreak, error-tolerance, bounded
    // concurrency, the TOPIC_DISCOVERY_CAP truncation) are score_topics_with's contracts, pinned
    // UNCHANGED by the QURATOR-82 tests above — that they need no editing is itself the proof the
    // split happened at the seam. rank_discovered_topics is that seam plus a member_count closure,
    // and exercising it needs a live relay client, which is the integration suites' job (hb-it L2
    // TOPIC / hb-wan-it wan-t), not a unit test's.
}
