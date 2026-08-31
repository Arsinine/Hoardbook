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

use std::collections::{HashSet, VecDeque};

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
pub(crate) fn parse_recipient(s: &str) -> Result<PublicKey, String> {
    hb_core::ShareCode::parse(s)
        .map(|sc| sc.pubkey())
        .map_err(|e| format!("Invalid recipient: {e}"))
}

/// devtest #14: a self-send is never valid — `send_message` rejects it before any network I/O.
/// `route_dm`'s `from == own_npub` inbox routing stays; it exists for the legitimate sent-echo
/// of a message you already sent, not to allow creating a self-conversation from scratch.
pub(crate) fn is_self_send(recipient: &PublicKey, me: &PublicKey) -> bool {
    recipient == me
}

pub(crate) fn npub_of(pk: &PublicKey) -> String {
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
    // QURATOR-66: DM send lifecycle — enqueued/sealed/published. The recipient npub is TRUNCATED
    // (INV-2: no full npub in a log the user pastes). The plaintext is NEVER logged — only the
    // content length, which is enough to distinguish an empty/oversize failure from a real one.
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&npub_of(recipient)),
        content_len = content.len(),
        "dm: building NIP-17 gift wrap"
    );
    let wrap = build_dm(identity, recipient, content).await?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&npub_of(recipient)),
        "dm: sealed — resolving recipient read-relays"
    );
    let targets = dm_delivery_targets(
        own_relays,
        hb_net::resolve_recipient_relays(client, recipient, own_relays, own_relays, timeout).await,
    );
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&npub_of(recipient)),
        target_relays = targets.len(),
        "dm: publishing gift wrap to recipient inbox ∪ own relays"
    );
    client.ensure_relays(&targets, timeout).await?;
    client.publish_to(&wrap, &targets).await?;
    tracing::info!(
        recipient = %crate::logging::trunc_npub(&npub_of(recipient)),
        "dm: delivered"
    );
    Ok(wrap)
}

/// SSRF-filter the DM delivery targets (QURATOR-113 #21). `resolve_recipient_relays` merges the
/// recipient's **peer-authored** NIP-65 read-relays with our own/seed; the peer-authored portion is
/// attacker-controlled whenever the recipient is a stranger. Keep the caller's own relays (trusted —
/// validated on save) unconditionally, and keep any other target only when it is a public `ws`/`wss`
/// relay — the same `validate_relay_url` check `browse::big_relay_fetch_order` applies to the peer's
/// big-relay URL. Address-class filtering, not scheme filtering: the plain-`ws://` VPS relays on
/// public IPs keep working. Pure — unit-tested.
fn dm_delivery_targets(own_relays: &[String], targets: Vec<String>) -> Vec<String> {
    targets
        .into_iter()
        .filter(|t| own_relays.iter().any(|o| o == t) || net::validate_relay_url(t).is_ok())
        .collect()
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
            Err(e) => tracing::debug!(
                wrap_id = %wrap.id.to_hex(),
                "dm inbox: skipping undecryptable/foreign gift wrap: {e}"
            ),
        }
    }
    out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    out
}

// ---------------------------------------------------------------------------
// DM_CACHE_LOCK (finding M2) — serializes every load→mutate→save transaction against
// `dm_cache.json`. Three sites read-modify-write that one file: the sent-echo persist
// (`persist_sent_dm`), the inbox poll's merge (`get_messages`), and the Request-accept history
// migration (`dm_request_accept_inner`). Each used to load → mutate → save independently, so a 3 s
// poll overlapping a send (or an accept) could last-write-wins away one side's update.
//
// A SINGLE static, hoisted to MODULE scope — not one declared inside each function. A lock declared
// per-function is a *different* mutex at every site, so it only serializes repeat calls to that one
// function against itself; it does nothing to stop two DIFFERENT functions' transactions from
// interleaving, which is exactly the race this exists to close (the documented per-function-statics
// trap). `tokio::sync::Mutex`, not `std` (house rule): the inbox-merge transaction awaits
// `unwrap_dm` while the lock is held (crypto, not relay I/O), so the guard must survive an `.await`.
//
// Kept tight: relay I/O (the send itself, the inbox `client.fetch`) always happens OUTSIDE the lock;
// only the disk load/mutate/save is guarded.
static DM_CACHE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// DM_REQUESTS_LOCK — serializes every load→mutate→save transaction against `dm_requests.json`
// (the Request-inbox bucket file), the sibling of DM_CACHE_LOCK above and hit by the same race:
// `dm_request_accept_inner`, `dm_request_decline_inner`, `dm_block_inner`, and the inbox poll's
// merge in `get_messages` each load the full bucket list, mutate their one bucket, and save the
// whole list back. Proven live by the `dm_cache_lock_survives_concurrent_persist_and_accept_hammering`
// hammer test: DM_CACHE_LOCK only ever covered `dm_cache.json`, so 25 concurrent accepts raced
// unguarded on this file and last-write-wins stranded buckets that should have drained.
//
// `std::sync::Mutex`, not `tokio` — none of the four guarded spans hold the lock across an
// `.await` (decline/block are plain sync fns; accept's bucket span completes before its own
// `DM_CACHE_LOCK` section starts), so a std mutex is correct and matches the house pattern used
// for the same shape of race in `store.rs` (`READ_STATE_LOCK`, `ANNOUNCE_SEEN_LOCK`).
static DM_REQUESTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---------------------------------------------------------------------------
// DM_DECLINED_LOCK / DM_BLOCKED_LOCK (finding #33) — the same load→mutate→save serialization as
// DM_REQUESTS_LOCK above, for the two sibling files that were left unguarded: `dm_declined.json`
// (`dm_request_accept_inner`/`dm_block_inner` remove an entry; `dm_request_decline_inner` adds one)
// and `dm_blocked.json` (`dm_block_inner` adds; `dm_unblock_inner` removes). Two concurrent
// block/decline clicks raced here exactly as accept/decline/block/inbox-merge did on
// dm_requests.json before DM_REQUESTS_LOCK — last-write-wins silently dropped one entry.
//
// `std::sync::Mutex`, matching DM_REQUESTS_LOCK: none of the guarded spans hold the lock across an
// `.await` (decline/block/unblock are sync fns; accept's declined span completes before its own
// `DM_CACHE_LOCK` section starts). **`dm_block_inner` and `dm_request_decline_inner` hold BOTH locks
// at once, in the single consistent order DM_DECLINED_LOCK before DM_BLOCKED_LOCK** (decline-before-
// block, the order `dm_block_inner` already writes the two files) — no site ever takes them in the
// opposite order, so there is no lock-ordering cycle and no deadlock (finding B). The DM_REQUESTS_LOCK
// scope always closes before either relationship lock is taken.
static DM_DECLINED_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static DM_BLOCKED_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Fetch budget for the inbox poll. Without an explicit `.limit()` the client leaves the response
/// size to the relay's own default (strfry's `maxFilterLimit`) — a hostile or misconfigured relay
/// could return an unbounded batch (CWE-400). 1000 is far above realistic DM volume in a 48 h window
/// and matches the other fetch-budget constants (`TEASER_SEARCH_FETCH_LIMIT`,
/// `TOPIC_DISCOVERY_FETCH_LIMIT`).
const DM_INBOX_FETCH_LIMIT: usize = 1000;

/// Cap on remembered failed-unwrap wrap ids (the negative cache). A wrap that fails to unwrap is
/// remembered here so the ~15 s poll doesn't re-run the full unwrap (schnorr + ECDH + AES-GCM) on it
/// for up to 48 h — one cheap signed event would otherwise buy ~57,600 redundant decryptions. Bounded
/// and FIFO-evicted because it is fed by attacker-controlled ids; 4,096 ids ≈ 256 KiB, and a flood
/// beyond that is bounded by the 48 h fetch window + relay bandwidth rather than our CPU. In-memory
/// only (not persisted, not part of [`DmCache`]): it is a CPU-DoS backstop, not a correctness
/// boundary — losing it across a restart merely re-attempts a wrap once, never loses a message.
const MAX_FAILED_WRAPS: usize = 4_096;

