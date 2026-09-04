//! QURATOR-137 slice 3 — the auto-approve loop (owner ruling 2026-08-31, option B); Carrier-4
//! auto re-serve added 2026-09-04 (QURATOR-164, owner rulings below).
//!
//! This is the Rust half of "manifest on demand": once the owner has approved a peer once, a later
//! request-DM from that peer is answered **without a human click** — a fresh ticket is minted and
//! DM'd, exactly as the "Send the full list" click does, so the asker's fetch proceeds without a
//! round trip the owner already paid. An AUTHOR-BEARING ask (a Carrier-4 re-serve) is answered
//! the same way with NO prior approval at all: third-party serving is background infrastructure
//! (owner ruling 2026-09-04), so this node re-serves a cached copy to anyone who asks with the
//! `(author, slug, fingerprint)` triple only the author's own public teaser can have given them.
//!
//! Where the request is recognised today: production knows the `{"hb":"manifest_request",…}` DM
//! only at render time (`ui/src/lib/request-inbox.ts`, `parseManifestRequest`) — there was NO
//! production Rust handler for an incoming request-DM before this module. This loop is that seam.
//!
//! **The ticket contract is untouched.** Every approval this loop issues goes through a production
//! body — [`crate::commands::fulfil::send_full_list_inner`] for an own-collection ask (author
//! `None`), [`crate::commands::fulfil::send_cached_manifest_inner`] for an author-bearing
//! (Carrier-4 re-serve) ask — and each mints a FRESH ticket per fetch the same way the click does. Nothing here adds any serve-path check — there is none to modify
//! (QURATOR-177 Option E, owner ruling 2026-09-03: authorization is the standing grant this loop
//! itself checks, and the ticket is address delivery; a repeat fetch is a legitimate fetch, and a
//! peer whose fetch failed retries the ticket it already holds — immediately, unlimited, never
//! waiting on a cooldown).
//!
//! **What the caps are NOT:** they bound how often this node MINTS A NEW APPROVAL, never how long a
//! minted ticket lives. No ticket may ever expire (standing owner ruling); conflating the two would
//! smuggle a time-box back in through a side door.
//!
//! **What the caps are:** a mint-rate bound, in two dimensions —
//!   - per `(peer, author, slug)`: at most one auto-approval per 60 seconds;
//!   - globally: at most 32 auto-approvals per rolling 5 minutes.
//!
//! On exceeding either, the loop falls back to today's behaviour: the request-DM stays in the
//! inbox for the human, and the fallback is logged. **A rate limit that presents as a denial is
//! indistinguishable from being blocked**, so no error is ever surfaced to the peer — not an error
//! reply, not a refusal, nothing. The peer simply waits for the human, as it does today.
//!
//! ## What may be auto-approved
//!
//! The two paths have different authorisation, per the 2026-09-04 Carrier-4 rulings (QURATOR-164):
//!
//! * **Own-collection ask (`author_npub == None`)** — BOTH must hold:
//!   1. a standing grant exists for `(sender_npub, None, slug)` — the owner approved this exact
//!      triple before. The AUTHOR is part of the pace key (see `pace_key`): pacing over this
//!      node's own `films` is not a grant over some third party's `films`.
//!   2. the cap budget allows another approval for this pair (and globally).
//!
//! * **Author-bearing ask (`author_npub == Some(author)`, a Carrier-4 RE-SERVE ask)** — the grant
//!   requirement is WAIVED. The asker must already hold `(author, slug, fingerprint)` — a triple
//!   only the author's own public teaser can have given them — so there is no list to enumerate
//!   and no per-asker consent step left to ask for (the probe objection was refuted 2026-09-04).
//!   Third-party serving is background infrastructure: *"sharing third party should be background
//!   behavior - clients essentially become cognizant infrastructure nodes. Data passes through
//!   them on the way to the recipient, they dont need to auth it. They just need to pass it on."*
//!   Only the caps still bind. **The serving body is `send_cached_manifest_inner`, NEVER
//!   `send_full_list_inner`**: the own-collection body builds THIS node's collection by slug, so a
//!   common-slug collision ("films", "music") would serve the wrong collection — the author is
//!   load-bearing in the key, and that mis-route is exactly what the deleted step (0) used to
//!   guard against.
//!
//! A live-standing check used to sit between them, requiring `ContactStanding::Good` (re-read
//! every request and again at redeem). It was withdrawn by owner ruling 2026-09-03, QURATOR-177
//! (*"Blocks should only block interaction i.e. chats, it should not meaningfully affect other
//! traffic."*): blocking gates chat/DM interaction only — never the approval mint and never the
//! serve. Grants are permanent (owner ruling 2026-09-03), so for a granted pair there is
//! deliberately no second veto left to re-introduce.
//!
//! A request that fails its checks is not an error — it is today's behaviour: the DM stays for
//! the human. The loop never creates contacts (the WAN harness's `save_asker_contact` is
//! harness-only policy, deliberately not carried over), never writes an ask record, and never
//! answers a request for a collection it cannot build a manifest for — private collections are
//! refused by construction inside `build_slug_manifest`, pinned by
//! `build_slug_manifest_refuses_a_private_collection` (`commands/collection.rs`), so no redundant
//! fence stands here (adding one would imply the constructor alone is not the boundary, which it
//! is).
//!
//! ## The double-approval question
//!
//! If the loop auto-approves and the human then clicks "Send the full list" on the still-visible
//! card, two tickets get minted for one ask. That is the SAME outcome as a human clicking twice
//! today — one ticket per click, each independently consumable at most once, each recorded before
//! its DM — so it is safe by the existing per-ticket mechanics rather than by anything this module
//! adds. Coordinating the two (e.g. dismissing the card once auto-approved) is UI work owed to a
//! follow-up, not a defect in this seam: an extra ticket is inert, costs one DM, and changes no
//! authorisation (QURATOR-177 Option E: there is no spent bit for a second one to collide with —
//! both would fetch, and that is legitimate). This is recorded here rather than silently ignored.
//!
//! ## Locking
//!
//! This loop binds an endpoint and sends DMs, so it must never run inside a critical section that
//! forbids relay I/O. It takes no `DM_CACHE_LOCK` and no `DM_REQUESTS_LOCK`; it does not touch the
//! DM cache at all — it fetches gift-wraps on its own short-lived client (the WAN harness loop's
//! shape), decodes them with `decode_dms`, and never persists a DM-side effect. The relay fetch,
//! endpoint bind, and ticket DM all happen at this loop's own top level, outside any lock.
//!
//! ## Why a background task
//!
//! Spawned from `spawn_background_tasks`, so it keeps running while the window is minimised or
//! hidden to tray — which is the point of option B: a peer's request is answered whether or not
//! the owner is looking at the app.

