//! QURATOR-164 item 3 — the BACKGROUND FETCH DRIVER (owner ruling 2026-09-04, option (b)).
//!
//! The ruling picked a background driver over a UI-driven ask wave: *"a background driver that
//! watches fingerprints and fetches unattended."* So nobody clicks. This loop notices that a
//! collection this node already holds has moved on to a new snapshot, and re-fetches it — asking
//! CARRIERS first and the author only as a last resort, which is the whole point of QURATOR-164
//! (spreading load off the author, since prefetch takes no caps).
//!
//! Four properties, each from a ruling and each with a pin below:
//!
//! 1. **The only trigger is a fingerprint change.** There is deliberately no manual retrigger, and
//!    *that absence IS the anti-nuisance-traffic control* — not a rate limit and not a cap. It
//!    self-heals: an offline peer leaves the inequality true, so the next poll simply tries again.
//! 2. **An unknown fingerprint is NOT a change.** If the author's published listing does not carry
//!    one (a pre-M16 listing, or a slug we hold but they no longer publish), the holding is left
//!    alone. Treating "unknown" as "stale" would make the driver re-ask forever against a peer
//!    that can never satisfy it — the exact nuisance traffic property 1 exists to prevent.
//! 3. **Carriers before the author.** Candidates are every contact EXCEPT the author, ordered by
//!    [`crate::peer_wave`], which asks 2-3 per wave, retries a peer 3 times with exponential
//!    backoff, and falls back to the author only once every carrier is exhausted.
//! 4. **Asks go through the production path.** Every ask this loop sends is a production command
//!    body — [`request_manifest_from_inner`] when the target is a carrier, [`request_manifest_inner`]
//!    when the target IS the author. The WAN harness has three times re-implemented a command body
//!    and dropped one step from it; a driver with its own copy would be the fourth, and the dropped
//!    step would not surface until a live run.
//!
//! ⚠ **Refreshing is the BASELINE behaviour, not the opt-in one.** Refreshing something you already
//! hold is what "no manual retrigger" means, so it is not gated on `swarm_caching`. That switch
//! governs discovery-triggered auto-fetch of collections you have NEVER held — QURATOR-189, wired
//! as [`discover_unheld`]: one authorless ask to the author per unheld published collection, with
//! "never held" derived from the manifest cache so a collection stops qualifying the moment it is
//! held (no seen-set, nothing new persisted). The switch's OTHER half — relay-caching, retaining
//! data that passed through this node — stays unwired by owner ruling (QURATOR-164: "that is an
//! architectural question of its own").
//!
//! Shape: the decisions are pure functions ([`stale_holdings`], [`candidates_for`]) with the clock
//! and the network kept out of them; the loop is a thin shell that gathers inputs, calls them, and
//! sends. §5's integration half — a live two-machine run — is owed on QURATOR-164 and is not
//! discharged by any unit test here.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use hb_core::TransportTicket;
use hb_net::RelayClient;
use nostr::prelude::*;
use tokio::time::Instant;

use crate::commands::browse::{contact_share_code, resolve_peer};
use crate::commands::chat::{
    decode_dms, request_manifest_from_inner, request_manifest_inner, DM_FETCH_MARGIN_SECS,
    DM_INBOX_FETCH_LIMIT,
};
use crate::commands::fulfil::redeem_manifest_ticket_inner;
use crate::identity_state::SharedIdentity;
use crate::manifest_cache::CachedKey;
use crate::net::SharedRelay;
use crate::peer_wave::{next_action, Candidate, WaveAction};
use crate::store::{manifest_ask_key, parse_watermark_ts, ManifestAsk, DataStore};
use crate::transport_state::SharedEndpoint;

/// How often the driver re-reads published fingerprints.
///
/// Minutes, not seconds: a snapshot changes when a human edits a collection, and every poll costs
/// one listing resolve per author held. The ask throttle paces what this produces, but the cheapest
/// relay traffic is the request never made.
const POLL_INTERVAL: Duration = Duration::from_secs(300);

/// QURATOR-197 (F19) — failed DIALS against one ask before the redeem poll stops dialling it,
/// mirroring `peer_wave`'s ruled convention (owner 2026-09-04: *"3 attempts against the same peer
/// before moving on"*). Exhaustion is terminal **for that ask**: the recovery paths are a FRESH
/// ask (`record_manifest_ask` resets the budget with the new nonce) or a manual redeem on the
/// Chat page — the bound here gags the UNATTENDED loop, never the human.
const REDEEM_DIAL_CAP: u32 = 3;

/// QURATOR-197 (F19) — the re-dial backoff base, at POLL scale. `peer_wave::backoff_for`'s 2 s
/// base is invisible to a 300 s poll (the backoff would lapse before the next tick), so the base
/// here is two poll intervals: the first retry waits out one whole poll, each further failure
/// doubles the wait, and the cap stops the dialling outright.
const REDEEM_DIAL_BACKOFF_BASE_SECS: u64 = POLL_INTERVAL.as_secs() * 2;

/// A held collection whose author has published a newer snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleHolding {
    pub author_npub: String,
    pub slug: String,
    /// The fingerprint now published — what the ask names as seen, and what a reply must match.
    pub want_fingerprint: String,
}

/// Pure core — which of `held` have been superseded by `published`.
///
/// `published` is `(slug, current fingerprint)` as it arrives on the author's listing, so the
/// fingerprint is optional: a listing without the M16 marker carries none. **A missing fingerprint,
/// and a slug absent from the listing entirely, both mean NOT STALE** (property 2 in the module
/// doc) — the driver only ever acts on a fingerprint it can actually compare.
pub(crate) fn stale_holdings(held: &[CachedKey], published: &[(String, Option<String>)]) -> Vec<StaleHolding> {
    held.iter()
        .filter_map(|k| {
            let (_, current) = published.iter().find(|(slug, _)| *slug == k.slug)?;
            let current = current.as_ref()?;
            (current != &k.fingerprint).then(|| StaleHolding {
                author_npub: k.npub.clone(),
                slug: k.slug.clone(),
                want_fingerprint: current.clone(),
            })
        })
        .collect()
}

/// Pure core — which of `author`'s published collections this node has NEVER held (QURATOR-189,
/// the `swarm_caching` discovery tier).
///
/// "Newly discovered" is DERIVED, never persisted: a collection stops qualifying the moment it
/// appears in `held`, which is the whole anti-nuisance control — until the reply lands the ask
/// repeats, which is the same self-heal the refresh path rests on ("an offline peer leaves the
/// inequality true"). A listing without a fingerprint does not qualify either, for the same reason
/// as property 2 in the module doc: the ask NAMES the snapshot ("what a reply must match"), and a
/// pre-M16 listing cannot name one.
pub(crate) fn unheld_collections(
    held: &[CachedKey],
    author: &str,
    published: &[(String, Option<String>)],
) -> Vec<(String, String)> {
    published
        .iter()
        .filter(|(slug, _)| !held.iter().any(|k| k.npub == author && k.slug == *slug))
        .filter_map(|(slug, fp)| Some((slug.clone(), fp.as_ref()?.clone())))
        .collect()
}

/// Pure core — who may be asked for `author`'s collection.
///
/// Every contact except the author themself (they are the fallback, held separately) and except
/// this node (asking yourself for what you already hold is a no-op that would still burn a wave
/// slot and a throttle slot).
pub(crate) fn candidates_for(contacts: &[String], author: &str, me: &str) -> Vec<Candidate> {
    contacts
        .iter()
        .filter(|n| n.as_str() != author && n.as_str() != me)
        .map(Candidate::fresh)
        .collect()
}

/// Per-collection ask state, in memory for the process lifetime.
pub(crate) struct AskState {
    pub peers: Vec<Candidate>,
    pub author: Candidate,
}

/// Record an attempt against `npub` in `state`, so the wave's backoff and 3-try cap advance.
fn note_attempt(state: &mut AskState, npub: &str, now: Instant) {
    let target = if state.author.npub == npub {
        Some(&mut state.author)
    } else {
        state.peers.iter_mut().find(|c| c.npub == npub)
    };
    if let Some(c) = target {
        c.attempts += 1;
        c.last_attempt = Some(now);
    }
}