/// Bounded, FIFO negative cache of gift-wrap ids that failed to unwrap (`set` for O(1) membership,
/// `order` for oldest-first eviction).
struct FailedWrapCache {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl FailedWrapCache {
    fn new() -> Self {
        FailedWrapCache { order: VecDeque::new(), set: HashSet::new() }
    }
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
    fn insert(&mut self, id: String) {
        if self.set.insert(id.clone()) {
            self.order.push_back(id);
            if self.order.len() > MAX_FAILED_WRAPS {
                if let Some(evicted) = self.order.pop_front() {
                    self.set.remove(&evicted);
                }
            }
        }
    }
}

/// The failed-unwrap negative cache. Module-scope static (house rule: never a per-function static).
/// `std::sync::Mutex`, not `tokio` — the check and the record are separate synchronous spans, so the
/// lock is never held across the `unwrap_dm` `.await`. `LazyLock` because `HashSet::new()` is not
/// `const`, so a plain `static … = Mutex::new(…)` initializer won't compile.
///
/// Keyed by **(recipient identity npub, wrap id)**, not wrap id alone. `unwrap_dm` is deterministic
/// over `(identity keys, wrap)` — a wrap that fails under one identity may decode under another (a
/// wrap addressed to identity B that a relay fed while A was active, or after a wipe/restore in
/// `commands/identity.rs`). A wrap-id-only key would let A's failure poison the global cache and skip
/// B's genuinely valid message — silent INV-8-adjacent message loss. Pinned by
/// `negative_cache_does_not_cross_identity_boundaries`.
static FAILED_WRAPS: std::sync::LazyLock<std::sync::Mutex<FailedWrapCache>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(FailedWrapCache::new()));

/// Compose the negative-cache key: (identity npub, wrap id). `\x00` appears in neither bech32 (npub)
/// nor hex (wrap id), so the join is collision-free.
fn failed_wrap_key(identity_npub: &str, wrap_id: &str) -> String {
    format!("{identity_npub}\u{0}{wrap_id}")
}

/// O(1) "already failed before for THIS identity?" lookup — checked before `unwrap_dm` so a
/// previously-failed wrap is not re-unwrapped on every poll.
fn failed_wrap_seen(identity_npub: &str, id: &str) -> bool {
    FAILED_WRAPS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&failed_wrap_key(identity_npub, id))
}

/// Record a (identity, wrap id) that failed to unwrap (bounded, FIFO-evicted).
fn record_failed_wrap(identity_npub: &str, id: String) {
    FAILED_WRAPS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(failed_wrap_key(identity_npub, &id));
}