use std::collections::HashSet;
use std::time::Duration;

use nostr::prelude::*;

use crate::commands::chat::decode_dms;
use crate::commands::fulfil::{send_cached_manifest_inner, send_full_list_inner};
use crate::identity_state::SharedIdentity;
use crate::net::{self, SharedRelay};
use crate::store::DataStore;
use crate::transport_state::SharedEndpoint;

/// One auto-approval per `(peer, author, slug)` per this many seconds (owner-ruled).
///
/// Gates MINTING A NEW APPROVAL ONLY — never the retry path. A failed transfer is simply
/// undelivered (QURATOR-177 Option E: there is no spent bit — `into_consumed` and the receipt it
/// required are deleted with the ledger), and a peer whose fetch failed retries the ticket it
/// already holds immediately and without limit; this cooldown never makes a retrying peer wait.
const AUTO_APPROVE_PER_PAIR_COOLDOWN_SECS: u64 = 60;

/// At most this many auto-approvals per rolling [`AUTO_APPROVE_GLOBAL_WINDOW_SECS`] (owner-ruled).
const AUTO_APPROVE_GLOBAL_MAX: usize = 32;

/// The rolling window for the global cap.
const AUTO_APPROVE_GLOBAL_WINDOW_SECS: u64 = 5 * 60;

/// How often the loop polls the DM inbox for request-DMs. The WAN harness loop's cadence.
const AUTO_APPROVE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long one relay fetch may take before the poll gives up and retries next tick.
const AUTO_APPROVE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// The in-memory rate-limit state. **Process lifetime, deliberately** (owner-ruled "in-memory
/// counters"): a restart clears the cooldowns and the window, which is fine — the caps exist to
/// stop a runaway mint loop within one process, not to be a durable throttle. The grants
/// themselves live in the store; nothing durable is rate-limited here.
#[derive(Default)]
struct AutoApproveCaps {
    /// `(peer, author, slug)` key → unix secs of the last auto-approval for that triple.
    /// A backwards clock jump can only shorten a cooldown, never extend one, because each entry is
    /// compared against the same wall clock that wrote it.
    per_pair: std::collections::HashMap<String, u64>,
    /// Unix secs of every auto-approval in the current rolling window, oldest first. Pruned by
    /// popping entries older than the window on every check.
    global: std::collections::VecDeque<u64>,
}

impl AutoApproveCaps {
    /// Whether a new auto-approval may be minted for `pair_key` at `now`. **A pure decision** — it
    /// records nothing; the caller commits via [`record`](Self::record) only after the approval
    /// succeeded, so a failed approval body (`send_full_list_inner` or
    /// `send_cached_manifest_inner`) spends no budget.
    fn allows(&mut self, pair_key: &str, now: u64) -> bool {
        // Prune first so the window check below is against the live set, not history.
        let cutoff = now.saturating_sub(AUTO_APPROVE_GLOBAL_WINDOW_SECS);
        while let Some(&oldest) = self.global.front() {
            if oldest <= cutoff {
                self.global.pop_front();
            } else {
                break;
            }
        }
        if self.global.len() >= AUTO_APPROVE_GLOBAL_MAX {
            return false;
        }
        match self.per_pair.get(pair_key) {
            Some(&last) => now.saturating_sub(last) >= AUTO_APPROVE_PER_PAIR_COOLDOWN_SECS,
            None => true,
        }
    }

    /// Commit one minted auto-approval. Only called after whichever approval body ran returned Ok.
    fn record(&mut self, pair_key: &str, now: u64) {
        self.per_pair.insert(pair_key.to_string(), now);
        self.global.push_back(now);
    }
}