/// What one poll actually did — the harness's only observable, and the reason [`poll_once`] is a
/// separate function rather than a block inside the loop.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PollOutcome {
    /// Holdings this poll found superseded.
    pub stale: Vec<StaleHolding>,
    /// The npubs actually asked, in wave order — carriers first, then the author.
    pub asked: Vec<String>,
    /// Slugs redeemed this poll, from tickets answering asks a PREVIOUS poll sent.
    pub redeemed: Vec<String>,
    /// Collections this poll asked an AUTHOR for that this node has NEVER held — the
    /// `swarm_caching` discovery tier (QURATOR-189). Entries are `author|slug`; empty whenever
    /// the switch is off. Like `asked`, this records asks SENT, not manifests received: the reply
    /// lands in a later poll's `redeemed`.
    pub discovered: Vec<String>,
}

/// The peers this node has actually asked — the allow-list for [`redeem_pending_tickets`].
///
/// **This is a security boundary, not a filter for tidiness.** It is what stops an unsolicited
/// "ticket" from a stranger reaching the redeem body at all: a ticket names a node address to dial,
/// so accepting one nobody asked for would let any peer make this node dial an address of their
/// choosing. Keys are `peer|author|slug`; the peer is the first segment.
/// Generic in the value because it reads only the KEYS — which keeps the allow-list decoupled
/// from whatever the ask record holds, and lets the test exercise the segment logic without
/// fabricating a `ManifestAsk`.
pub(crate) fn asked_peers<V>(asks: &HashMap<String, V>) -> HashSet<String> {
    asks.keys().filter_map(|k| k.split('|').next()).map(String::from).collect()
}

/// Unix seconds now — the same helper shape every module here carries (cf. `auto_approve`).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// QURATOR-197 — the `since` floor for [`redeem_pending_tickets`]' inbox fetch.
///
/// A wrap this loop can redeem answers an ask this node sent (the `allow` list and the ask lookup
/// in the redeem body both enforce that), so it cannot predate the oldest still-pending ask:
/// anchoring the window there covers every legitimate — including much-delayed — answer, while
/// stopping the 300 s poll from re-fetching all-time history every tick. `DM_FETCH_MARGIN_SECS`
/// is the same NIP-59 outer-stamp wobble allowance `dm_inbox_filter` subtracts (a peer's clock
/// may also sit behind ours). The `.min(now)` clamp keeps a hostile or skewed future `sent_at`
/// from future-poisoning the window (the codebase's clamp-to-now discipline). Any absent or
/// unparseable trace maps to 0, forcing the WHOLE window open — the pre-QURATOR-197 fetch's
/// behaviour, never silent delivery loss.
fn ticket_inbox_since(asks: &HashMap<String, ManifestAsk>, now: u64) -> u64 {
    asks.values()
        .map(|a| {
            parse_watermark_ts(&a.sent_at)
                .map(|ts| ts.timestamp().max(0) as u64)
                .unwrap_or(0)
        })
        .min()
        .map(|oldest| oldest.saturating_sub(DM_FETCH_MARGIN_SECS).min(now))
        .unwrap_or(0)
}

/// QURATOR-197 — the redemption poll's inbox filter: BOTH bounds the chat inbox's
/// `dm_inbox_filter` (`commands/chat.rs`) already carries. The `.limit()` keeps the fetch budget
/// ours (CWE-400 — otherwise the relay's own default decides how much the 300 s poll
/// re-decrypts); the `since` window (from [`ticket_inbox_since`]) stops the poll from
/// re-fetching all-time history. `since == 0` omits the window — the cold/fail-open anchor.
fn ticket_inbox_filter(me: PublicKey, since: u64) -> Filter {
    let f = Filter::new().kind(Kind::GiftWrap).pubkey(me).limit(DM_INBOX_FETCH_LIMIT);
    if since > 0 {
        f.since(Timestamp::from(since))
    } else {
        f
    }
}

/// QURATOR-197 (F19) — pure core: the re-dial wait after `attempts` failed dials of one ask.
///
/// `peer_wave::backoff_for`'s sibling at poll scale: `0` failures means nothing to back off from,
/// otherwise the base doubles per failure (600 s, 1200 s, …). The shift is clamped so an
/// out-of-range `attempts` (which the cap should prevent, but this function does not get to
/// assume) can never overflow.
fn redeem_backoff_for(attempts: u32) -> u64 {
    if attempts == 0 {
        return 0;
    }
    let doublings = (attempts - 1).min(16);
    REDEEM_DIAL_BACKOFF_BASE_SECS.saturating_mul(1u64 << doublings)
}

/// QURATOR-197 (F19) — pure core: may this ask be dialled at `now_unix`?
///
/// `peer_wave::Candidate::is_ready`'s sibling (`!is_exhausted && backoff elapsed`), collapsed to
/// one predicate over the persisted counter pair. False once the cap is reached — terminal for
/// the ask, see [`REDEEM_DIAL_CAP`] — or while the exponential backoff still has time to run.
/// A never-failed ask (`last_fail_unix == 0`) is always ready.
fn redeem_dial_ready(attempts: u32, last_fail_unix: u64, now_unix: u64) -> bool {
    attempts < REDEEM_DIAL_CAP
        && now_unix.saturating_sub(last_fail_unix) >= redeem_backoff_for(attempts)
}

/// Drain tickets that answer asks THIS node sent, redeeming each through the production body.
///
/// Runs at the START of a poll, before the staleness check, so a ticket that arrived since the last
/// poll is in the cache before we decide whether anything is still stale — otherwise the driver
/// would re-ask for a collection it had just been handed.
///
/// ⚠ **This exists because redemption had no unattended path.** The only other caller of
/// `redeem_manifest_ticket` is the chat page, whose DM poll is created in `onMount`, torn down on
/// destroy, and gated on `!document.hidden`. So before this, the driver could ask unattended and
/// then sit on the answer until the user happened to open the Chat tab with the window focused —
/// which is not the background driver that was ruled for (owner, 2026-09-04, option (b)).
async fn redeem_pending_tickets(
    store: &DataStore,
    identity: &hb_core::Identity,
    own_npub: &str,
    live: &SharedIdentity,
    endpoint: &SharedEndpoint,
) -> Vec<String> {
    let mut redeemed = Vec::new();
    let Ok(asks) = store.load_manifest_asks() else { return redeemed };
    if asks.is_empty() {
        return redeemed;
    }
    let allow = asked_peers(&asks);

    // One poll, one clock read — the fetch window and the re-dial gate below must agree on "now".
    let now = now_secs();
    // QURATOR-197 — bound the redemption fetch: the window anchors to the oldest pending ask
    // (minus the shared wobble allowance, clamped to now), the budget to the shared inbox limit.
    // Neither narrows a legitimate answer — see `ticket_inbox_since`'s doc.
    let since = ticket_inbox_since(&asks, now);

    // A short-lived client per poll, as the auto-approve loop does: the persistent shared client
    // belongs to the command surface and must not be held across this loop's sleeps.
    let relays = crate::net::relay_urls(store);
    let Ok(client) = RelayClient::connect(identity, &relays, crate::net::RELAY_TIMEOUT).await else {
        return redeemed;
    };
    let wraps = client
        .fetch(
            ticket_inbox_filter(identity.public_key(), since),
            crate::net::RELAY_TIMEOUT,
        )
        .await;
    client.disconnect().await;
    let Ok(wraps) = wraps else { return redeemed };

    for msg in decode_dms(own_npub, identity, wraps, Some(&allow)).await {
        let trimmed = msg.content.trim();
        let Ok(ticket) = serde_json::from_str::<TransportTicket>(trimmed) else { continue };
        if ticket.verify_shape().is_err() {
            continue;
        }
        // The fingerprint we asked for, so the backend's staleness gate has a real comparand rather
        // than None. An authorless ticket is the peer serving their own collection, so the author
        // key is the sender.
        let author = ticket.author_npub.clone().unwrap_or_else(|| msg.from.clone());
        let entry = asks.get(&manifest_ask_key(&msg.from, &author, &ticket.slug));
        let want = entry.map(|a| a.fingerprint_seen.clone());

        // QURATOR-197 (F19) — the re-dial gate. A wrap that answered one of our asks stays on the
        // relay until it expires, so this loop re-reads it EVERY poll; without this check a
        // permanently-failing ticket was re-dialled every 300 s forever (the claim re-grants the
        // same `request_id`, and only a success spends). Backoff first, cap terminal — see
        // `redeem_dial_ready`. An ask missing from our own map cannot be dial-budgeted here; the
        // claim inside the body still refuses it as `Unsolicited`.
        if !entry
            .map(|a| redeem_dial_ready(a.dial_attempts, a.dial_last_fail_unix, now))
            .unwrap_or(true)
        {
            tracing::debug!(
                from = %truncate(&msg.from),
                slug = %ticket.slug,
                "fetch driver: redeem backed off — a failing ticket is not re-dialled this poll"
            );
            continue;
        }

        match redeem_manifest_ticket_inner(
            msg.from.clone(),
            trimmed.to_string(),
            want,
            live,
            store,
            endpoint,
        )
        .await
        {
            Ok(_) => {
                tracing::info!(
                    from = %truncate(&msg.from),
                    slug = %ticket.slug,
                    "fetch driver: redeemed a ticket answering our ask"
                );
                redeemed.push(ticket.slug.clone());
            }
            Err(e) => {
                tracing::debug!(
                    from = %truncate(&msg.from),
                    error = %e,
                    "fetch driver: redeem failed; the ask stays retryable inside its dial budget"
                );
                // QURATOR-197 (F19) — count the failure so the gate above bites on the NEXT poll.
                // `note_failed_dial` no-ops for pre-claim refusals (nothing dialled), so this is
                // safe to call unconditionally. Best-effort: a backoff we failed to persist is a
                // retry we fail to avoid, not a lost delivery.
                if let Err(be) =
                    store.note_failed_dial(&msg.from, &author, &ticket.slug, &ticket.request_id)
                {
                    tracing::warn!(
                        from = %truncate(&msg.from),
                        error = %be,
                        "fetch driver: could not record the failed dial; its backoff is lost"
                    );
                }
            }
        }
    }
    redeemed
}