/// The inbox fetch filter: kind-1059 wraps addressed to us, bounded by `since = cursor − margin` once
/// the cache has a cursor (an incremental read — most polls then return ~nothing), or unbounded on a
/// cold cache (the one full initial pull). `since` is **bandwidth-only** — dedup + security are by
/// wrap id and the persisted block/decline sets, never this attacker-fuzzable timestamp.
///
/// `pub(crate)` so the `hb-wan-it` WAN-C suite can drive the exact cursor/fetch path `get_messages`
/// uses (the future-poison clamp + restart round-trip is a WAN-C row). No full `pub` — this is an
/// internal seam, not a stable API.
pub(crate) fn dm_inbox_filter(me: PublicKey, newest_seen_outer: u64) -> Filter {
    let f = Filter::new().kind(Kind::GiftWrap).pubkey(me).limit(DM_INBOX_FETCH_LIMIT);
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
pub(crate) async fn merge_wraps_into_cache(
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
        if failed_wrap_seen(own_npub, &wrap_id) {
            continue; // already failed to unwrap under THIS identity on a prior poll — skip the redundant re-decrypt
        }
        match unwrap_dm(identity, &wrap).await {
            Ok(dm) => {
                // Record the id only after a successful unwrap — an undecodable/foreign wrap is never
                // remembered in the success ledger (it may be re-tried next poll, bounded by the
                // 48 h window); instead it lands in the bounded negative cache, so the poll does not
                // re-run the unwrap on it.
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
            Err(e) => {
                // audit #11: remember the failed id so the ~15 s poll doesn't re-run the full unwrap
                // on it for up to 48 h. Bounded (attacker-controlled ids), so it can't become a
                // memory DoS; unwrap is deterministic for a given identity+wrap, so a failure here is
                // permanent for this identity and the cache never blacklists a message that could
                // later succeed.
                record_failed_wrap(own_npub, wrap_id.clone());
                tracing::debug!(
                    wrap_id = %wrap.id.to_hex(),
                    "dm inbox: skipping undecryptable/foreign gift wrap: {e}"
                );
            }
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
/// `pub(crate)` so the `hb-wan-it` WAN-C suite can read the post-merge inbox (the same view
/// `get_messages` returns) without duplicating the reclassify-under-current-sets logic.
pub(crate) fn cached_inbox(
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

pub(crate) fn dm_requests_inner(store: &DataStore, identity: &hb_core::Identity, own_npub: &str) -> Result<Vec<DmRequestView>, String> {
    let buckets = store.load_dm_requests(identity).map_err(cmd_err)?;
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
pub(crate) async fn dm_request_accept_inner(
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
        listings_state: Default::default(), // QURATOR-134 tri-state (not classified on this stub path)
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None, // W5.2: stamped by the online poll only
        local_tags: vec![],
        fingerprint,
    };
    store.save_contact(&hash, &peer).map_err(cmd_err)?;

    // DM_REQUESTS_LOCK: this load→mutate→save transaction against `dm_requests.json` must serialize
    // against the other three sites touching the same file (see the lock's doc above), or a
    // concurrent accept/decline/block/inbox-merge can last-write-wins away this bucket's removal.
    let (drained, cache_adds) = {
        let _guard = DM_REQUESTS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut buckets = store.load_dm_requests(identity).map_err(cmd_err)?;
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
        store.save_dm_requests(identity, &buckets).map_err(cmd_err)?;
        (drained, cache_adds)
    };

    {
        // DM_DECLINED_LOCK: un-decline the sender (remove their entry) as one serialized
        // load→mutate→save against `dm_declined.json` — see the lock's doc above.
        let _guard = DM_DECLINED_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let declined: Vec<(String, u64)> =
            store.load_dm_declined().map_err(cmd_err)?.into_iter().filter(|(n, _)| n != &npub).collect();
        store.save_dm_declined(&declined).map_err(cmd_err)?;
    }

    // Migrate the accepted history into the DM cache so it persists past the next poll (see doc
    // above). Through DM_CACHE_LOCK (M2): this is a load→mutate→save transaction against the same
    // file the send-persist and inbox-merge paths write, so it must serialize with them too, or an
    // overlapping poll/send could last-write-wins away this migration (or vice versa).
    if !cache_adds.is_empty() {
        let _guard = DM_CACHE_LOCK.lock().await;
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
pub(crate) fn dm_request_decline_inner(
    store: &DataStore,
    identity: &hb_core::Identity,
    npub: String,
    now: u64,
) -> Result<(), String> {
    {
        // DM_REQUESTS_LOCK: see the lock's doc — serializes against accept/block/inbox-merge on the
        // same `dm_requests.json`.
        let _guard = DM_REQUESTS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut buckets = store.load_dm_requests(identity).map_err(cmd_err)?;
        buckets.retain(|b| b.npub != npub);
        store.save_dm_requests(identity, &buckets).map_err(cmd_err)?;
    }

    {
        // BOTH relationship locks, in the documented DECLINED → BLOCKED order, so the add-decline is
        // atomic against `dm_block_inner`'s clear-decline + add-block (finding B). The blocked set is
        // consulted under the same critical section, and a decline for an already-blocked sender is
        // skipped — blocked supersedes decline, so recording it would be the stale entry a later
        // unblock reveals as a silent decline.
        let _declined = DM_DECLINED_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _blocked = DM_BLOCKED_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if store.load_dm_blocked().map_err(cmd_err)?.iter().any(|n| n == &npub) {
            return Ok(()); // blocked supersedes decline — never record a decline for a blocked sender
        }
        let declined = store.load_dm_declined().map_err(cmd_err)?;
        store.save_dm_declined(&record_declined(declined, npub, now)).map_err(cmd_err)
    }
}

/// Send + persist-the-echo path (QURATOR-91): await the wrap-producing send (`send_dm_inner` in
/// production), then the local-cache write that makes own history survive a restart. This is the ONE
/// function both the production `send_message` command and the Q91 regression test drive, so the
/// test cannot stay green when the persist half is removed (the lookalike-helper trap CLAUDE.md §9
/// names: a test exercising its own copy of the emission). The send half is injected as a future so
/// the offline test drives the SAME persist wiring without a live relay. The persist is best-effort
/// BY DESIGN: a cache-write failure must not fail an already-delivered send, so it logs a warning
/// instead of returning the error.
///
/// Finding M3: the timestamp is captured ONCE, right here, and threaded into both the cache entry
/// (`persist_sent_dm`) and the returned value — the caller (`send_message`) reuses it for the
/// `ReceivedMessage` it hands back to the UI. Before this fix each side called `Utc::now()`
/// independently, with encryption + disk I/O in between; the UI's feed-echo dedup key
/// (`from|sent_at|to|content`) then saw two different `sent_at` strings for the same send and
/// rendered a duplicate bubble once the feed echo arrived on the next poll.
pub(crate) async fn send_dm_and_cache_inner(
    store: &DataStore,
    identity: &hb_core::Identity,
    recipient: &PublicKey,
    content: &str,
    send: impl std::future::Future<Output = Result<Event, hb_net::NetError>>,
) -> Result<(Event, String), hb_net::NetError> {
    let wrap = send.await?;
    let sent_at = Utc::now().to_rfc3339();
    if let Err(e) = persist_sent_dm(store, identity, &wrap, recipient, content, &sent_at).await {
        tracing::warn!("dm: own-send cache persist failed (message was still delivered): {e}");
    }
    Ok((wrap, sent_at))
}

/// QURATOR-91: persist an OWN SENT DM into the at-rest cache after a successful publish. The inbox
/// fetch (`dm_inbox_filter`) is the `#p` addressed-to-us filter, so our own wraps — addressed to the
/// recipient — never enter the feed; without this write, own history vanishes on restart. A pure
/// LOCAL cache write on the existing DM path: no wire format, sealing, or relay change. Deduped by
/// the real gift-wrap id (a retried send that reuses the same wrap can't double-insert), `to` is the
/// PEER (the `ReceivedMessage` contract: "us for inbound, the peer for our sent echo"), and the
/// entry routes to `Inbox` on read (`route_dm` admits `from == own_npub`), so no ledger change is
/// needed beyond the cache's own dedup key.
///
/// `sent_at` is captured ONCE by the caller (`send_dm_and_cache_inner`) and passed in rather than
/// stamped here (finding M3) — see that function's doc for why a second, independently-captured
/// `Utc::now()` produced a duplicate bubble. The load→mutate→save below runs behind `DM_CACHE_LOCK`
/// (finding M2), serialized against the inbox-merge and request-accept transactions on the same file.
pub(crate) async fn persist_sent_dm(
    store: &DataStore,
    identity: &hb_core::Identity,
    wrap: &Event,
    recipient: &PublicKey,
    content: &str,
    sent_at: &str,
) -> Result<(), String> {
    let _guard = DM_CACHE_LOCK.lock().await;
    let mut cache = store.load_dm_cache(identity).map_err(cmd_err)?;
    let wrap_id = wrap.id.to_hex();
    if cache.seen_wraps.iter().any(|id| id == &wrap_id) || cache.messages.iter().any(|m| m.wrap_id == wrap_id) {
        return Ok(()); // already persisted (a retry carrying the same wrap)
    }
    cache.messages.push(CachedDm {
        wrap_id,
        from: identity.npub(),
        to: npub_of(recipient),
        content: content.to_string(),
        sent_at: sent_at.to_string(),
    });
    cache.seen_wraps.push(wrap.id.to_hex());
    cache.prune();
    store.save_dm_cache(identity, &cache).map_err(cmd_err)
}

/// Add `npub` to the local blocklist (spec §Blocked keys — the canonical local blocklist, named for
/// future Settings reuse). Deletes any Request bucket and any decline record — blocked supersedes both.
pub(crate) fn dm_block_inner(store: &DataStore, identity: &hb_core::Identity, npub: String) -> Result<(), String> {
    {
        // DM_REQUESTS_LOCK: see the lock's doc — serializes against accept/decline/inbox-merge on the
        // same `dm_requests.json`.
        let _guard = DM_REQUESTS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut buckets = store.load_dm_requests(identity).map_err(cmd_err)?;
        buckets.retain(|b| b.npub != npub);
        store.save_dm_requests(identity, &buckets).map_err(cmd_err)?;
    }

    {
        // BOTH relationship locks, in the documented DECLINED → BLOCKED order, held together so the
        // clear-decline + add-block transition is atomic against `dm_request_decline_inner`'s
        // add-decline (finding B): a racing decline can no longer slip its decline in between the
        // clear and the add. Clear any decline record (blocked supersedes), then add to the blocklist.
        let _declined = DM_DECLINED_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _blocked = DM_BLOCKED_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let declined: Vec<(String, u64)> =
            store.load_dm_declined().map_err(cmd_err)?.into_iter().filter(|(n, _)| n != &npub).collect();
        store.save_dm_declined(&declined).map_err(cmd_err)?;
        let mut blocked = store.load_dm_blocked().map_err(cmd_err)?;
        if !blocked.contains(&npub) {
            blocked.push(npub);
        }
        store.save_dm_blocked(&blocked).map_err(cmd_err)
    }
}

pub(crate) fn dm_unblock_inner(store: &DataStore, npub: String) -> Result<(), String> {
    // DM_BLOCKED_LOCK: remove the sender from the blocklist as one serialized load→mutate→save — the
    // same lost-update class as `dm_block_inner`'s add, on the same `dm_blocked.json`.
    let _guard = DM_BLOCKED_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
    // M3: `sent_at` comes back from the same seam that stamped the cache entry — one capture, both
    // outputs — so the UI's feed-echo dedup (`from|sent_at|to|content`) can never see two different
    // timestamps for the same send.
    let (_wrap, sent_at) = send_dm_and_cache_inner(
        &store,
        &id_clone,
        &recipient,
        &trimmed,
        send_dm_inner(&client, &id_clone, &recipient, &trimmed, &own, net::RELAY_TIMEOUT),
    )
    .await
    .map_err(cmd_err)?;

    Ok(ReceivedMessage { from, to: npub_of(&recipient), content: trimmed, sent_at })
}

/// Mint an ask nonce: 128 bits of randomness, hex. Unguessable by the peer being asked, which is
/// the whole point — it is the asker's proof that a ticket answers *this* ask.
fn new_ask_nonce() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
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
    /// **The asker's nonce for this ask** (owner ruling ① 2026-07-31). The hoarder echoes it into
    /// the ticket; the asker then auto-dials only for a ticket carrying the nonce it stored. Without
    /// it the redeem-side gate can only ask "did I ever ask this peer for this slug?", which is a
    /// standing authorization to make our client dial on demand.
    ///
    /// `Option` for wire compatibility: an old client omits it, the hoarder echoes `None`, and the
    /// asker refuses to auto-dial — fail closed.
    #[serde(skip_serializing_if = "Option::is_none")]
    ask_nonce: Option<String>,
    /// **VESTIGIAL — kept deliberately, ruled on 2026-07-31 (owner: option (b)).** This carried the
    /// requester's Mascara pubkey so a hoarder knew which Mascara identity to ticket a file to. That
    /// role died with the courier framing (2026-07-26 ruling): M18's transport ticket flows
    /// owner→asker over the manifest plane and needs nothing from the asker's side of the wire.
    ///
    /// Nothing writes it and nothing reads it. It stays because it is part of a shipped,
    /// `wire_freeze`-pinned body and `skip_serializing_if` already keeps it off the wire in practice,
    /// so removing it would buy a `wire_freeze` amendment for an observably identical result.
    /// **Repurposing it was rejected on sight** — the request is asker→owner and the ticket is
    /// owner→asker, so the direction is wrong, and overloading one field across two meanings is
    /// exactly what `wire_freeze` exists to prevent.
    #[serde(skip_serializing_if = "Option::is_none")]
    mascara_pubkey: Option<String>,
    /// **Carrier 4 (QURATOR-79) — the author of the collection being asked for**, when it is not
    /// the asked peer's own: peer D asks peer C for a manifest peer A authored, and C re-serves it
    /// from its cache. `None` means "the asked peer's own collection" — today's semantics exactly.
    ///
    /// Additive and optional, no wire-discriminant change (owner ruling, QURATOR-79, 2026-08-30 —
    /// the same shape as the ticket-side `author_npub` in `hb-core::ticket`). The exemption's
    /// load-bearing condition holds: absence cannot be exploited as a downgrade, because `None`
    /// is the ordinary own-collection ask the asked peer is entitled to serve — there is no weaker
    /// behaviour hiding behind `None`, so unlike `ask_nonce` there is no fail-closed gate to write.
    /// No `#[serde(default)]`: it is a no-op on an `Option` (a missing key already reads as `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    author_npub: Option<String>,
}

/// Build the manifest-request DM body (canonical JSON). Pure — unit-tested without a relay.
///
/// `pub(crate)` so the WAN-E2E harness (`crate::wan_it::suite_wan_e2e`) can build the EXACT production
/// wire format when it drives the "Ask owner" leg over a real link. The harness must not reimplement
/// the wire body (drift risk against a `wire_freeze`-pinned contract); this is the single source.
pub(crate) fn build_manifest_request(
    slug: &str,
    fingerprint_seen: &str,
    teaser_event_id: Option<String>,
    mascara_pubkey: Option<String>,
    ask_nonce: Option<String>,
) -> Result<String, String> {
    let req = ManifestRequest {
        hb: MANIFEST_REQUEST_TAG,
        slug: slug.to_string(),
        fingerprint_seen: fingerprint_seen.to_string(),
        teaser_event_id,
        ask_nonce,
        mascara_pubkey,
        // Carrier 4 not in play from this builder: the ordinary ask targets the asked peer's own
        // collection. A third-party-author ask goes through `build_manifest_request_for_author`.
        author_npub: None,
    };
    serde_json::to_string(&req).map_err(cmd_err)
}

/// Carrier 4 (QURATOR-79) — build a manifest-request DM body naming a **third-party author**: peer D
/// asks peer C for a manifest peer A authored, so C can re-serve it from its cache. `author_npub`
/// must be a non-empty npub string; empty is normalised to `None` so "present but blank" cannot
/// masquerade as a real author pin on the wire (same normalisation the ticket side applies to
/// `ask_nonce`). Pure — unit-tested without a relay, alongside the frozen-wire tests below.
///
/// No production caller yet — the carrier-4 UI slice that drives a third-party ask is a later
/// slice — hence the `#[allow(dead_code)]` outside `test` (same shape as `logging.rs`'s
/// copy-diagnostics helper). The wire body it emits is production regardless.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_manifest_request_for_author(
    slug: &str,
    fingerprint_seen: &str,
    teaser_event_id: Option<String>,
    mascara_pubkey: Option<String>,
    ask_nonce: Option<String>,
    author_npub: &str,
) -> Result<String, String> {
    let req = ManifestRequest {
        hb: MANIFEST_REQUEST_TAG,
        slug: slug.to_string(),
        fingerprint_seen: fingerprint_seen.to_string(),
        teaser_event_id,
        ask_nonce,
        mascara_pubkey,
        author_npub: if author_npub.is_empty() { None } else { Some(author_npub.to_string()) },
    };
    serde_json::to_string(&req).map_err(cmd_err)
}

/// M16 W4 — DM the hoarder a structured request for the full manifest of a truncated collection (the
/// blessed "ask by DM" seam). One relay write; the hoarder then decides — **the ruling that a human
/// decides survives, but the mechanism it named is dead.** This used to end in "export + ticket it in
/// Mascara"; since M18 W4 the hoarder clicks *Send the full list* and the manifest crosses Hoardbook's
/// own plane, with export as the fallback. Hoardbook still never auto-produces anything.
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
    // Mint the nonce HERE, not in the UI: it must be the same value that reaches both the wire and
    // the local trace, and a caller that could supply it could also replay one.
    let ask_nonce = new_ask_nonce();
    // Carrier 4 not in play: this command asks the PEER for the peer's own collection. A
    // third-party-author ask (peer D asking C for A's manifest) has no call site yet — the carrier-4
    // slice that drives it passes the author here via `build_manifest_request_for_author`.
    let content = build_manifest_request(
        &slug,
        &fingerprint_seen,
        teaser_event_id,
        mascara_pubkey,
        Some(ask_nonce.clone()),
    )?;
    let own = net::relay_urls(&store);
    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    send_dm_inner(&client, &id_clone, &recipient, &content, &own, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;
    // M17 W7.1a — record the ask AFTER `send_dm_inner` resolves, so a failed publish never leaves a
    // trace (a rejected/errored ask must never render as "Asked"). `send_dm_inner` delivers to the
    // recipient's inbox only (no self-copy), so without this record the ask leaves zero local trace and
    // the button reads as dead on the requester's side. One entry per `(npub, slug)`, overwritten on
    // re-ask; the re-ask cooldown is derived client-side from `sent_at`.
    //
    // Carrier 4: the ask is keyed on the AUTHOR it asks about, which for this command (the ordinary
    // owner-path ask — see `build_manifest_request`) is the asked peer itself.
    let sent_at = chrono::Utc::now().to_rfc3339();
    store
        .record_manifest_ask(&npub, &npub, &slug, &fingerprint_seen, &sent_at, &ask_nonce)
        .map_err(cmd_err)?;
    Ok(())
}

/// M17 W7.1a — the persisted ask-trace map (npub|slug → {fingerprint_seen, sent_at}), so the Browse
/// paywall can read back the asked-state across restarts. A pure local read, no relay I/O.
#[tauri::command]
pub async fn get_manifest_asks(
    store: State<'_, DataStore>,
) -> CmdResult<std::collections::HashMap<String, crate::store::ManifestAsk>> {
    store.load_manifest_asks().map_err(cmd_err)
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
    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    // Read the cursor for the fetch filter OUTSIDE the lock — `since` is bandwidth-only (see
    // `dm_inbox_filter`'s doc), never a correctness boundary, so a cursor that's a moment stale here
    // just costs one extra re-fetched wrap (deduped by wrap id below), never a lost message.
    let cursor = store.load_dm_cache(&id_clone).map_err(cmd_err)?.newest_seen_outer.min(now);
    let filter = dm_inbox_filter(id_clone.public_key(), cursor);
    let wraps = client.fetch(filter, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;

    // M2: reload + merge + save is ONE atomic transaction behind `DM_CACHE_LOCK`, taken only after
    // the (potentially slow) relay fetch returns — no relay I/O happens while the lock is held.
    // Reloading here — rather than reusing a snapshot taken before the fetch — is what actually closes
    // the race: a `persist_sent_dm`/request-accept transaction that ran while the fetch was in flight
    // must not be clobbered by a save based on stale pre-fetch state.
    let guard = DM_CACHE_LOCK.lock().await;
    let mut cache = store.load_dm_cache(&id_clone).map_err(cmd_err)?;
    // Heal a poisoned/future cursor before it drives the next fetch window (a stale install may carry
    // one).
    let healed = cache.newest_seen_outer > now;
    if healed {
        cache.newest_seen_outer = now;
    }
    let (requests, merged) = merge_wraps_into_cache(&id_clone, &own_npub, wraps, &ctx, &mut cache, now).await;
    let pruned = cache.prune();
    // Only re-seal + write when something actually changed — an idle 3s poll (all wraps already seen)
    // leaves the cache untouched, so it costs no disk write / re-encrypt. The explicit dirty flags
    // catch a balanced push+prune the length tuple alone would miss.
    if healed || merged || pruned {
        store.save_dm_cache(&id_clone, &cache).map_err(cmd_err)?;
    }
    drop(guard); // release before the separate-file Request-bucket write below (its own DM_REQUESTS_LOCK)

    if !requests.is_empty() {
        // DM_REQUESTS_LOCK: see the lock's doc — serializes against accept/decline/block on the same
        // `dm_requests.json`.
        let _guard = DM_REQUESTS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let existing = store.load_dm_requests(&id_clone).map_err(cmd_err)?;
        let merged = merge_into_requests(existing, requests, now_secs());
        store.save_dm_requests(&id_clone, &merged).map_err(cmd_err)?;
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
    let (own_npub, id_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded.")?;
        (id.npub(), id.identity.clone())
    };
    dm_requests_inner(&store, &id_clone, &own_npub)
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
    dm_request_accept_inner(&store, &id_clone, &own_npub, npub, petname).await
}

#[tauri::command]
pub async fn dm_request_decline(
    npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let id_clone = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded.")?;
        id.identity.clone()
    };
    dm_request_decline_inner(&store, &id_clone, npub, now_secs())
}

#[tauri::command]
pub async fn dm_block(
    npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let id_clone = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded.")?;
        id.identity.clone()
    };
    dm_block_inner(&store, &id_clone, npub)
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

    /// **The `ManifestRequest` half of the M18 wire contract, pinned here** because `wire_freeze`
    /// lives in hb-core and cannot see this type. Without it the ticket half was frozen and this half
    /// was not: dropping or renaming `ask_nonce` would make owners emit nonce-less tickets and make
    /// every current requester silently fail closed — a total outage of automatic delivery, with a
    /// green suite.
    #[test]
    fn manifest_request_ask_nonce_is_wire_frozen() {
        const FREEZE: &str = "FROZEN WIRE FIELD — changing this breaks in-flight requests";

        let json = build_manifest_request("s", "fp", None, None, Some("n0nce".into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ask_nonce"], "n0nce", "ManifestRequest.ask_nonce field name — {FREEZE}");

        // Absent, not null, when there is none — so a client that predates the field parses a new
        // request exactly as it always did.
        let json = build_manifest_request("s", "fp", None, None, None).unwrap();
        assert!(
            !json.contains("ask_nonce"),
            "an absent ask nonce is omitted, not null — {FREEZE}"
        );

        // And a legacy body with no nonce at all is still a recognisable request.
        let legacy = r#"{"hb":"manifest_request","slug":"s","fingerprint_seen":"fp"}"#;
        let v: serde_json::Value = serde_json::from_str(legacy).unwrap();
        assert_eq!(v["hb"], MANIFEST_REQUEST_TAG, "a pre-nonce request still reads as a request");
        assert!(v.get("ask_nonce").is_none());
    }

    /// **The `ManifestRequest.author_npub` half of the carrier-4 ask wire (QURATOR-79)** — same
    /// pinning discipline as `manifest_request_ask_nonce_is_wire_frozen` above. Additive optional
    /// field, no discriminant bump: the exemption is earned ONLY by covering present AND absent,
    /// which is strictly more coverage than a bump (a bump proves a number changed; this proves
    /// both shapes a peer can actually receive still parse as requests).
    ///
    /// MUTATION (P-10) — each arm reds under one production edit, all inside `build_manifest_request`
    /// or `build_manifest_request_for_author` (resolved by containing function, not text):
    /// 1. present-arm: inside `build_manifest_request_for_author`, hardcode the `author_npub:`
    ///    init to `None` (or rename the struct field / its serde key) → `v["author_npub"]` reads
    ///    `null` and the `assert_eq!` against "npub1a" fails.
    /// 2. absent-arm: drop `#[serde(skip_serializing_if = "Option::is_none")]` from the struct's
    ///    `author_npub` field → the ordinary body emits `"author_npub":null` and the
    ///    `!json.contains("author_npub")` assert fails.
    /// 3. legacy-arm: change `MANIFEST_REQUEST_TAG`'s value, or make `build_manifest_request`
    ///    serialise `hb` from a different constant, → `v["hb"]` no longer equals the tag and the
    ///    pre-author request stops reading as a request.
    #[test]
    fn manifest_request_author_npub_is_wire_frozen() {
        const FREEZE: &str = "FROZEN WIRE FIELD — changing this breaks in-flight requests";

        // 1. PRESENT — a carrier-4 ask names the third-party author, and the field name is the
        //    contract the asked peer's inbox parses against.
        let json =
            build_manifest_request_for_author("s", "fp", None, None, Some("n0nce".into()), "npub1a")
                .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["author_npub"], "npub1a", "ManifestRequest.author_npub field name — {FREEZE}");

        // 2. ABSENT — `None` omits the key entirely (never `null`), so a client that predates the
        //    field parses a new request exactly as it always did, and `None` keeps meaning "the
        //    asked peer's own collection" — today's semantics, no weaker behaviour to fall into.
        let json = build_manifest_request("s", "fp", None, None, Some("n0nce".into())).unwrap();
        assert!(
            !json.contains("author_npub"),
            "an absent author is omitted, not null — {FREEZE}"
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("author_npub").is_none());

        // 3. LEGACY / wrong-discriminator: deliberately NOT asserted here, audit 2026-08-31. The
        //    arm that stood here parsed a string literal written by the test into a generic
        //    `serde_json::Value` and asserted the `hb` field it had itself hardcoded — no
        //    production code ran, so it could not fail for the reason it claimed. It cannot be
        //    fixed in place either: `ManifestRequest` derives `Serialize` only, and nothing in Rust
        //    deserializes a request body. The recogniser is TypeScript, and the property is really
        //    pinned in `ui/src/lib/request-inbox.test.ts` against the real `parseManifestRequest`.
    }

    #[test]
    fn is_self_send_rejects_own_pubkey_devtest_14() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        assert!(is_self_send(&me.public_key(), &me.public_key()));
        assert!(!is_self_send(&stranger.public_key(), &me.public_key()));
    }

    #[test]
    fn dm_delivery_targets_keeps_own_and_public_peer_relays() {
        // Own relays are trusted (validated on save) and never filtered — even a local dev relay.
        let own = vec!["wss://own.example".to_string(), "ws://localhost:7777".to_string()];
        let targets = vec![
            "ws://localhost:7777".to_string(),      // own, local — kept because it is ours
            "wss://peer.example".to_string(),       // peer, public wss — kept
            "ws://198.51.100.1:7777".to_string(), // peer, public plain-ws VPS relay — kept (address class, not scheme)
        ];
        let kept = dm_delivery_targets(&own, targets);
        assert_eq!(kept.len(), 3, "own + public peer relays all survive, got {kept:?}");
        assert!(kept.contains(&"ws://localhost:7777".to_string()), "own local relay is never filtered");
        assert!(kept.contains(&"ws://198.51.100.1:7777".to_string()), "plain-ws public relay kept");
    }

    #[test]
    fn dm_delivery_targets_drops_ssrf_peer_relays() {
        // A stranger's NIP-65 read-list can name internal hosts; every such target must be dropped
        // while the caller's own relays survive untouched.
        let own = vec!["wss://own.example".to_string()];
        let targets = vec![
            "wss://own.example".to_string(),
            "ws://127.0.0.1:7777".to_string(),   // loopback
            "ws://10.0.0.5:7777".to_string(),    // RFC1918
            "ws://169.254.1.1:7777".to_string(), // link-local
            "ws://[::1]:7777".to_string(),       // IPv6 loopback
            "wss://localhost".to_string(),       // literal hostname
            "ws://100.64.0.1:7777".to_string(),  // CGNAT
        ];
        let kept = dm_delivery_targets(&own, targets);
        assert_eq!(
            kept,
            vec!["wss://own.example".to_string()],
            "every peer-authored internal target is dropped"
        );
    }

    #[test]
    fn manifest_request_json_is_tagged_and_omits_absent_options() {
        // M16 W4: the DM body is `{hb:"manifest_request", slug, fingerprint_seen}` — the frontend
        // detects the tag and renders a light hint. Absent optional fields are omitted (not null).
        let json = build_manifest_request("criterion", "abc123", None, None, None).unwrap();
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
            build_manifest_request("s", "fp", Some("evt1".into()), Some("mpub".into()), Some("n1".into())).unwrap();
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
    fn dm_inbox_filter_declares_an_explicit_limit() {
        // audit #11: without `.limit()` the client left the fetch budget to the relay's own default;
        // an explicit bound keeps the response size ours (CWE-400).
        let me = Identity::generate();
        let cold = dm_inbox_filter(me.public_key(), 0);
        assert_eq!(cold.limit, Some(DM_INBOX_FETCH_LIMIT), "the cold-cache filter declares a budget");
        let incremental = dm_inbox_filter(me.public_key(), now_secs());
        assert_eq!(incremental.limit, Some(DM_INBOX_FETCH_LIMIT), "the incremental filter too");
    }

    #[tokio::test]
    async fn failed_unwrap_is_recorded_apart_from_the_success_ledger() {
        // audit #11: a wrap that fails to unwrap is remembered in the (bounded) negative cache so the
        // next poll can skip it — while staying OUT of the success ledger (`seen_wraps`), which is
        // exactly the gap the DoS exploited (only successes advanced the ledger, so a failed wrap was
        // re-fetched and re-unwrapped every poll for 48 h).
        let me = Identity::generate();
        let attacker = Identity::generate();
        let garbage = attacker.sign(EventBuilder::new(Kind::GiftWrap, "junk")).unwrap();
        let garbage_id = garbage.id.to_hex();

        let empty: HashSet<String> = HashSet::new();
        let ctxv = ctx(&empty, &empty, &empty, true);
        let mut cache = DmCache::default();
        let (requests, _) =
            merge_wraps_into_cache(&me, &me.npub(), vec![garbage], &ctxv, &mut cache, now_secs()).await;

        assert!(requests.is_empty(), "a failed unwrap yields no request");
        assert!(cache.seen_wraps.is_empty(), "a failed unwrap never enters the success ledger");
        assert!(failed_wrap_seen(&me.npub(), &garbage_id), "…but IS remembered in the negative cache");
    }

    #[tokio::test]
    async fn negative_cached_wrap_is_skipped_before_unwrap() {
        // audit #11: an id already in the negative cache must short-circuit BEFORE `unwrap_dm` runs
        // again. We seed the cache with the id of a wrap that WOULD decode, so the absence of the
        // decoded message proves the unwrap never ran — the ~15 s poll no longer re-runs crypto on a
        // hostile wrap for 48 h.
        let me = Identity::generate();
        let contact = Identity::generate();
        let wrap = build_dm(&contact, &me.public_key(), "would decode").await.unwrap();
        record_failed_wrap(&me.npub(), wrap.id.to_hex());

        let contacts: HashSet<String> = [contact.npub()].into_iter().collect();
        let empty: HashSet<String> = HashSet::new();
        let ctxv = ctx(&contacts, &empty, &empty, true);
        let mut cache = DmCache::default();
        let (requests, _) =
            merge_wraps_into_cache(&me, &me.npub(), vec![wrap], &ctxv, &mut cache, now_secs()).await;

        assert!(cache.messages.is_empty(), "a negative-cached wrap is not re-unwrapped");
        assert!(requests.is_empty(), "…and produces no request");
        assert!(cache.seen_wraps.is_empty(), "…and is never recorded as a success");
    }

    #[tokio::test]
    async fn negative_cache_does_not_cross_identity_boundaries() {
        // audit #11 follow-up (finding A): the negative cache must key on the RECIPIENT identity, not
        // the wrap id alone. `unwrap_dm` is deterministic over (identity, wrap) — a wrap addressed to
        // identity B fails under identity A but decodes under B (a hostile relay feeding B's wrap
        // while A is active, or a wipe/restore in `commands/identity.rs`). A wrap-id-only cache would
        // let A's failure poison the GLOBAL cache and skip B's valid message — silent message loss.
        let me_a = Identity::generate();
        let me_b = Identity::generate();
        let contact = Identity::generate();

        // A wrap addressed to B: valid under B, undecryptable under A.
        let wrap = build_dm(&contact, &me_b.public_key(), "for b").await.unwrap();
        let wrap_id = wrap.id.to_hex();

        // Simulate A polling it and failing: the failure is recorded in the (global) negative cache.
        record_failed_wrap(&me_a.npub(), wrap_id.clone());

        // Under B the SAME wrap must still be attempted and decoded — not skipped on A's failure.
        let contacts: HashSet<String> = [contact.npub()].into_iter().collect();
        let empty: HashSet<String> = HashSet::new();
        let ctxv = ctx(&contacts, &empty, &empty, true);
        let mut cache = DmCache::default();
        let (requests, _) =
            merge_wraps_into_cache(&me_b, &me_b.npub(), vec![wrap], &ctxv, &mut cache, now_secs()).await;

        assert_eq!(cache.messages.len(), 1, "a wrap failed under A is still attempted (and decoded) under B");
        assert_eq!(cache.messages[0].content, "for b", "…and the decoded content is B's message");
        assert!(requests.is_empty(), "…routing to the contact's inbox, not a request");
    }

    #[test]
    fn failed_wrap_cache_is_bounded_and_fifo_evicts() {
        // audit #11: the negative cache is fed by attacker-controlled ids, so it must be a hard bound
        // (an unbounded one just moves the DoS to memory). FIFO: oldest evicted first.
        let mut c = FailedWrapCache::new();
        for i in 0..MAX_FAILED_WRAPS {
            c.insert(format!("id{i}"));
        }
        assert_eq!(c.set.len(), MAX_FAILED_WRAPS, "the cache holds exactly the cap");
        assert!(c.contains("id0"), "the first-inserted id survives at exactly the cap");
        c.insert("overflow".into());
        assert_eq!(c.set.len(), MAX_FAILED_WRAPS, "the cap is a hard bound, never exceeded");
        assert!(!c.contains("id0"), "FIFO: the oldest entry is evicted first");
        assert!(c.contains("overflow"), "the newest entry is retained");
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

    #[tokio::test]
    async fn request_accept_adds_manual_contact_no_browse_key_and_drains_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let npub = "npub1stranger".to_string();
        store
            .save_dm_requests(&me, &[DmRequestBucket {
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

        let drained = dm_request_accept_inner(&store, &me, "npub1me", npub.clone(), None).await.unwrap();
        assert_eq!(drained.len(), 2, "both messages are drained into the conversation");
        assert_eq!(drained[0].content, "hi");

        let contact = store.load_contact(&CachedPeer::pubkey_hash(&npub)).unwrap().unwrap();
        assert_eq!(contact.source, ContactSource::Manual);
        assert!(contact.browse_key_hex.is_none(), "an accepted request contact carries no browse-key");
        assert!(contact.petname.is_none(), "petname=None leaves the default unset");
        assert!(store.load_dm_requests(&me).unwrap().is_empty(), "the bucket is gone after accept");
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
            .save_dm_requests(&me, &[DmRequestBucket { npub: npub2.clone(), first_seen: 1, last_message_at: 1, messages: vec![] }])
            .unwrap();
        dm_request_accept_inner(&store, &me, "npub1me", npub2.clone(), Some("Bob".into())).await.unwrap();
        let contact2 = store.load_contact(&CachedPeer::pubkey_hash(&npub2)).unwrap().unwrap();
        assert_eq!(contact2.petname.as_deref(), Some("Bob"));
    }

    #[test]
    fn request_decline_persists_and_block_removes_bucket_and_declined() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let npub = "npub1stranger".to_string();
        store
            .save_dm_requests(&me, &[DmRequestBucket {
                npub: npub.clone(),
                first_seen: 1,
                last_message_at: 1,
                messages: vec![RequestMessage { wrap_id: "w1".into(), content: "hi".into(), sent_at: "t".into() }],
            }])
            .unwrap();

        dm_request_decline_inner(&store, &me, npub.clone(), 100).unwrap();
        assert!(store.load_dm_requests(&me).unwrap().is_empty(), "the bucket is gone after decline");
        let declined = store.load_dm_declined().unwrap();
        assert!(declined.iter().any(|(n, _)| n == &npub), "the decline is remembered");

        // Re-seed a bucket (as if the stranger messaged again) and block instead.
        store
            .save_dm_requests(&me, &[DmRequestBucket { npub: npub.clone(), first_seen: 2, last_message_at: 2, messages: vec![] }])
            .unwrap();
        dm_block_inner(&store, &me, npub.clone()).unwrap();
        assert!(store.load_dm_requests(&me).unwrap().is_empty(), "block removes any bucket");
        assert!(store.load_dm_declined().unwrap().is_empty(), "block also clears the decline record (blocked supersedes)");
        assert!(store.load_dm_blocked().unwrap().contains(&npub));

        dm_unblock_inner(&store, npub.clone()).unwrap();
        assert!(!store.load_dm_blocked().unwrap().contains(&npub));
    }

    /// QURATOR-141 — a PROACTIVE block (Settings' block-by-npub, no prior DM Request, no contact
    /// record) must reach the same DM-acceptance path chat's `handleBlock` feeds. The acceptance
    /// classifier is `route_dm` driven by `get_messages`'s fresh `load_dm_blocked()` read, keyed on
    /// the bare npub string — NOT on any request record — so a store-side block is enforced there
    /// with zero relationship prerequisites. This pins that end-to-end at the store level: block a
    /// stranger with no bucket/decline/contact anywhere, then assert (a) their subsequent DM is
    /// REFUSED (`route_dm` → Drop), not merely hidden, (b) a stranger-Request is never created for
    /// them, and (c) unblock restores acceptance (Request, since they're still a stranger).
    #[test]
    fn proactive_block_refuses_later_dms_and_unblock_restores_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let stranger = "npub1hostile".to_string();
        let own_npub = me.npub();

        // The proactive state: nothing exists for this npub — no Request bucket, no decline, no
        // contact. `dm_block_inner` must work purely from the npub (creating a contact is out of
        // scope by owner ruling).
        assert!(store.load_dm_requests(&me).unwrap().is_empty());
        assert!(store.load_dm_declined().unwrap().is_empty());
        dm_block_inner(&store, &me, stranger.clone()).unwrap();

        // (a) REFUSED, not hidden: the blocklist written by the proactive block is exactly what
        // `get_messages` loads into `DmClassifyCtx::blocked`, and `route_dm` consults it FIRST —
        // blocked supersedes contact, decline, and the stranger-Request path alike.
        let blocked: HashSet<String> = store.load_dm_blocked().unwrap().into_iter().collect();
        assert!(blocked.contains(&stranger), "the proactive block landed in dm_blocked.json");
        let contacts: HashSet<String> = HashSet::new();
        let declined: HashSet<String> = HashSet::new();
        let on = ctx(&contacts, &blocked, &declined, true);
        assert_eq!(
            route_dm(&stranger, &own_npub, &on),
            DmRoute::Drop,
            "a proactively-blocked stranger's DM is refused at acceptance, not merely hidden"
        );

        // (b) No Request entry may be created for them (blocked supersedes the stranger-Request
        // path — this is the WAN-C C5 shape, pinned here at the unit level too).
        assert!(store.load_dm_requests(&me).unwrap().is_empty());

        // (c) Unblock restores acceptance: same stranger, now no contact/decline and allow_dms on,
        // routes to Request again.
        dm_unblock_inner(&store, stranger.clone()).unwrap();
        let blocked_after: HashSet<String> = store.load_dm_blocked().unwrap().into_iter().collect();
        let restored = ctx(&contacts, &blocked_after, &declined, true);
        assert_eq!(
            route_dm(&stranger, &own_npub, &restored),
            DmRoute::Request,
            "unblock restores stranger-DM acceptance"
        );
    }

    // ── QURATOR-91 — own sent history survives a restart via the at-rest cache ──────────────────

    /// The inbox fetch filter (`dm_inbox_filter`) is `Kind::GiftWrap` + `.pubkey(me)` — the `#p`
    /// addressed-to-us filter — so our OWN sent wraps (addressed to the recipient) never enter the
    /// feed. The ONLY way an own send survives a restart is `send_message` persisting it into the
    /// cache itself. The read path is already own-send-ready (`route_dm` routes `from == own_npub`
    /// to `Inbox`; `cached_inbox` admits `m.from == own_npub`), so the fix is a local-cache write on
    /// the existing send path — no wire, sealing, or relay change. This test drives the exact persist
    /// seam `send_message` calls, against a real `DataStore` tempdir, and asserts the round-trip a
    /// restart performs.
    #[tokio::test]
    async fn send_message_persists_own_send_into_dm_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let peer = Identity::generate();

        // The EXACT send+persist seam `send_message` drives (the QURATOR-83 lesson: a test that
        // exercises its own persist helper stays green when the production call site reverts). The
        // wrap-producing half is the no-I/O `build_dm` — the offline stand-in for `send_dm_inner`'s
        // publish, which returns this same wrap.
        let built = build_dm(&me, &peer.public_key(), "my own sent line").await.unwrap();
        let (_wrap, sent_at) = send_dm_and_cache_inner(
            &store,
            &me,
            &peer.public_key(),
            "my own sent line",
            std::future::ready(Ok(built.clone())),
        )
        .await
        .unwrap();

        // The restart round-trip: reload from disk, reclassify under the CURRENT sets.
        let cache = store.load_dm_cache(&me).unwrap();
        let inbox = cached_inbox(&cache, &me.npub(), &HashSet::new(), &HashSet::new());
        assert_eq!(inbox.len(), 1, "the own send survives a restart via the at-rest cache");
        assert_eq!(inbox[0].from, me.npub(), "attributed to the sender (us)");
        assert_eq!(inbox[0].to, npub_of(&peer.public_key()), "addressed to the peer, not ourselves");
        assert_eq!(inbox[0].content, "my own sent line");

        // QURATOR M3 regression: `send_message` builds its returned `ReceivedMessage.sent_at` from
        // the SAME string this seam returns (it does not call `Utc::now()` again). Assert that
        // returned string against the cached entry's `sent_at` directly, so this reds under the old
        // two-`Utc::now()` code (one stamp in `persist_sent_dm`, a second, later one in the command)
        // without needing to drive the full Tauri command.
        assert_eq!(
            inbox[0].sent_at, sent_at,
            "M3: the cache entry and the command's returned timestamp are the SAME captured instant, \
             not two independent Utc::now() calls — otherwise the feed echo re-arrives under a \
             different sent_at than the session-appended bubble and the UI dedups on (from, sent_at, \
             to, content) fail, duplicating the bubble"
        );

        // Re-driving the seam with the SAME wrap id is a no-op — the cache entry keys on the wrap id,
        // so a retried/publish-acked resend can never double-insert the bubble. (A fresh build_dm
        // would mint a fresh wrap; the dedup case needs the id the first call persisted.)
        let same_wrap_id = store.load_dm_cache(&me).unwrap().messages[0].wrap_id.clone();
        assert_eq!(same_wrap_id, built.id.to_hex(), "the cached entry keys on the real wrap id");
        send_dm_and_cache_inner(
            &store,
            &me,
            &peer.public_key(),
            "my own sent line",
            std::future::ready(Ok(built)),
        )
        .await
        .unwrap();
        let cache2 = store.load_dm_cache(&me).unwrap();
        assert_eq!(
            cache2.messages.iter().filter(|m| m.from == me.npub()).count(),
            1,
            "re-persisting the same wrap is a no-op (dedup by wrap id)"
        );
    }

    // ── M2 — DM_CACHE_LOCK serializes concurrent load→mutate→save transactions ──────────────────
    //
    // `persist_sent_dm` (the send-echo path) and `dm_request_accept_inner`'s cache migration are two
    // INDEPENDENT, directly-callable production transactions that each load → mutate → save
    // `dm_cache.json` (no Tauri State or live relay needed for either — both take a plain
    // `&DataStore`). Hammering many of each concurrently, on a real multi-thread runtime so the two
    // families of task can genuinely overlap on separate OS threads, is what gives the two writers a
    // chance to race for real: `write_atomic` makes a single writer's file replace atomic (no torn
    // JSON), but WITHOUT DM_CACHE_LOCK two overlapping transactions can each load before the other's
    // save lands, and the later save then clobbers the earlier one outright (last-write-wins,
    // classic lost update) — every entry from the loser's transaction disappears with no error, no
    // corruption, nothing but a message count short of a Chat pane. With the lock, the two
    // transactions' load→mutate→save spans cannot overlap AT ALL, so no interleaving can ever get one
    // transaction's load in ahead of the other's save.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn dm_cache_lock_survives_concurrent_persist_and_accept_hammering() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        let peer = Identity::generate();

        const N: usize = 25;

        // Seed N distinct Request buckets up front — one per concurrent accept task.
        let buckets: Vec<DmRequestBucket> = (0..N)
            .map(|i| DmRequestBucket {
                npub: format!("npub1stranger{i}"),
                first_seen: 1,
                last_message_at: 1,
                messages: vec![RequestMessage {
                    wrap_id: format!("req-wrap-{i}"),
                    content: format!("hi {i}"),
                    sent_at: "2026-01-01T00:00:00Z".into(),
                }],
            })
            .collect();
        store.save_dm_requests(&me, &buckets).unwrap();

        let mut handles = Vec::new();
        for i in 0..N {
            // build_dm mints a fresh, distinct wrap (its own event id) per iteration — a real
            // production wrap, not a lookalike — so each persist_sent_dm call is a genuinely distinct
            // cache-adding transaction, not N no-op retries of the same wrap id.
            let wrap = build_dm(&me, &peer.public_key(), &format!("send {i}")).await.unwrap();

            let store_a = store.clone();
            let me_a = me.clone();
            let peer_pk = peer.public_key();
            let content = format!("send {i}");
            let sent_at = format!("2026-01-02T00:00:{i:02}Z");
            handles.push(tokio::spawn(async move {
                persist_sent_dm(&store_a, &me_a, &wrap, &peer_pk, &content, &sent_at).await.unwrap();
            }));

            let store_b = store.clone();
            let me_b = me.clone();
            let npub_i = format!("npub1stranger{i}");
            handles.push(tokio::spawn(async move {
                dm_request_accept_inner(&store_b, &me_b, "npub1me", npub_i, None).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let cache = store.load_dm_cache(&me).unwrap();
        let sent_count = cache.messages.iter().filter(|m| m.content.starts_with("send ")).count();
        let accepted_count = cache.messages.iter().filter(|m| m.wrap_id.starts_with("req-wrap-")).count();
        assert_eq!(
            sent_count, N,
            "DM_CACHE_LOCK (M2): every concurrent send-persist transaction landed — none lost to a \
             racing accept's save"
        );
        assert_eq!(
            accepted_count, N,
            "DM_CACHE_LOCK (M2): every concurrent request-accept migration landed — none lost to a \
             racing send-persist's save"
        );
        assert!(
            store.load_dm_requests(&me).unwrap().is_empty(),
            "all N buckets were drained — no accept silently no-op'd on a stale read"
        );
    }

    // ── #33 — DM_DECLINED_LOCK / DM_BLOCKED_LOCK serialize concurrent block/decline saves ─────────
    //
    // `dm_block_inner` (blocked-add) and `dm_request_decline_inner` (decline-add) each load → mutate
    // → save their own file. Without their respective locks, two concurrent blocks/declines would
    // last-write-wins away one entry (the finding's "block two spammers back to back" scenario) — the
    // exact class DM_REQUESTS_LOCK already closed for dm_requests.json. Hammer N of each on real OS
    // threads (these are sync fns, unlike the async DM_CACHE_LOCK hammer above) and assert every
    // entry landed.

    #[test]
    fn dm_blocked_lock_survives_concurrent_block_hammering() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        const N: usize = 25;
        let mut handles = Vec::new();
        for i in 0..N {
            let store = store.clone();
            let me = me.clone();
            let npub = format!("npub1block{i}");
            handles.push(std::thread::spawn(move || {
                dm_block_inner(&store, &me, npub).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let blocked = store.load_dm_blocked().unwrap();
        assert_eq!(
            blocked.len(),
            N,
            "DM_BLOCKED_LOCK (#33): every concurrent block landed — none lost to a racing block's save"
        );
    }

    #[test]
    fn dm_declined_lock_survives_concurrent_decline_hammering() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        const N: usize = 25;
        let mut handles = Vec::new();
        for i in 0..N {
            let store = store.clone();
            let me = me.clone();
            let npub = format!("npub1decline{i}");
            handles.push(std::thread::spawn(move || {
                dm_request_decline_inner(&store, &me, npub, 1_700_000_000 + i as u64).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let declined = store.load_dm_declined().unwrap();
        assert_eq!(
            declined.len(),
            N,
            "DM_DECLINED_LOCK (#33): every concurrent decline landed — none lost to a racing decline's save"
        );
    }

    // ── finding B — block-vs-decline must not race into a stale decline ──────────────────────────
    //
    // `dm_block_inner` clears the decline under DM_DECLINED_LOCK and then adds the block under
    // DM_BLOCKED_LOCK — two separate scopes. A concurrent `dm_request_decline_inner` (DM_DECLINED_LOCK)
    // can re-add the decline in between, leaving the sender blocked AND declined; on a later unblock
    // they surface as silently declined, a state the user never chose. Blocked must supersede decline
    // at REST, not just at classification. Hammer one block + one decline per distinct npub on real OS
    // threads; the invariant `blocked ⇒ not declined` must hold for every npub no matter the ordering.

    #[test]
    fn block_and_decline_do_not_race_into_a_stale_decline() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let me = Identity::generate();
        const N: usize = 100;
        let mut handles = Vec::new();
        for i in 0..N {
            let npub = format!("npub1race{i}");
            let store_b = store.clone();
            let me_b = me.clone();
            let npub_b = npub.clone();
            handles.push(std::thread::spawn(move || {
                dm_block_inner(&store_b, &me_b, npub_b).unwrap();
            }));
            let store_d = store.clone();
            let me_d = me.clone();
            handles.push(std::thread::spawn(move || {
                dm_request_decline_inner(&store_d, &me_d, npub, 1_700_000_000 + i as u64).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let blocked = store.load_dm_blocked().unwrap();
        let declined = store.load_dm_declined().unwrap();
        assert_eq!(blocked.len(), N, "every concurrent block landed");
        assert_eq!(
            declined.len(),
            0,
            "blocked supersedes decline: no racing decline may leave a stale decline that a later \
             unblock would reveal"
        );
        // The concrete user-visible bug: after unblocking, the sender is not silently declined.
        dm_unblock_inner(&store, "npub1race0".to_string()).unwrap();
        let declined_after = store.load_dm_declined().unwrap();
        assert!(
            !declined_after.iter().any(|(n, _)| n == "npub1race0"),
            "unblocking a racing-declined sender must not reveal a silent decline"
        );
    }

    // ── QURATOR-161 — the #[tauri::command] guards, driven through the command itself ──────────
    //
    // The v0.18.0 post-mortem: the WAN harness re-implemented command bodies step by step, and the
    // one step it never copied was the broken one. These tests call `send_message` ITSELF — the
    // `#[tauri::command]` fn at its real signature, `State<'_, T>` and all — via tauri's own test
    // scaffolding (`mock_app()` + the app's `StateManager`), so what is pinned is the command the
    // frontend invokes, not a lookalike. No re-implementation of any guard appears here.
    //
    // All three guards fire BEFORE `net::client` (the first network I/O), so no relay is ever
    // dialled: the tests are offline and hermetic by construction, not by chance.
    mod command_guards {
        use super::*;
        use crate::identity_state::AppIdentity;
        use std::sync::Arc;
        use tauri::Manager;

        /// `send_message` as the frontend calls it: the real command fn with real managed `State`.
        /// Tauri's `State<'_, T>` has a private field, so the ONLY way to mint one outside the IPC
        /// machinery is `StateManager::get` — which is exactly what the `#[tauri::command]` macro's
        /// generated `CommandArg` impl asks at invoke time. This is the same seam, driven directly.
        async fn send_message_via_command(
            app: &tauri::App<tauri::test::MockRuntime>,
            to: &str,
            content: &str,
        ) -> CmdResult<ReceivedMessage> {
            send_message(
                to.to_string(),
                content.to_string(),
                app.state::<SharedIdentity>(),
                app.state::<DataStore>(),
                app.state::<SharedRelay>(),
            )
            .await
        }

        fn guard_app(identity_loaded: bool) -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            let identity: SharedIdentity = Arc::new(tokio::sync::RwLock::new(
                identity_loaded.then(AppIdentity::generate),
            ));
            app.manage(Arc::clone(&identity));
            app.manage(store);
            app.manage(net::new_shared());
            app
        }

        #[tokio::test]
        async fn send_message_command_rejects_empty_and_whitespace() {
            let app = guard_app(true);
            let peer = Identity::generate().npub();
            for empty in ["", "   ", "\n\t "] {
                let err = send_message_via_command(&app, &peer, empty)
                    .await
                    .unwrap_err();
                assert_eq!(err, "Message cannot be empty", "content {empty:?}");
            }
        }

        #[tokio::test]
        async fn send_message_command_rejects_over_4096_chars() {
            let app = guard_app(true);
            let peer = Identity::generate().npub();
            let err = send_message_via_command(&app, &peer, &"x".repeat(4097))
                .await
                .unwrap_err();
            assert_eq!(err, "Message too long (4097 chars, max 4096)");
            // The boundary's other side: exactly 4096 PASSES the length guard. Proven hermetically
            // by pairing it with an unparseable recipient, which errors at `parse_recipient` — the
            // very next statement after the guard — so the command never reaches the relay connect.
            // This is what reds under an off-by-one (`>=`) mutation of the comparison.
            let err = send_message_via_command(&app, "not-a-share-code", &"x".repeat(4096))
                .await
                .unwrap_err();
            assert!(
                err.starts_with("Invalid recipient:"),
                "4096 chars clears the length guard and fails at the recipient parse instead, \
                 got {err}"
            );
        }

        #[tokio::test]
        async fn send_message_command_rejects_self_send_before_any_network_io() {
            let app = guard_app(true);
            // `to` = our own npub. The identity is loaded, so the guard is reached with the real
            // `AppIdentity` the command reads from `SharedIdentity`.
            let own_npub = {
                let guard = app.state::<SharedIdentity>();
                let id = guard.read().await;
                id.as_ref().unwrap().npub()
            };
            let err = send_message_via_command(&app, &own_npub, "note to self")
                .await
                .unwrap_err();
            assert_eq!(err, "You can't send a message to yourself.");
        }
    }
}