/// A parsed `{"hb":"manifest_request",…}` DM body — the Rust counterpart of the TS
/// `parseManifestRequest` (`request-inbox.ts`). The wire shape is `wire_freeze`-pinned on the TS
/// side, and production still has exactly one consumer: this loop.
///
/// **Shared `pub(crate)` with the WAN carry suite** (`wan_it/suite_wan_carry.rs`, QURATOR-183) —
/// the type, [`Self::parse`], and the `slug`/`ask_nonce` fields. A harness that re-implements this
/// parse covers its own copy, not the code that ships: that is the defect class behind
/// `sanitize_node_addr` (2026-08-27), `approve_request` (2026-09-01) and QURATOR-169. The blank-to-
/// `None` normalisation below is exactly what a hand-rolled field read gets wrong.
///
/// `author_npub` stays module-private on purpose: the harness reaches the author through
/// [`approval_body_for`], so it routes by production's decision rather than by reading the field
/// and deciding for itself.
///
/// `author_npub` is `None` when the field is absent **or an empty string** — the same
/// normalisation the TS parser performs (`typeof o.author_npub === 'string' && o.author_npub !== ''`),
/// so "present but blank" can never masquerade as a real author pin and mis-route the serve.
/// `None` means "the asked peer's own collection", the one convention
/// [`hb_core::TransportTicket::author_npub`] already gives `None`.
///
/// `Clone` because the auto-approve loop's hold queue owns a paced request across polls.
#[derive(Clone)]
pub(crate) struct ManifestRequestBody {
    pub(crate) slug: String,
    #[allow(dead_code)] // parsed for wire-shape parity with the TS parser; this loop has no use
    fingerprint_seen: Option<String>,
    pub(crate) ask_nonce: Option<String>,
    author_npub: Option<String>,
}