/// QURATOR-189 — the `swarm_caching` discovery tier's network half.
///
/// The refresh loop inside [`poll_once`] only ever tends what is held; this asks each contact, as
/// the AUTHOR of their own listings, for collections this node has NEVER held — so a fresh node
/// with an empty cache still fills. Public-only is by construction: `resolve_peer` builds listings
/// from the browse key, and private listings come from a different function
/// (`hb_net::priv_browse::fetch_private_listings`) this path never calls. Keyless contacts
/// self-skip the same way they do everywhere else: `contact_share_code` answers `FollowOnly` and
/// `resolve_peer`'s no-browse-key arm yields no collections.
///
/// Returns `author|slug` per ask SENT, not per manifest received — the reply lands in a later
/// poll's `redeemed`, exactly like the refresh tier.
async fn discover_unheld(
    store: &DataStore,
    identity: &hb_core::Identity,
    own_npub: &str,
    contacts: &[crate::store::CachedPeer],
    held: &[CachedKey],
    relay: &SharedRelay,
) -> Vec<String> {
    let mut discovered = Vec::new();
    for contact in contacts {
        if contact.npub == own_npub {
            // Never discover from yourself: the listings would be your own, and the ask is a
            // self-send `request_manifest_inner` already refuses.
            continue;
        }
        let Ok(share_code) = contact_share_code(contact) else { continue };
        let published: Vec<(String, Option<String>)> =
            match resolve_peer(&share_code, identity, store, relay).await {
                Ok(peer) => peer
                    .collections
                    .into_iter()
                    .map(|c| (c.collection.slug, c.snapshot_fingerprint))
                    .collect(),
                Err(e) => {
                    tracing::debug!(
                        author = %truncate(&contact.npub),
                        error = %e,
                        "fetch driver: discovery listing resolve failed"
                    );
                    continue;
                }
            };

        for (slug, fingerprint) in unheld_collections(held, &contact.npub, &published) {
            // ⚠ THE SHAPE OF THE ASK MUST MATCH WHO IS BEING ASKED — same rule as the refresh
            // loop: the target IS the author, so the ask is AUTHORLESS. `approval_body_for`
            // routes purely on `author_npub` being present, and an author never caches its own
            // publish, so an author-bearing ask sent TO the author makes it look for a cached
            // copy of its own collection and miss, silently. `request_manifest_inner` takes the
            // shared 1/sec throttle slot itself, so a contact with many unheld collections is
            // asked slowly rather than in a burst.
            let sent = request_manifest_inner(
                &contact.npub,
                &slug,
                &fingerprint,
                None,
                None,
                identity,
                store,
                relay,
            )
            .await;
            match sent {
                Ok(()) => tracing::info!(
                    author = %truncate(&contact.npub),
                    slug = %slug,
                    "fetch driver: asked the author for a collection never held (swarm_caching)"
                ),
                Err(e) => tracing::debug!(
                    author = %truncate(&contact.npub),
                    error = %e,
                    "fetch driver: discovery ask failed"
                ),
            }
            discovered.push(format!("{}|{}", contact.npub, slug));
        }
    }
    discovered
}

/// The driver loop. Spawned once at startup; runs for the process lifetime.
///
/// Deliberately thin: it owns the cadence and the attempt state and delegates every decision to
/// [`poll_once`]. The split is what makes the behaviour reachable by the WAN harness — a loop that
/// sleeps five minutes between observations cannot be driven, and a harness carrying its own copy
/// of the poll would be the drift this project has already paid for three times.
pub(crate) async fn run_fetch_driver_loop(
    store: DataStore,
    live_npub: SharedIdentity,
    relay: SharedRelay,
    endpoint: SharedEndpoint,
) {
    // Keyed (author_npub, slug). In memory, like the auto-approve loop's caps: a restart re-reads
    // fingerprints and starts its attempt counting over, which is correct — a fresh process has no
    // reason to believe a peer that was down an hour ago still is.
    let mut states: HashMap<(String, String), AskState> = HashMap::new();

    tracing::info!(
        poll_secs = POLL_INTERVAL.as_secs(),
        "fetch driver: loop started (refetch on fingerprint change; author asked alongside carriers)"
    );

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        poll_once(&store, &live_npub, &relay, &endpoint, &mut states).await;
    }
}