impl ManifestRequestBody {
    /// Parse one DM's content. `None` for an ordinary chat DM (any non-JSON / wrong-tag content),
    /// a non-object, a body missing its `hb` discriminator or its `slug` — the exact conditions
    /// the TS parser rejects on. `pub(crate)` so the WAN carry suite parses with THIS, not a copy.
    pub(crate) fn parse(content: &str) -> Option<Self> {
        let trimmed = content.trim();
        if !trimmed.starts_with('{') {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        // The wire discriminator FIRST: without it any JSON DM carrying a string `slug` (a chat
        // message pasting a listing, a future structured body) would enter the grant gate. Both
        // the TS parser (`o.hb !== 'manifest_request'`) and the WAN harness loop check this.
        if v.get("hb").and_then(|h| h.as_str()) != Some("manifest_request") {
            return None;
        }
        // `as_str()` on a non-string returns None, so a numeric `slug` is rejected here — the same
        // rejection `typeof o.slug !== 'string'` performs in TS.
        let slug = v.get("slug").and_then(|s| s.as_str())?.to_string();
        let str_field = |name: &str| -> Option<String> {
            match v.get(name).and_then(|f| f.as_str()) {
                // Blank normalises to absent (the TS parser's `!== ''` arms).
                Some(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            }
        };
        Some(Self {
            slug,
            fingerprint_seen: str_field("fingerprint_seen"),
            ask_nonce: str_field("ask_nonce"),
            author_npub: str_field("author_npub"),
        })
    }
}

/// The pacing half of the loop, extracted so it is testable without a relay: should this
/// request-DM, arriving from `sender_npub` at unix-seconds `now`, be served now or held?
///
/// **Every request is served eventually.** There is no approval and no refusal — only "now" or
/// "shortly". See the `PaceVerdict` doc below.
///
/// Returns the `pair_key` the caps will pace this ask under (the caller's rate-limit key — distinct
/// from the loop's dedup key, which additionally carries the nonce) so the decision and the
/// bookkeeping can never drift apart on what "this pair" means. For an own-collection ask that key
/// is the grant's own key (the grant is what authorised it); for an author-bearing ask it is the
/// same `standing_grant_key(sender, author, slug)` shape, computable with no grant existing — a
/// deliberate reuse, so an author-bearing triple is paced under the same key shape the store
/// writes. `None` means "leave it for the human" — which is a normal outcome, not an error, and is
/// never reported to the peer as anything.
fn pace_request(
    caps: &mut AutoApproveCaps,
    sender_npub: &str,
    body: &ManifestRequestBody,
    now: u64,
) -> PaceVerdict {
    // THERE IS NO APPROVAL STEP. Owner ruling 2026-09-04 (QURATOR-164): *"There's no approval
    // needed for public collections, thats why they are called public."*
    //
    // This is structural, not a policy choice that could drift back: the ONLY body this loop can
    // reach for an own-collection ask is `send_full_list_inner` → `build_slug_manifest`, which
    // REFUSES a private collection outright (`collection.rs`, pinned by
    // `build_slug_manifest_refuses_a_private_collection`). Private listings are sealed per
    // recipient through `priv_listing.rs` and never touch this path. So every ask this loop can
    // answer is for public bytes, and there is nothing left to authorise.
    //
    // ⚠ A `standing_grant_for` check stood here until 2026-09-04 and gated exactly that: a peer
    // asking for this node's OWN public collection fell to a human unless the owner had approved
    // them before. Do not restore it in any form. The grant concept is gone from the crate; see
    // the module doc for the three other jobs it was quietly doing and what replaced each.
    //
    // What DOES remain is pacing, and it never refuses — see `PaceVerdict`.
    let pair_key = pace_key(sender_npub, body.author_npub.as_deref(), &body.slug);
    match caps.allows(&pair_key, now) {
        true => PaceVerdict::ServeNow(pair_key),
        false => PaceVerdict::Defer,
    }
}

/// The pacing decision. **There is no refusal variant, deliberately.** With the approval deleted
/// there is no human card to fall back to, so a cap that dropped a request would be a silent
/// denial of service for public bytes — the "rate-limit-as-denial" failure the caps' own doc
/// forbids, and a contradiction of the ask-throttle ruling (*"it DELAYS, it never DISCARDS"*).
enum PaceVerdict {
    /// Serve it now; the payload is the caps key to commit against on success.
    ServeNow(String),
    /// Over budget this instant — hold it and re-decide on a later poll. Never dropped.
    Defer,
}

/// The caps key for a request. Formerly `store::standing_grant_key`, kept as a local helper when
/// the grant map was deleted: the caps still need a stable per-`(peer, author, slug)` identity,
/// which is a rate-limiting concern with no remaining connection to authorisation.
fn pace_key(sender_npub: &str, author_npub: Option<&str>, slug: &str) -> String {
    format!("{sender_npub}|{}|{slug}", author_npub.unwrap_or("self"))
}

/// Which production approval body an ask of this shape must be served by — the Carrier-4 routing
/// discriminator, extracted pure so the branch is testable without a relay. The loop's serve call
/// matches on this value, and its tracing derives from it, so the routing and the attributable
/// evidence can never drift apart.
pub(crate) enum ApprovalBody {
    /// An author-bearing (Carrier-4 re-serve) ask: serve the cached copy pinned to that author via
    /// [`crate::commands::fulfil::send_cached_manifest_inner`]. NEVER the own-collection body —
    /// common slugs ("films", "music") collide constantly and the author is load-bearing in the key.
    CachedManifest { author: String },
    /// An own-collection ask (`author_npub == None`): build THIS node's collection by slug via
    /// [`crate::commands::fulfil::send_full_list_inner`], exactly as before Carrier 4.
    FullList,
}

impl ApprovalBody {
    /// The tracing name — always the actual `fulfil` body this variant routes to.
    fn log_name(&self) -> &'static str {
        match self {
            ApprovalBody::CachedManifest { .. } => "send_cached_manifest_inner",
            ApprovalBody::FullList => "send_full_list_inner",
        }
    }
}

/// The one pure decision the serve branch consults: author-bearing ⇒ re-serve body, authorless ⇒
/// own-collection body. Pinned by `author_bearing_asks_route_to_the_cached_manifest_body`.
pub(crate) fn approval_body_for(body: &ManifestRequestBody) -> ApprovalBody {
    match body.author_npub.as_deref() {
        Some(author) => ApprovalBody::CachedManifest { author: author.to_string() },
        None => ApprovalBody::FullList,
    }
}

/// The loop itself. Runs forever; every decision is logged (info for approvals, debug/warn for the
/// human-fallback cases) so a real run produces evidence without spamming idle polls.
///
/// Modelled on the WAN harness's `run_auto_approve_loop` (`wan_it/mod.rs`), with the one deviation
/// that matters removed: the harness approves *any* asker because it has no human; this loop gates
/// an own-collection ask on a standing grant within cap budget, paces an author-bearing (Carrier-4
/// re-serve) ask on the caps alone (owner ruling 2026-09-04), and creates no contacts.
pub(crate) async fn run_auto_approve_loop(
    store: DataStore,
    live_npub: SharedIdentity,
    relay: SharedRelay,
    endpoint: SharedEndpoint,
) {
    use hb_net::RelayClient;

    // The identity is re-read EVERY poll rather than snapshotted once at spawn. Two reasons, both
    // production-only (the WAN harness snapshots once because it is single-identity by
    // construction): a fresh install has no identity when this loop starts — the first-run wizard
    // generates one later, and a one-shot snapshot would leave the loop dead for that whole
    // session; and a wiped/regenerated identity takes effect on the next poll instead of serving
    // as a stale one. The approval body separately re-reads the session secrets from the live
    // `SharedIdentity` inside `send_full_list_inner`, so no second copy of them is held here.
    //
    // In-memory, process-lifetime (see `AutoApproveCaps`).
    let mut caps = AutoApproveCaps::default();
    // Dedup by (sender, author, slug, nonce): a request-DM re-arrives on every poll (the relay
    // hands back the same wraps each time), so without this the loop would re-decide the same ask
    // forever. The per-pair cooldown covers most of it; this makes it exact.
    //
    // **Bounded on purpose.** The nonce is attacker-chosen, so a peer can mint unboundedly many
    // distinct dedup keys. Once the set is full it is CLEARED, not refused — the worst a flood
    // achieves is that already-seen request-DMs get re-decided, and every one of those decisions
    // runs the full grant + caps gate again (a granted pair then hits its 60 s
    // cooldown; an ungranted one is refused outright). A cache that protects memory by denying
    // service would be the rate-limit-as-denial failure the caps forbid.
    let mut seen_request_ids: HashSet<String> = HashSet::new();
    const SEEN_REQUESTS_MAX: usize = 4096;
    // Requests the caps paced away from this instant. They are RETRIED, never dropped: with the
    // approval deleted there is no human card to fall through to, so a dropped request would be a
    // silent denial of public bytes. The inbox cursor only moves forward, so a paced request would
    // never be re-fetched — it has to be held here or it is lost.
    let mut deferred: std::collections::VecDeque<(String, ManifestRequestBody)> =
        std::collections::VecDeque::new();
    // A ceiling on the hold queue, so a pathological burst cannot grow it without bound. Reaching
    // it is a loud warning, not a silent drop: the oldest is retried first, and the cap is far
    // above any rate the 1/sec ask throttle on the SENDING side can sustain.
    const DEFERRED_MAX: usize = 1024;
    // The inbox fetch cursor: newest outer gift-wrap `created_at` we have already fetched. 0 ⇒ one
    // full pull on the first poll (a cold cache), then `since`-bounded incremental reads — the
    // exact `dm_inbox_filter` shape `get_messages` uses, because without it this 5 s loop would
    // re-download and re-decrypt the ENTIRE DM history every poll, forever. Fine for a
    // minutes-long harness run; not fine for a loop that runs for days (relay citizenship, the
    // M16 ruling). In-memory like the caps: a restart pays one full pull again, which is
    // `get_messages`' own cold-cache cost. **Bandwidth-only, never a correctness boundary** —
    // dedup is by wrap id in `decode_dms` and by dedup key here, never this timestamp — and the
    // advance is clamped to `now` so an attacker-chosen future `created_at` (NIP-59's outer stamp
    // is arbitrary) can never push `since` past the present and blind the loop.
    let mut newest_seen_outer: u64 = 0;

    tracing::info!(
        poll_secs = AUTO_APPROVE_POLL_INTERVAL.as_secs(),
        "auto-approve: loop started (own-collection asks: grant + caps; Carrier-4 re-serve asks: \
         caps only)"
    );
    loop {
        // The identity snapshot for THIS poll (see the note above on why per-poll, not once).
        let (identity, own_npub) = {
            let guard = live_npub.read().await;
            let Some(id) = guard.as_ref() else {
                // No identity yet (fresh install, before the wizard) or mid-wipe. Sleep and retry —
                // the wizard can generate one at any moment, and returning here would disable the
                // loop for the rest of the process.
                tokio::time::sleep(AUTO_APPROVE_POLL_INTERVAL).await;
                continue;
            };
            (id.identity.clone(), id.npub())
        };

        // One short-lived client per poll, as the WAN harness loop does: the persistent shared
        // client (`net::client`) belongs to the command surface, and this loop must not hold it
        // across its sleeps. Every failure below logs and retries on the next tick — an
        // unreachable relay is a transient condition, not a reason to stop serving a granted peer.
        let relays = net::relay_urls(&store);
        let now_outer = now_secs();
        // Bandwidth-only cursor (see the declaration above); the 48 h margin is `get_messages`'s
        // own `DM_FETCH_MARGIN_SECS` allowance for relay-side `since` boundary wobble.
        let since = newest_seen_outer
            .saturating_sub(48 * 60 * 60)
            .min(now_outer);
        let mut filter = Filter::new().kind(Kind::GiftWrap).pubkey(identity.public_key());
        if since > 0 {
            filter = filter.since(Timestamp::from(since));
        }
        let wraps = match RelayClient::connect(&identity, &relays, AUTO_APPROVE_FETCH_TIMEOUT).await {
            Ok(client) => {
                let fetched = client.fetch(filter, AUTO_APPROVE_FETCH_TIMEOUT).await;
                client.disconnect().await;
                match fetched {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::debug!("auto-approve: inbox fetch failed: {e}");
                        tokio::time::sleep(AUTO_APPROVE_POLL_INTERVAL).await;
                        continue;
                    }
                }
            }
            Err(e) => {
                tracing::debug!("auto-approve: relay connect failed: {e}");
                tokio::time::sleep(AUTO_APPROVE_POLL_INTERVAL).await;
                continue;
            }
        };
        if wraps.is_empty() {
            tokio::time::sleep(AUTO_APPROVE_POLL_INTERVAL).await;
            continue;
        }
        // Advance the cursor (clamped to now, never backwards) BEFORE any processing: even if
        // every wrap below fails to decode, they have been fetched and are covered by the dedup
        // set, so the next poll must not re-download them.
        let batch_newest = wraps.iter().map(|w| w.created_at.as_u64()).max().unwrap_or(0);
        newest_seen_outer = newest_seen_outer.max(batch_newest.min(now_outer));

        // Decode with no contact filter — there is no filter left to apply. A wrap not addressed to
        // us is skipped inside `decode_dms` (NIP-17 seal verification), so a stranger's gift-wrap
        // cannot forge a sender, and a stranger asking for public bytes is served like anyone else.
        let msgs = decode_dms(&own_npub, &identity, wraps, None).await;

        // Anything the caps held on an earlier poll goes FIRST — oldest first, so a paced request
        // cannot be starved by a steady arrival of new ones.
        let replays: Vec<(String, ManifestRequestBody)> = deferred.drain(..).collect();
        let fresh = msgs.into_iter().filter_map(|msg| {
            let body = ManifestRequestBody::parse(&msg.content)?; // not ours: an ordinary chat DM
            Some((msg.from, body))
        });
        // `is_replay` is load-bearing: a deferred request ALREADY passed the dedup check on the
        // poll that held it, so re-running it here would find its own key and `continue` — the
        // hold queue would silently eat every request it was built to protect.
        let queue: Vec<(String, ManifestRequestBody, bool)> = replays
            .into_iter()
            .map(|(f, b)| (f, b, true))
            .chain(fresh.map(|(f, b)| (f, b, false)))
            .collect();
        for (from, body, is_replay) in queue {
            if !is_replay {
                let dedup_key = format!(
                    "{}|{}|{}|{}",
                    from,
                    body.author_npub.as_deref().unwrap_or(""),
                    body.slug,
                    body.ask_nonce.as_deref().unwrap_or("")
                );
                if !seen_request_ids.insert(dedup_key) {
                    continue;
                }
            }
            if seen_request_ids.len() > SEEN_REQUESTS_MAX {
                seen_request_ids.clear();
                tracing::debug!(
                    "auto-approve: dedup set full ({SEEN_REQUESTS_MAX}) — cleared; a re-decided \
                     request is simply served again, which is harmless for public bytes"
                );
            }

            let now = now_secs();
            // Pace, never refuse. (The caps state needs no lock: this task alone owns it, and it is
            // only touched between awaits.)
            let pair_key = match pace_request(&mut caps, &from, &body, now) {
                PaceVerdict::ServeNow(k) => k,
                PaceVerdict::Defer => {
                    if deferred.len() >= DEFERRED_MAX {
                        tracing::warn!(
                            sender = %crate::logging::trunc_npub(&from),
                            slug = %body.slug,
                            held = deferred.len(),
                            "auto-approve: hold queue at DEFERRED_MAX — this request is dropped. \
                             That is a denial of public bytes and should not happen: the 1/sec ask \
                             throttle bounds the sending side far below this rate."
                        );
                    } else {
                        tracing::debug!(
                            sender = %crate::logging::trunc_npub(&from),
                            slug = %body.slug,
                            "auto-approve: paced — held for a later poll, not refused"
                        );
                        deferred.push_back((from.clone(), body.clone()));
                    }
                    continue;
                }
            };

            // The approval: one call to the production body — the same call the click makes, chosen
            // by ask shape. A FRESH ticket is minted per fetch; no serve-path check exists to touch
            // (QURATOR-177 Option E). Endpoint binding, grant-record-before-DM, grant refresh, and
            // the ticket DM all happen inside, outside every DM cache/request locks (see the module
            // doc). The endpoint handle is the app's MANAGED one (passed in at spawn):
            // `ensure_endpoint` reuses the session's single listening plane or binds it here,
            // exactly as the fulfil click's `State<SharedEndpoint>` does — never a second binding
            // of the same secret.
            //
            // The branch IS the Carrier-4 contract: an author-bearing ask is a RE-SERVE and must go
            // to `send_cached_manifest_inner` (the author-pinned cache read). Routing it to
            // `send_full_list_inner` would build THIS node's same-slug collection instead — the
            // mis-route the deleted step (0) existed to prevent, since common slugs ("films",
            // "music") collide constantly and the author is load-bearing in the key.
            // Names the body in every tracing line below and routes the serve call — the Carrier-4
            // discriminator, extracted pure so it is testable (see `approval_body_for`).
            let which_body = approval_body_for(&body);
            let send_result = match &which_body {
                ApprovalBody::CachedManifest { author } => {
                    send_cached_manifest_inner(
                        from.clone(),
                        author.clone(),
                        body.slug.clone(),
                        body.ask_nonce.clone(),
                        &live_npub,
                        &store,
                        &relay,
                        &endpoint,
                    )
                    .await
                }
                ApprovalBody::FullList => {
                    send_full_list_inner(
                        from.clone(),
                        body.slug.clone(),
                        body.ask_nonce.clone(),
                        &live_npub,
                        &store,
                        &relay,
                        &endpoint,
                    )
                    .await
                }
            };
            match send_result {
                Ok(()) => {
                    caps.record(&pair_key, now);
                    tracing::info!(
                        sender = %crate::logging::trunc_npub(&from),
                        slug = %body.slug,
                        which_body = which_body.log_name(),
                        "auto-approve: approved — fresh ticket minted, recorded, grant refreshed, \
                         and DM'd"
                    );
                }
                Err(e) => {
                    // No budget consumed: a failed body mints no ticket that reaches an asker's
                    // redeem path, and the human can still answer the card.
                    tracing::warn!(
                        sender = %crate::logging::trunc_npub(&from),
                        slug = %body.slug,
                        which_body = which_body.log_name(),
                        "auto-approve: request left for the human: {e}"
                    );
                }
            }
        }
        tokio::time::sleep(AUTO_APPROVE_POLL_INTERVAL).await;
    }
}

/// Unix seconds now — the same helper shape every module here carries.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::Identity;
    use nostr::prelude::ToBech32;


    fn npub_of(id: &Identity) -> String {
        id.public_key().to_bech32().unwrap()
    }


    fn body(slug: &str, author_npub: Option<&str>) -> ManifestRequestBody {
        ManifestRequestBody {
            slug: slug.to_string(),
            fingerprint_seen: None,
            ask_nonce: Some("n1".to_string()),
            author_npub: author_npub.map(|a| a.to_string()),
        }
    }


    // DELETED 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only block interaction i.e.
    // chats, it should not meaningfully affect other traffic."*): `blocked_contact_with_a_grant_
    // is_not_auto_approved` pinned the opposite of the ruling — that a blocked peer's grant was
    // vetoed at the auto-approve gate. It is replaced by the test directly below, which pins the
    // ruled behaviour: the grant alone authorises, blocking changes nothing here, and blocking's
    // remaining enforcement (chat/DM acceptance) stays pinned in `commands/chat.rs` by
    // `proactive_block_refuses_later_dms_and_unblock_restores_acceptance`.






    /// The routing discriminator the serve branch consults: an author-bearing ask is served by the
    /// CACHED-MANIFEST body (author-pinned re-serve), an authorless one by the FULL-LIST body
    /// (own-collection build). Routing the author-bearing shape to the full-list body is the
    /// mis-route the deleted step (0) existed to prevent — common slugs ("films", "music") collide
    /// constantly and the author is load-bearing in the key.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `approval_body_for`, swap the two
    /// match arms (`Some(author) => ApprovalBody::FullList` / `None` =>
    /// `ApprovalBody::CachedManifest { .. }`). Both assertions red (each ask is attributed to the
    /// wrong body).
    #[test]
    fn author_bearing_asks_route_to_the_cached_manifest_body() {
        let author_a = npub_of(&Identity::generate());
        match approval_body_for(&body("films", Some(&author_a))) {
            ApprovalBody::CachedManifest { author } => {
                assert_eq!(
                    author, author_a,
                    "the re-serve body must carry the ASKED author verbatim — it is what the \
                     envelope is pinned against"
                );
            }
            ApprovalBody::FullList => panic!(
                "an author-bearing ask routed to the FULL-LIST body — the slug-collision mis-route \
                 (this would serve this node's own same-named collection as the other author's)"
            ),
        }
        assert!(
            matches!(approval_body_for(&body("films", None)), ApprovalBody::FullList),
            "an authorless ask routes to the FULL-LIST (own-collection) body, exactly as before \
             Carrier 4"
        );
        // The tracing name is the actual `fulfil` body each variant routes to, so a real run's
        // evidence names the body that served each ask.
        assert_eq!(
            approval_body_for(&body("films", Some(&author_a))).log_name(),
            "send_cached_manifest_inner"
        );
        assert_eq!(approval_body_for(&body("films", None)).log_name(), "send_full_list_inner");
    }

    /// Blank `author_npub` normalises to `None` at PARSE time. This mattered for the deleted grant
    /// lookup, and it still matters for two live reasons: the SERVE ROUTING reads it
    /// (`approval_body_for` — a `Some("")` would route a plain own-collection ask into the
    /// Carrier-4 cached-manifest body and serve nothing) and the pace key would drift to
    /// `"{peer}||{slug}"`, splitting one peer's rate budget in two.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `ManifestRequestBody::parse`'s
    /// `str_field` closure, delete the `!s.is_empty()` guard (keep `Some(s) => Some(s.to_string())`).
    /// This test reds: `author_npub` becomes `Some("")` and both assertions below fail.
    #[test]
    fn a_blank_author_npub_normalises_to_none_at_parse_time() {
        let raw = r#"{"hb":"manifest_request","slug":"vault","ask_nonce":"n1","author_npub":""}"#;
        let parsed = ManifestRequestBody::parse(raw)
            .expect("precondition: a well-formed request body must parse");
        assert_eq!(
            parsed.slug, "vault",
            "precondition: the slug parsed — the body is recognised as a request"
        );
        assert_eq!(
            parsed.author_npub, None,
            "an empty author_npub must normalise to None, never Some(\"\")"
        );
        // Absent behaves the same.
        let absent =
            ManifestRequestBody::parse(r#"{"hb":"manifest_request","slug":"vault"}"#)
                .expect("a body without author_npub must parse");
        assert_eq!(absent.author_npub, None);
        // And through the two live consumers: the blank-author body must route to the
        // OWN-COLLECTION serve body, and must produce the same pace key an absent author does.
        assert_eq!(
            approval_body_for(&parsed).log_name(),
            "send_full_list_inner",
            "a blank author must route to the own-collection body, not the Carrier-4 re-serve one"
        );
        assert_eq!(
            pace_key("npub1peer", parsed.author_npub.as_deref(), &parsed.slug),
            pace_key("npub1peer", None, "vault"),
            "blank and absent must share one rate budget, not split it"
        );
    }

    /// The wire discriminator: a JSON DM that is NOT a manifest request (wrong or missing `hb`
    /// tag) must not parse as one — even when it carries a string `slug`. This is the fence that
    /// keeps an ordinary chat DM out of the grant gate entirely.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `ManifestRequestBody::parse`, delete
    /// the `if v.get("hb")... != Some("manifest_request") { return None; }` check. The
    /// `{"hb":"chat","slug":"vault"}` assertion reds.
    #[test]
    fn an_ordinary_json_dm_is_not_a_manifest_request() {
        // Wrong tag, carries a slug — the trap shape.
        assert!(
            ManifestRequestBody::parse(r#"{"hb":"chat","slug":"vault"}"#).is_none(),
            "a JSON DM with the wrong hb tag must not parse as a manifest request"
        );
        // No tag at all.
        assert!(
            ManifestRequestBody::parse(r#"{"slug":"vault"}"#).is_none(),
            "a JSON DM with no hb tag must not parse as a manifest request"
        );
        // Non-JSON chat text.
        assert!(ManifestRequestBody::parse("hello there").is_none());
        // And the real shape still parses — proving the refusals are the tag, not a broken fixture.
        let real = ManifestRequestBody::parse(r#"{"hb":"manifest_request","slug":"vault"}"#)
            .expect("the real wire shape must parse");
        assert_eq!(real.slug, "vault");
    }



    /// **THE headline contract after QURATOR-164: there is no approval.** A peer with no prior
    /// relationship of any kind — no grant (grants do not exist), not a contact — asking for this
    /// node's OWN collection is served. Owner ruling: *"There's no approval needed for public
    /// collections, thats why they are called public."*
    ///
    /// This test is the inversion of the deleted `no_grant_is_not_auto_approved`, which pinned the
    /// opposite. If it ever reds because something "refuses a stranger", that something is a
    /// reintroduced approval and must come out.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `pace_request`, add
    /// `if body.author_npub.is_none() { return PaceVerdict::Defer; }` as the first statement (an
    /// approval-shaped refusal of the own-collection path) → this test reds.
    #[test]
    fn a_stranger_asking_for_a_public_collection_is_served_with_no_approval() {
        let mut caps = AutoApproveCaps::default();
        let stranger = npub_of(&Identity::generate());
        // Deliberately NOT saved as a contact and holding nothing: a pure stranger.
        for (label, b) in [
            ("own-collection ask", body("vault", None)),
            ("carrier-4 re-serve ask", body("films", Some("npub1authorA"))),
        ] {
            assert!(
                matches!(pace_request(&mut caps, &stranger, &b, 1_700_000_500), PaceVerdict::ServeNow(_)),
                "{label}: a stranger must be served — public bytes need no approval"
            );
        }
    }

    /// The caps DELAY, they never DISCARD. `PaceVerdict` has no refusal variant by construction,
    /// so this pins the runtime half: once over budget the verdict is `Defer`, and once the
    /// cooldown expires the very same request is served.
    ///
    /// This matters more than it used to: with the approval deleted there is no human card to fall
    /// through to, so a dropped request would be a silent denial of public bytes.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `AutoApproveCaps::allows`, return
    /// `true` unconditionally → the `Defer` assertion reds. Separately, in `pace_request`, map the
    /// `false` arm to `PaceVerdict::ServeNow(pair_key)` → same assertion reds.
    #[test]
    fn an_over_budget_request_is_deferred_and_later_served_never_refused() {
        let mut caps = AutoApproveCaps::default();
        let peer = npub_of(&Identity::generate());
        let b = body("vault", None);

        let PaceVerdict::ServeNow(key) = pace_request(&mut caps, &peer, &b, 1_000) else {
            panic!("first ask must serve immediately");
        };
        caps.record(&key, 1_000);

        assert!(
            matches!(pace_request(&mut caps, &peer, &b, 1_030), PaceVerdict::Defer),
            "inside the per-pair cooldown the verdict must be Defer — held, not refused"
        );
        assert!(
            matches!(
                pace_request(&mut caps, &peer, &b, 1_000 + AUTO_APPROVE_PER_PAIR_COOLDOWN_SECS),
                PaceVerdict::ServeNow(_)
            ),
            "once the cooldown expires the SAME request must be served — a delay, not a drop"
        );
    }

    /// The global rolling window behaves the same way: defer at the ceiling, serve once the window
    /// slides. Pinned separately from the per-pair cooldown because they are different mechanisms
    /// and a single test could pass on either one alone.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `AutoApproveCaps::allows`, delete the
    /// `if self.global.len() >= AUTO_APPROVE_GLOBAL_MAX { return false; }` block → the Defer
    /// assertion reds while the per-pair test above stays green.
    #[test]
    fn the_global_window_defers_at_the_ceiling_then_serves_once_it_slides() {
        let mut caps = AutoApproveCaps::default();
        // Fill the window with DISTINCT pairs, so only the global cap can be what bites.
        for i in 0..AUTO_APPROVE_GLOBAL_MAX {
            caps.record(&pace_key(&format!("npub1peer{i}"), None, "vault"), 1_000);
        }
        let fresh = npub_of(&Identity::generate());
        assert!(
            matches!(pace_request(&mut caps, &fresh, &body("vault", None), 1_001), PaceVerdict::Defer),
            "at the global ceiling a brand-new pair must be DEFERRED, never refused"
        );
        assert!(
            matches!(
                pace_request(
                    &mut caps,
                    &fresh,
                    &body("vault", None),
                    1_000 + AUTO_APPROVE_GLOBAL_WINDOW_SECS + 1
                ),
                PaceVerdict::ServeNow(_)
            ),
            "once the window slides the held request must be served"
        );
    }

    /// The caps key must still separate an own-collection ask from a Carrier-4 re-serve of the same
    /// slug. It is now a RATE-LIMIT key, not a permission key — but the collision it prevents is
    /// unchanged: slugs like "films" collide across authors constantly, and pacing one must not
    /// pace the other.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `pace_key`, drop the author component
    /// (`format!("{sender_npub}|{slug}")`) → this test reds.
    #[test]
    fn the_pace_key_separates_own_collection_from_a_carrier4_reserve() {
        let peer = "npub1peer";
        assert_ne!(
            pace_key(peer, None, "films"),
            pace_key(peer, Some("npub1authorA"), "films"),
            "an own-collection ask and a re-serve of someone else's same-named collection must not \
             share a rate-limit budget"
        );
        assert_eq!(pace_key(peer, None, "films"), "npub1peer|self|films");
    }

}