/// One poll of the driver: read what is held, resolve each author's published listing, and ask for
/// anything whose fingerprint has moved.
///
/// Takes `states` by reference so attempt counts survive across polls — that is what makes the
/// backoff and the 3-try cap mean anything. Returns [`PollOutcome`] so a caller (the WAN harness)
/// can assert on WHO was asked, which is the whole claim of QURATOR-164: a carrier is asked, not
/// only the author.
pub(crate) async fn poll_once(
    store: &DataStore,
    live_npub: &SharedIdentity,
    relay: &SharedRelay,
    endpoint: &SharedEndpoint,
    states: &mut HashMap<(String, String), AskState>,
) -> PollOutcome {
    let mut outcome = PollOutcome::default();

    // Per-poll identity read, for the same reason the auto-approve loop does it: a fresh install
    // has no identity when the driver starts, and a one-shot snapshot would leave it dead for the
    // whole session.
    let (identity, own_npub) = {
        let guard = live_npub.read().await;
        let Some(id) = guard.as_ref() else { return outcome };
        (id.identity.clone(), id.npub())
    };

    // QURATOR-250 — expire stale ask-trace entries before the redeem drain reads the same map:
    // an entry past retention must not authorise a dial or hold the ticket-inbox window open
    // (`ticket_inbox_since` anchors on the OLDEST entry) in the same tick it dies. Ahead of the
    // holdings/discovery early returns below on purpose — a node that holds nothing with
    // discovery off must still stop carrying entries the retention policy has declared dead.
    // A failure here is hygiene-level: log it and let the poll proceed (the next tick retries).
    match store.evict_expired_manifest_asks(chrono::Utc::now()) {
        Ok(removed) if removed > 0 => {
            tracing::info!(evicted = removed, "fetch driver: expired stale manifest asks");
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "fetch driver: manifest-ask eviction skipped"),
    }

    // Redeem BEFORE checking staleness — see `redeem_pending_tickets`' doc.
    outcome.redeemed =
        redeem_pending_tickets(store, &identity, &own_npub, live_npub, endpoint).await;

    let held = crate::manifest_cache::list(&store.manifest_cache_dir());
    // QURATOR-189 — the discovery tier's gate, read the way `resolve_peer` reads `big_relay_url`:
    // a missing or unreadable settings file must mean OFF, never a failed poll. The early return
    // now fires only when there is nothing held AND discovery is off: a fresh node (empty `held`)
    // is precisely the case that needs discovery, so it must NOT strand here.
    let swarm_caching = store.load_settings().ok().flatten().unwrap_or_default().swarm_caching;
    if held.is_empty() && !swarm_caching {
        return outcome;
    }
    let Ok(contacts) = store.list_contacts() else { return outcome };
    let contact_npubs: Vec<String> = contacts.iter().map(|c| c.npub.clone()).collect();

    // The opt-in discovery tier runs before the refresh grouping only because that grouping
    // consumes `held` below; both tiers' asks share one 1/sec throttle, so the order between
    // them changes nothing observable.
    if swarm_caching {
        outcome.discovered =
            discover_unheld(store, &identity, &own_npub, &contacts, &held, relay).await;
    }

    // Group holdings by author so each author's listing is resolved once per poll, not once per
    // collection held.
    let mut by_author: HashMap<String, Vec<CachedKey>> = HashMap::new();
    for k in held {
        by_author.entry(k.npub.clone()).or_default().push(k);
    }

    for (author_npub, keys) in by_author {
        let Some(contact) = contacts.iter().find(|c| c.npub == author_npub) else {
            // We hold a manifest from someone who is not a contact, so there is no share code to
            // resolve their listing with. Nothing to compare against; leave it alone.
            continue;
        };
        let Ok(share_code) = contact_share_code(contact) else { continue };
        let published: Vec<(String, Option<String>)> =
            match resolve_peer(&share_code, &identity, store, relay).await {
                Ok(peer) => peer
                    .collections
                    .into_iter()
                    .map(|c| (c.collection.slug, c.snapshot_fingerprint))
                    .collect(),
                Err(e) => {
                    tracing::debug!(
                        author = %truncate(&author_npub),
                        error = %e,
                        "fetch driver: listing resolve failed"
                    );
                    continue;
                }
            };

        for stale in stale_holdings(&keys, &published) {
            let key = (stale.author_npub.clone(), stale.slug.clone());
            let state = states.entry(key).or_insert_with(|| AskState {
                peers: candidates_for(&contact_npubs, &stale.author_npub, &own_npub),
                author: Candidate::fresh(stale.author_npub.clone()),
            });

            let now = Instant::now();
            // The wave already carries the author when the author is ready (owner ruling
            // 2026-09-04: whoever has a free slot answers first), so there is no separate fallback
            // branch here.
            let targets = match next_action(&state.peers, &state.author, now) {
                WaveAction::Ask(sources) => sources,
                // Backing off, or every source exhausted. Both are handled by simply not asking
                // this poll — the next one re-evaluates, which is what makes the give-up bound
                // self-healing rather than terminal.
                WaveAction::Wait(_) | WaveAction::GiveUp => continue,
            };

            outcome.stale.push(stale.clone());
            for target in targets {
                // ⚠ THE SHAPE OF THE ASK MUST MATCH WHO IS BEING ASKED. `approval_body_for` routes
                // purely on `author_npub` being present: Some ⇒ `send_cached_manifest_inner` (read
                // the CACHE), None ⇒ `send_full_list_inner` (build from the collection). An author
                // does not cache its own published manifest — the only production
                // `manifest_cache::put` is the import path — so an author-bearing ask sent TO that
                // author makes it look for a cached copy of its own collection and miss. Since the
                // author rides in every wave (owner ruling 2026-09-04), getting this wrong fails
                // the author leg of EVERY fetch, silently.
                //
                // Both are production bodies; neither is a second copy. Each takes the shared 1/sec
                // throttle slot itself, so a wide wave leaves slowly rather than bursting.
                let sent = if target == stale.author_npub {
                    request_manifest_inner(
                        &target,
                        &stale.slug,
                        &stale.want_fingerprint,
                        None,
                        None,
                        &identity,
                        store,
                        relay,
                    )
                    .await
                } else {
                    request_manifest_from_inner(
                        &target,
                        &stale.author_npub,
                        &stale.slug,
                        &stale.want_fingerprint,
                        None,
                        &identity,
                        store,
                        relay,
                    )
                    .await
                };
                match sent {
                    Ok(()) => tracing::info!(
                        asked = %truncate(&target),
                        slug = %stale.slug,
                        "fetch driver: asked for a refreshed manifest"
                    ),
                    Err(e) => tracing::debug!(
                        asked = %truncate(&target),
                        error = %e,
                        "fetch driver: ask failed"
                    ),
                }
                note_attempt(state, &target, now);
                outcome.asked.push(target);
            }
        }
    }

    outcome
}

/// npubs are truncated in logs — a full one identifies a person, and this loop names every peer it
/// asks. Same treatment `send_dm_inner` gives a recipient.
fn truncate(npub: &str) -> String {
    npub.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(npub: &str, slug: &str, fp: &str) -> CachedKey {
        CachedKey { npub: npub.into(), slug: slug.into(), fingerprint: fp.into() }
    }

    fn ask(sent_at: &str) -> ManifestAsk {
        ManifestAsk {
            fingerprint_seen: "fp".into(),
            sent_at: sent_at.into(),
            nonce: String::new(),
            claimed_by: None,
            spent: false,
            dial_attempts: 0,
            dial_last_fail_unix: 0,
        }
    }

    /// The store fixture the ask-lifecycle tests need — same shape as `store.rs`'s `test_store`
    /// (that one is private to its module), kept here so the store.rs footprint of this ticket
    /// stays minimal.
    fn test_store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    // ── QURATOR-197 (F19) — the redeem-path re-dial backoff ──────────────────────────────────
    //
    // The ask wave already had `peer_wave` (attempts + last-attempt + exponential backoff + a 3
    // cap) for SENDING asks; the REDEEM half — a wrap that answered one of our asks persists on
    // the relay and was re-dialled every 300 s poll forever, because `claim_manifest_ask`
    // re-grants the same `request_id` and only a SUCCESS spends — had no equivalent. These pin
    // the one added here.

    /// QURATOR-197 (F19), acceptance (b): the re-dial delay GROWS — the base is two poll
    /// intervals (a wave-scale 2 s base is invisible to a 300 s poll), and each further failure
    /// doubles it.
    ///
    /// MUTATION (P-10) — in `redeem_backoff_for` (the pure fn between `ticket_inbox_filter` and
    /// `redeem_pending_tickets`), replace the multiply line
    /// `REDEEM_DIAL_BACKOFF_BASE_SECS.saturating_mul(1u64 << doublings)` with
    /// `REDEEM_DIAL_BACKOFF_BASE_SECS` (a flat base) → the third and fourth asserts red.
    #[test]
    fn redeem_backoff_doubles_per_failed_dial_and_starts_at_two_polls() {
        assert_eq!(redeem_backoff_for(0), 0, "a never-failed ask has nothing to back off from");
        assert_eq!(REDEEM_DIAL_BACKOFF_BASE_SECS, 600, "the base is two 300 s polls");
        assert_eq!(redeem_backoff_for(1), 600, "the first retry waits out one whole poll");
        assert_eq!(redeem_backoff_for(2), 1_200, "the second failure doubles the wait");
        assert_eq!(redeem_backoff_for(3), 2_400, "and so on");
    }

    /// QURATOR-197 (F19), acceptance (c): once the cap is reached the ask is NEVER dial-ready
    /// again, however much time passes — exhaustion is terminal for that ask (a fresh ask or a
    /// manual redeem are the recovery paths; see `REDEEM_DIAL_CAP`'s doc), and deliberately NOT
    /// the same state as `spent`.
    ///
    /// MUTATION (P-10) — in `redeem_dial_ready` (the pure fn directly after `redeem_backoff_for`),
    /// change the cap clause `attempts < REDEEM_DIAL_CAP` to `attempts < REDEEM_DIAL_CAP + 1`
    /// → this test reds (a capped ask would become ready again once its backoff lapsed).
    #[test]
    fn a_capped_ask_is_never_dial_ready_again_even_after_its_backoff_lapses() {
        let far_future = 2_000_000_000u64;
        assert!(
            !redeem_dial_ready(REDEEM_DIAL_CAP, 0, far_future),
            "the cap is terminal — no elapsed time re-arms it"
        );
        assert!(
            redeem_dial_ready(REDEEM_DIAL_CAP - 1, 0, far_future),
            "one failure short of the cap is only backoff, not exhaustion"
        );
    }

    /// QURATOR-197 (F19), acceptance (b) at the gate: an ask INSIDE its backoff window waits
    /// (the poll skips it — no dial), one at or past the boundary dials.
    ///
    /// MUTATION (P-10) — in `redeem_dial_ready` (the pure fn directly after `redeem_backoff_for`),
    /// change the window comparison `>= redeem_backoff_for(attempts)` to
    /// `> redeem_backoff_for(attempts) + 1` → the second assert reds; changing it to always
    /// `true` reds the first.
    #[test]
    fn an_ask_inside_its_backoff_waits_and_one_past_it_dials() {
        let now = 1_788_220_800u64;
        let base = REDEEM_DIAL_BACKOFF_BASE_SECS;
        assert!(!redeem_dial_ready(1, now - (base - 1), now), "one second inside the window: wait");
        assert!(redeem_dial_ready(1, now - base, now), "at the boundary exactly: dial");
        assert!(redeem_dial_ready(0, 0, now), "a never-failed ask dials at once");
    }

    /// QURATOR-197 (F19), acceptance (a): attempts ACCUMULATE per ask, and survive a reload —
    /// the ask persists in `manifest_asks.json` and the wrap persists on the relay, so both
    /// halves of the loop survive a restart and the counter must too (the design choice recorded
    /// on `ManifestAsk::dial_attempts`: an in-memory budget like `AskState` would reset exactly
    /// when the storm does not).
    ///
    /// MUTATION (P-10) — in `store.rs`'s `note_failed_dial` (the fn directly after
    /// `spend_manifest_ask`), delete the `ask.dial_attempts += 1;` statement → the first and
    /// third asserts red (the reloads would keep showing 0).
    #[test]
    fn failed_dials_accumulate_on_the_ask_across_reloads() {
        let (dir, store) = test_store();
        let (npub, slug, nonce) = ("npub1peer", "vault", "nonce-1");
        store
            .record_manifest_ask(npub, npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
            .unwrap();
        // The claim is what makes a later failure a DIAL failure (see the sibling test below).
        store.claim_manifest_ask(npub, npub, slug, nonce, "req-A").unwrap();

        // Poll 1's dial fails.
        store.note_failed_dial(npub, npub, slug, "req-A").unwrap();
        let after_one = store
            .load_manifest_asks()
            .unwrap()
            .get(&manifest_ask_key(npub, npub, slug))
            .unwrap()
            .clone();
        assert_eq!(after_one.dial_attempts, 1, "the first failed dial is recorded");
        assert!(after_one.dial_last_fail_unix > 0, "the wall-clock stamp is set");

        // A "restart": a fresh DataStore over the same directory, then poll 2's dial fails.
        let reopened = DataStore::new(dir.path().to_path_buf());
        reopened.note_failed_dial(npub, npub, slug, "req-A").unwrap();
        let after_two = reopened
            .load_manifest_asks()
            .unwrap()
            .get(&manifest_ask_key(npub, npub, slug))
            .unwrap()
            .clone();
        assert_eq!(after_two.dial_attempts, 2, "the second failure ADDS to the persisted count");
        assert!(
            after_two.dial_last_fail_unix >= after_one.dial_last_fail_unix,
            "the stamp advances, never rewinds"
        );
    }

    /// QURATOR-197 (F19): only a failure that REACHED A DIAL counts. `claimed_by == request_id`
    /// is durable proof that the redeem body's claim returned Granted for this exact ticket (the
    /// claim writes it only after the nonce and spent checks passed), so a pre-claim refusal —
    /// the relay replays the wrap every poll, including stale-nonce ones — must leave the
    /// CURRENT ask's budget alone, or the loop would be gagged without a dial ever happening.
    ///
    /// MUTATION (P-10) — in `store.rs`'s `note_failed_dial` (the fn directly after
    /// `spend_manifest_ask`), change the guard
    /// `if ask.spent || ask.claimed_by.as_deref() != Some(request_id) { return Ok(()); }`
    /// to `if ask.spent { return Ok(()); }` → this test reds (the unclaimed note would count).
    #[test]
    fn a_refusal_that_never_dialled_does_not_burn_the_dial_budget() {
        let (_dir, store) = test_store();
        let (npub, slug, nonce) = ("npub1peer", "vault", "nonce-1");
        store
            .record_manifest_ask(npub, npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
            .unwrap();

        // Before any claim: a wrap failing now fails AT the claim, before any dial.
        store.note_failed_dial(npub, npub, slug, "req-A").unwrap();
        // After req-A claims: a DIFFERENT ticket's failure is a ClaimedByAnother refusal.
        store.claim_manifest_ask(npub, npub, slug, nonce, "req-A").unwrap();
        store.note_failed_dial(npub, npub, slug, "req-B").unwrap();

        let ask = store
            .load_manifest_asks()
            .unwrap()
            .get(&manifest_ask_key(npub, npub, slug))
            .unwrap()
            .clone();
        assert_eq!(ask.dial_attempts, 0, "pre-claim refusals record nothing");
        assert_eq!(ask.dial_last_fail_unix, 0, "and stamp nothing");
    }

    /// QURATOR-197 (F19), acceptance (d): a SUCCESS is unchanged by the backoff — the ask that
    /// succeeded is SPENT, which stays distinguishable on disk from an exhausted one (`spent`
    /// means served; `dial_attempts` means tried-and-failed). A spent ask's replays stop
    /// counting, and a FRESH ask re-arms the dial budget with its new nonce.
    ///
    /// MUTATION (P-10) — in `store.rs`'s `record_manifest_ask` (the struct literal the insert
    /// builds), change the fresh-ask reset `dial_attempts: 0,` to `dial_attempts: 3,` → the
    /// final assert reds (a fresh ask would arrive pre-exhausted).
    #[test]
    fn a_success_spends_a_spent_ask_stops_counting_and_a_fresh_ask_re_arms() {
        let (_dir, store) = test_store();
        let (npub, slug, nonce) = ("npub1peer", "vault", "nonce-1");
        store
            .record_manifest_ask(npub, npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
            .unwrap();
        store.claim_manifest_ask(npub, npub, slug, nonce, "req-A").unwrap();

        // The success path, exactly as production sequences it: claim, dial, spend — no note.
        store.spend_manifest_ask(npub, npub, slug, nonce).unwrap();
        let served = store
            .load_manifest_asks()
            .unwrap()
            .get(&manifest_ask_key(npub, npub, slug))
            .unwrap()
            .clone();
        assert!(served.spent, "a success still spends the ask");
        assert_eq!(served.dial_attempts, 0, "a served ask was never a failing one");

        // The wrap keeps arriving off the relay; a spent ask is terminal, nothing more counts.
        store.note_failed_dial(npub, npub, slug, "req-A").unwrap();
        let still = store
            .load_manifest_asks()
            .unwrap()
            .get(&manifest_ask_key(npub, npub, slug))
            .unwrap()
            .clone();
        assert_eq!(still.dial_attempts, 0, "a spent ask's replays burn no budget");

        // A fresh ask (new nonce) resets the budget and the claim — the ruled recovery path.
        store
            .record_manifest_ask(npub, npub, slug, "fp2", "2026-02-01T00:00:00Z", "nonce-2")
            .unwrap();
        let fresh = store
            .load_manifest_asks()
            .unwrap()
            .get(&manifest_ask_key(npub, npub, slug))
            .unwrap()
            .clone();
        assert_eq!(fresh.dial_attempts, 0, "a fresh ask re-arms the dial budget");
        assert_eq!(fresh.claimed_by, None, "and clears the old claim");
        assert!(!fresh.spent, "and the spent flag");
    }

    /// QURATOR-197 (F19) — the pure cores above must actually be WIRED into the redeem poll:
    /// the gate consulted BEFORE the production redeem body (a backed-off ask never dials, never
    /// even claims), and the failure NOTED after it (so the gate has something to read next
    /// poll). Without this guard the backoff could rot into decoration while every unit test
    /// above stays green — the round-trip-test rule (a guard must end where production ends).
    ///
    /// MUTATION (P-10) — in `redeem_pending_tickets`, delete the gate block that consults
    /// `redeem_dial_ready` (the `if !entry ... { ...; continue; }` immediately before the
    /// `match redeem_manifest_ticket_inner(...)`) → the first assert reds; delete the
    /// `store.note_failed_dial(...)` call inside the `Err` arm → the second reds.
    #[test]
    fn the_redeem_poll_consults_the_gate_and_notes_failures() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find("async fn redeem_pending_tickets(")
            .expect("the redeem helper must exist");
        let end = code[at..]
            .find("pub(crate) async fn run_fetch_driver_loop(")
            .expect("the loop must follow the redeem helper")
            + at;
        let region = &code[at..end];
        // Call forms, never bare names (CLAUDE.md §9): this test's own prose names both symbols.
        let gate_at = region
            .find("redeem_dial_ready(")
            .expect("the poll must consult the re-dial gate");
        let body_at = region
            .find("redeem_manifest_ticket_inner(")
            .expect("the poll must call the production redeem body");
        assert!(
            gate_at < body_at,
            "the gate must run BEFORE the redeem body, or a backed-off ask still dials"
        );
        assert!(
            region.contains("note_failed_dial("),
            "a failed redeem must be recorded, or the gate has nothing to read next poll"
        );
    }

    /// QURATOR-197 — the redemption poll's inbox filter must carry BOTH bounds the chat inbox's
    /// `dm_inbox_filter` has: an explicit `.limit` (CWE-400 — otherwise the relay's own default
    /// decides how much the 300 s poll re-decrypts) and a `since` window (otherwise it re-fetches
    /// all-time history every poll).
    ///
    /// MUTATION (P-10) — remove `.limit(DM_INBOX_FETCH_LIMIT)` from `ticket_inbox_filter` and the
    /// first two asserts red; remove the `since` arm and the third reds.
    #[test]
    fn ticket_inbox_filter_declares_budget_and_window() {
        let me = hb_core::Identity::generate();
        let cold = ticket_inbox_filter(me.public_key(), 0);
        assert_eq!(
            cold.limit,
            Some(DM_INBOX_FETCH_LIMIT),
            "a budget, not the relay's default"
        );
        let warm = ticket_inbox_filter(me.public_key(), 1_700_000_000);
        assert_eq!(warm.limit, Some(DM_INBOX_FETCH_LIMIT), "the bounded filter too");
        assert_eq!(
            warm.since.map(|s| s.as_secs()),
            Some(1_700_000_000),
            "a warm anchor bounds the fetch window"
        );
        assert!(cold.since.is_none(), "a zero anchor is the cold, full initial pull");
    }

    /// QURATOR-197 — the window anchors to the OLDEST still-pending ask minus the shared 48 h
    /// NIP-59 wobble allowance: a redeemable wrap answers an ask, so it cannot predate it, and
    /// the margin absorbs relay-side `since` wobble plus a peer clock sitting behind ours.
    /// `2026-09-01T00:00:00Z` = 1_788_220_800 unix secs.
    ///
    /// MUTATION (P-10) — change `.min()` to `.max()` in `ticket_inbox_since` (newest ask, not
    /// oldest) and the first assert reds; drop the `DM_FETCH_MARGIN_SECS` subtraction and it reds
    /// too.
    #[test]
    fn ticket_inbox_since_anchors_to_the_oldest_pending_ask() {
        let now = 1_800_000_000u64;
        let mut asks = HashMap::new();
        asks.insert("a|a|films".to_string(), ask("2026-09-01T00:00:00Z"));
        asks.insert("b|b|music".to_string(), ask("2026-09-10T12:00:00Z"));
        assert_eq!(
            ticket_inbox_since(&asks, now),
            1_788_220_800 - DM_FETCH_MARGIN_SECS,
            "oldest ask minus the wobble allowance"
        );
        assert_eq!(
            ticket_inbox_since(&HashMap::new(), now),
            0,
            "no asks -> no window (the loop does not fetch at all)"
        );
    }

    /// QURATOR-197 — future-poison and fail-open discipline for the window anchor: a future
    /// `sent_at` (hostile or merely skewed) clamps to `now` so `since` can never land in the
    /// future and silently stop delivery; an unparseable trace maps to 0, forcing the whole
    /// window open rather than letting the parseable remainder silently narrow past it.
    ///
    /// MUTATION (P-10) — remove `.min(now)` in `ticket_inbox_since` and the first assert reds;
    /// change the inner `.unwrap_or(0)` to `.unwrap_or(now)` and the second reds.
    #[test]
    fn ticket_inbox_since_clamps_future_and_fails_open() {
        let now = 1_800_000_000u64; // ≈ 2027-01-15, comfortably after both stamps below
        let mut asks = HashMap::new();
        asks.insert("a|a|films".to_string(), ask("2027-06-01T00:00:00Z"));
        assert_eq!(
            ticket_inbox_since(&asks, now),
            now,
            "a future stamp clamps down to now, never past it"
        );
        asks.insert("b|b|music".to_string(), ask("not-a-timestamp"));
        assert_eq!(
            ticket_inbox_since(&asks, now),
            0,
            "one unparseable trace opens the whole window (0 = no since filter)"
        );
    }

    /// The driver must ask the AUTHOR authorlessly and a CARRIER author-bearingly.
    ///
    /// `approval_body_for` routes on `author_npub` alone — Some to the cache re-serve, None to the
    /// full-list build — and an author never caches its own publish. Send the author an
    /// author-bearing ask and it looks for a cached copy of its own collection and misses, so the
    /// author leg of every wave fails while the carrier legs still work: an intermittent,
    /// source-dependent fetch failure rather than a loud one.
    ///
    /// MUTATION (P-10) — in `poll_once`, replace the `if target == stale.author_npub` discriminator
    /// with `if false` → this test reds (every ask becomes author-bearing).
    #[test]
    fn the_ask_shape_matches_whether_the_target_is_the_author() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code.find("pub(crate) async fn poll_once(").expect("poll_once must exist");
        let end = code[at..].find("fn truncate(").expect("truncate must follow") + at;
        let region = &code[at..end];
        assert!(
            region.contains("if target == stale.author_npub"),
            "the driver must discriminate on whether the target IS the author"
        );
        // Call forms, not bare names — this test's own prose names both symbols (CLAUDE.md §9).
        // `request_manifest_from_inner(` does not contain `request_manifest_inner(`, so these count
        // the two bodies separately.
        assert_eq!(
            region.matches("request_manifest_inner(").count(),
            1,
            "the authorless ask must be sent, exactly once, for the author target"
        );
        assert_eq!(
            region.matches("request_manifest_from_inner(").count(),
            1,
            "the author-bearing ask must be sent, exactly once, for a carrier target"
        );
    }

    /// MUTATION (P-10) — in `asked_peers`, change `.split('|').next()` to `.split('|').last()`
    /// → this test reds (the SLUG would become the allow-list entry, so every real peer would be
    /// filtered out and no ticket could ever be redeemed).
    #[test]
    fn asked_peers_takes_the_peer_segment_not_the_slug() {
        let mut asks: HashMap<String, ()> = HashMap::new();
        asks.insert("npubC|npubA|films".to_string(), ());
        asks.insert("npubC|npubA|music".to_string(), ());
        asks.insert("npubE|npubA|films".to_string(), ());

        let got = asked_peers(&asks);
        assert_eq!(got.len(), 2, "two distinct peers across three asks");
        assert!(got.contains("npubC") && got.contains("npubE"));
        assert!(!got.contains("films"), "the slug is not a peer");
        assert!(
            !got.contains("npubZ"),
            "a peer we never asked is absent — this set IS the gate on whose ticket may be redeemed"
        );
    }

    /// The allow-list must actually reach `decode_dms` — the boundary, not just a computed set.
    ///
    /// A ticket names a node address this process will DIAL. Redeeming an unsolicited one would let
    /// any stranger choose that address, which is the SSRF shape `redeem_manifest_ticket_with_progress`
    /// guards at QURATOR-113 #20; this allow-list is the layer before it. Passing `None` here would
    /// admit every gift-wrap the relay returns.
    ///
    /// MUTATION (P-10) — in `redeem_pending_tickets`, change `Some(&allow)` in the `decode_dms`
    /// call to `None` → this test reds.
    #[test]
    fn the_redeem_allow_list_is_wired_into_decode_dms() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find("async fn redeem_pending_tickets(")
            .expect("the redeem helper must exist");
        // Anchored on a SIGNATURE, not a doc comment: the strip above removes every `///` line,
        // so a doc-comment anchor can never be found in `code` (this guard failed exactly that way
        // when first written).
        let end = code[at..]
            .find("pub(crate) async fn run_fetch_driver_loop(")
            .expect("the loop must follow the redeem helper")
            + at;
        let region = &code[at..end];
        assert!(
            region.contains("decode_dms(own_npub, identity, wraps, Some(&allow))"),
            "the decoded set must be restricted to peers we asked; None would admit any stranger's \
             ticket and let them choose an address this node dials"
        );
    }

    /// `poll_once` must redeem BEFORE it checks staleness.
    ///
    /// A ticket that arrived since the last poll has to land in the cache first, or the staleness
    /// check still sees the old fingerprint and re-asks for a collection this node was just handed —
    /// burning a wave slot, a throttle slot, and one of the three attempts, every poll.
    ///
    /// MUTATION (P-10) — in `poll_once`, replace the
    /// `outcome.redeemed = redeem_pending_tickets(...).await;` statement with
    /// `outcome.redeemed = Vec::new();` → this test reds.
    #[test]
    fn poll_once_redeems_before_it_checks_staleness() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code.find("pub(crate) async fn poll_once(").expect("poll_once must exist");
        let end = code[at..].find("fn truncate(").expect("truncate must follow") + at;
        let region = &code[at..end];
        // Call forms, never bare names: this test's own prose names both symbols, and a guard that
        // counts an identifier it also writes can be satisfied by its own message (CLAUDE.md §9).
        let redeem_at = region
            .find("redeem_pending_tickets(")
            .expect("poll_once must drain tickets");
        let check_at = region
            .find("manifest_cache::list(")
            .expect("poll_once must read what is held");
        assert!(
            redeem_at < check_at,
            "redeem must precede the staleness read, or the driver re-asks for what it just received"
        );
    }

    /// The loop must stay a THIN SHELL over [`poll_once`] — cadence and state only.
    ///
    /// This is what makes the WAN row meaningful. The harness drives `poll_once` directly, so if
    /// the loop ever grew its own copy of the poll, the suite would be exercising a path production
    /// no longer takes — a phantom GREEN of exactly the shape this project has hit three times
    /// (`sanitize_node_addr`, `approve_request`, D4's teaser stamp).
    ///
    /// MUTATION (P-10) — in `run_fetch_driver_loop`, replace the `poll_once(...)` call with an
    /// inlined `crate::manifest_cache::list(&store.manifest_cache_dir());` → this test reds on both
    /// asserts.
    #[test]
    fn the_loop_delegates_to_poll_once_and_holds_no_copy_of_it() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find("pub(crate) async fn run_fetch_driver_loop(")
            .expect("the loop must exist");
        let end = code[at..].find("pub(crate) async fn poll_once(").expect("poll_once must follow") + at;
        let loop_body = &code[at..end];
        // Count the CALL FORM, never the bare name: this test's own strings mention the symbol, and
        // a guard that counts an identifier it also writes can be satisfied by itself (CLAUDE.md §9).
        assert_eq!(
            loop_body.matches("poll_once(").count(),
            1,
            "the loop must delegate to poll_once exactly once"
        );
        assert!(
            !loop_body.contains("manifest_cache::list("),
            "the loop must not read the cache itself — that is poll_once's job, and a second copy \
             is how the harness ends up driving a path production has left behind"
        );
    }

    /// MUTATION (P-10) — in `stale_holdings`, invert the comparison in the `.then(...)` guard to
    /// `(current == &k.fingerprint)` → this test reds (an unchanged holding would be refetched and
    /// a changed one ignored).
    #[test]
    fn a_changed_fingerprint_is_stale_and_an_unchanged_one_is_not() {
        let h = vec![held("npubA", "films", "fp-old"), held("npubA", "music", "fp-same")];
        let published = vec![
            ("films".to_string(), Some("fp-new".to_string())),
            ("music".to_string(), Some("fp-same".to_string())),
        ];
        assert_eq!(
            stale_holdings(&h, &published),
            vec![StaleHolding {
                author_npub: "npubA".into(),
                slug: "films".into(),
                want_fingerprint: "fp-new".into(),
            }],
            "only the collection whose snapshot moved on is refetched"
        );
    }

    /// MUTATION (P-10) — in `stale_holdings`, replace the `let current = current.as_ref()?;` line
    /// with `let current = current.as_ref().map(|s| s.as_str()).unwrap_or("").to_string(); let
    /// current = &current;` → an absent fingerprint compares unequal to anything and the holding
    /// reds as stale.
    ///
    /// This is property 2, and it is the difference between a quiet driver and one that re-asks a
    /// peer forever over a listing that can never satisfy it.
    #[test]
    fn a_listing_without_a_fingerprint_is_not_treated_as_a_change() {
        let h = vec![held("npubA", "films", "fp-old")];
        let published = vec![("films".to_string(), None)];
        assert!(stale_holdings(&h, &published).is_empty(), "unknown is not stale");
    }

    /// MUTATION (P-10) — in `stale_holdings`, change the slug lookup `.find(|(slug, _)| *slug ==
    /// k.slug)?` to `.next()?` → this test reds (a held slug would be compared against whichever
    /// collection happened to come first, and refetched under the wrong name).
    ///
    /// ⚠ NOT `.first()?` — that is a slice method and `published.iter()` is an iterator, so it does
    /// not compile, and a mutation that does not compile is NON-EVIDENCE rather than a proof.
    #[test]
    fn a_slug_the_author_no_longer_publishes_is_left_alone() {
        let h = vec![held("npubA", "retired", "fp-old")];
        let published = vec![("films".to_string(), Some("fp-new".to_string()))];
        assert!(stale_holdings(&h, &published).is_empty(), "nothing to compare against");
    }

    /// MUTATION (P-10) — in `candidates_for`, drop the `&& n.as_str() != me` clause of the filter
    /// → this test reds (the node would ask itself for what it already holds, burning a wave slot
    /// and a throttle slot on a guaranteed no-op).
    #[test]
    fn candidates_exclude_the_author_and_this_node() {
        let contacts = vec!["npubA".to_string(), "npubC".to_string(), "npubME".to_string()];
        let got = candidates_for(&contacts, "npubA", "npubME");
        assert_eq!(
            got.iter().map(|c| c.npub.as_str()).collect::<Vec<_>>(),
            ["npubC"],
            "the author is the fallback, not a carrier; and we never ask ourselves"
        );
    }

    /// MUTATION (P-10) — in `note_attempt`, delete the `c.attempts += 1;` line → this test reds
    /// (no peer would ever reach the 3-try cap, so the wave could never move on or fall back to
    /// the author).
    #[test]
    fn noting_an_attempt_advances_that_peer_only() {
        let now = Instant::now();
        let mut state = AskState {
            peers: vec![Candidate::fresh("npubC"), Candidate::fresh("npubD")],
            author: Candidate::fresh("npubA"),
        };
        note_attempt(&mut state, "npubC", now);
        assert_eq!(state.peers[0].attempts, 1, "the asked peer advances");
        assert_eq!(state.peers[1].attempts, 0, "its sibling does not");
        assert_eq!(state.author.attempts, 0, "and neither does the author");
    }

    /// MUTATION (P-10) — in `note_attempt`, change the author branch condition to
    /// `state.author.npub != npub` → this test reds (an author fallback attempt would be recorded
    /// against a carrier, or nowhere, so the author could never exhaust and `GiveUp` would be
    /// unreachable).
    #[test]
    fn an_author_fallback_attempt_is_recorded_against_the_author() {
        let now = Instant::now();
        let mut state = AskState {
            peers: vec![Candidate::fresh("npubC")],
            author: Candidate::fresh("npubA"),
        };
        note_attempt(&mut state, "npubA", now);
        assert_eq!(state.author.attempts, 1);
        assert_eq!(state.peers[0].attempts, 0);
    }

    /// MUTATION (P-10) — in the `POLL_INTERVAL` const initializer, change
    /// `Duration::from_secs(300)` to `Duration::from_secs(1)` → this test reds. A one-second
    /// fingerprint poll would resolve every held author's listing every second, which is the relay
    /// citizenship problem the ask throttle exists to avoid, arriving by a different door.
    #[test]
    fn the_poll_interval_is_minutes_not_seconds() {
        assert!(
            POLL_INTERVAL >= Duration::from_secs(60),
            "a fingerprint changes when a human edits a collection; polling faster only costs relays"
        );
    }

    /// MUTATION (P-10) — in `truncate`, change `.take(12)` to `.take(200)` → this test reds. A full
    /// npub in a log line identifies a person, and this loop logs every peer it asks.
    #[test]
    fn logged_npubs_are_truncated() {
        let full = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        assert_eq!(truncate(full).len(), 12);
        assert!(truncate(full).len() < full.len());
    }

    /// QURATOR-189 — the discovery tier must actually READ the switch: `swarm_caching` sat inert
    /// (defined, defaulted, tested in `store.rs`, read by nobody) until this ticket.
    ///
    /// MUTATION (P-10) — in `poll_once`, replace the
    /// `let swarm_caching = store.load_settings().ok().flatten().unwrap_or_default().swarm_caching;`
    /// line with `let swarm_caching = true;` → this test reds (the region no longer reads settings
    /// at all, so the tier would run for users who never opted in).
    #[test]
    fn discovery_is_gated_on_the_swarm_caching_flag() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code.find("pub(crate) async fn poll_once(").expect("poll_once must exist");
        let end = code[at..].find("fn truncate(").expect("truncate must follow") + at;
        let region = &code[at..end];
        // Call forms, never bare names (CLAUDE.md §9): the test's own prose names the flag, so
        // every assert below is on the poll_once region, not on this test's text.
        assert!(
            region.contains("load_settings("),
            "the poll must read the settings file, or swarm_caching stays the inert flag it was"
        );
        assert!(
            region.contains(".swarm_caching") && region.contains("if swarm_caching {"),
            "the flag read must gate the discovery tier, not merely be loaded"
        );
        assert!(
            region.contains("discover_unheld("),
            "the gate must actually reach the discovery tier's call"
        );
    }

    /// QURATOR-189 — a discovery ask goes TO the author, so it must use the AUTHORLESS body.
    ///
    /// Same rule as `the_ask_shape_matches_whether_the_target_is_the_author`: `approval_body_for`
    /// routes on `author_npub` alone — Some to the cache re-serve, None to the full-list build —
    /// and an author never caches its own publish. An author-bearing ask sent TO the author fails
    /// every discovery fetch silently, which is exactly the failure this ticket must not ship.
    ///
    /// MUTATION (P-10) — in `discover_unheld`, replace the whole `request_manifest_inner(...)`
    /// `.await` expression assigned to `sent` with `let sent: Result<(), String> = Ok(());` →
    /// this test reds (the ask would never be sent — the inert-flag failure shape again).
    #[test]
    fn discovery_asks_the_author_authorlessly() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find("async fn discover_unheld(")
            .expect("the discovery helper must exist");
        let end = code[at..]
            .find("pub(crate) async fn run_fetch_driver_loop(")
            .expect("the loop must follow the discovery helper")
            + at;
        let region = &code[at..end];
        // Call forms, not bare names (CLAUDE.md §9): `request_manifest_from_inner(` does not
        // contain `request_manifest_inner(`, so the two bodies are counted separately.
        assert_eq!(
            region.matches("request_manifest_inner(").count(),
            1,
            "the discovery ask must use the authorless body, exactly once"
        );
        assert!(
            !region.contains("request_manifest_from_inner("),
            "an author-bearing ask sent TO the author looks for a cached copy of the author's \
             own collection and misses, silently"
        );
    }

    /// QURATOR-189 — a fresh node holds nothing, and it is exactly the node that needs discovery.
    /// The old `held.is_empty()` early return must now spare the discovery tier.
    ///
    /// MUTATION (P-10) — in `poll_once`, replace `if held.is_empty() && !swarm_caching {` with
    /// `if held.is_empty() {` → this test reds (a fresh node would return before the discovery
    /// call, making the tier unreachable exactly where it matters most).
    #[test]
    fn discovery_is_not_stranded_behind_the_empty_holdings_return() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code.find("pub(crate) async fn poll_once(").expect("poll_once must exist");
        let end = code[at..].find("fn truncate(").expect("truncate must follow") + at;
        let region = &code[at..end];
        assert!(
            region.contains("if held.is_empty() && !swarm_caching {"),
            "the early return must be conditioned on the flag, or a fresh node — the one with \
             nothing to refresh and everything to discover — never reaches the tier"
        );
        let guard_at = region.find("held.is_empty()").expect("the guard must exist");
        let discover_at = region.find("discover_unheld(").expect("the discovery call must exist");
        assert!(
            guard_at < discover_at,
            "the discovery call must live past the holdings guard, not before it"
        );
    }

    /// QURATOR-250 — the ask-trace retention sweep must run inside the poll, AHEAD of the redeem
    /// drain that reads the same map (an expired entry must not authorise a dial or hold
    /// `ticket_inbox_since`'s window open in the tick it dies) and ahead of the
    /// holdings/discovery early returns (a node that holds nothing with discovery off must still
    /// stop carrying dead entries). Before this ticket nothing ever removed an entry, so one
    /// malicious listing minted `peer|author|slug` rows that lived forever.
    ///
    /// MUTATION (P-10) — in `poll_once`, delete the whole
    /// `match store.evict_expired_manifest_asks(...)` statement at fetch_driver.rs:526 → this
    /// test reds (the sweep never runs and the persisted set grows unbounded).
    #[test]
    fn stale_asks_are_expired_inside_the_poll_before_the_redeem_drain() {
        let src = include_str!("fetch_driver.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code.find("pub(crate) async fn poll_once(").expect("poll_once must exist");
        let end = code[at..].find("fn truncate(").expect("truncate must follow") + at;
        let region = &code[at..end];
        // Call forms, never bare names (CLAUDE.md §9): this test's own prose names the sweep.
        let evict_at = region
            .find("evict_expired_manifest_asks(")
            .expect("the retention sweep must be called from the poll");
        let drain_at = region
            .find("redeem_pending_tickets(")
            .expect("the redeem drain must exist");
        assert!(
            evict_at < drain_at,
            "eviction must precede the redeem drain, or a dead entry authorises one last dial"
        );
        let early_at = region
            .find("held.is_empty()")
            .expect("the holdings early return must exist");
        assert!(
            evict_at < early_at,
            "eviction must precede the early returns, or an idle node carries dead entries forever"
        );
    }

    /// MUTATION (P-10) — in `unheld_collections`, change the held-membership test inside the
    /// `.filter(...)` from `!held.iter().any(|k| k.npub == author && k.slug == *slug)` to
    /// `!held.iter().any(|k| k.slug == *slug)` → this test reds (one author's holding would mask
    /// another author's same-named slug, so discovery would skip a collection never held).
    #[test]
    fn unheld_collections_is_published_minus_held_per_author() {
        let h = vec![held("npubA", "films", "fp-old")];
        let published = vec![
            ("films".to_string(), Some("fp-new".to_string())),
            ("music".to_string(), Some("fp-2".to_string())),
            ("retro".to_string(), None),
        ];
        assert_eq!(
            unheld_collections(&h, "npubA", &published),
            vec![("music".to_string(), "fp-2".to_string())],
            "the held slug does not qualify even though its fingerprint differs; a listing \
             without a fingerprint cannot name the snapshot the ask must match"
        );
        // A different author's same-named slug is a DIFFERENT collection: never held, still
        // discoverable. Slugs are per-author namespaces.
        let other = vec![("films".to_string(), Some("fp-other".to_string()))];
        assert_eq!(
            unheld_collections(&h, "npubB", &other),
            vec![("films".to_string(), "fp-other".to_string())],
            "holding npubA/films must not mask npubB/films"
        );
    }
}
