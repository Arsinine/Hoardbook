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
//! ## QURATOR-264 — an ask is remembered by its serve OUTCOME, never by its arrival
//!
//! The answered-ask memory is written only after the attempt: on success, or when the
//! [`SERVE_ATTEMPTS_MAX`] give-up bound declares the failure permanent. A failed serve leaves the
//! ask unremembered, and the retry mechanism is the ask's own relay re-delivery — the same wrap
//! re-arrives on every poll inside the fetch margin — so a transient failure (relay blip, endpoint
//! rebind) recovers with no local retry queue at all. The asker is NOT a retry mechanism: a
//! refetch fires only on an observed fingerprint change and there is no manual retrigger (owner
//! ruling 2026-09-03), so an ask marked answered before its serve is an ask silently lost. The
//! bound is what keeps "retry the transient ones" from becoming a self-inflicted flood: an ask
//! that can never succeed (no cached copy of the asked author+slug, a collection that no longer
//! builds) is retried a bounded number of times and then remembered — the serve bodies' `Err` is
//! a bare message that cannot itself distinguish transient from permanent.
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

use crate::commands::chat::{decode_dms, giftwrap_inbox_filter, DM_FETCH_MARGIN_SECS};
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
///
/// `fingerprint_seen` is DELIBERATELY not retained (QURATOR-245, resolved 2026-09-19). The wire
/// field is the ASKER's observation — what their copy of the author's public teaser showed THEM —
/// and it is consumed on the asker's side (their redeem path's snapshot-change detection), never
/// on ours: a re-serve serves the NEWEST cached copy for the ask's `(author, slug)` (QURATOR-177
/// Option E), and "do we hold this collection at all" is enforced where it belongs — inside the
/// serve body itself, by the `newest_cached_for` lookup `send_cached_manifest_inner` refuses
/// without (`commands/fulfil.rs`). A held-fingerprint pre-check here would refuse an asker whose
/// teaser lags our cached snapshot; that refusal can never resolve into a serve, so it is a drop
/// of public bytes — and a silence-unless-willing mechanism the 2026-09-04 probe ruling
/// ("C answers asks normally — no silence-unless-willing mechanism") explicitly rules out. Bodies
/// carrying the field still parse (pinned by `a_body_with_fingerprint_seen_still_parses`): the
/// TS parser and the wire shape are untouched, the field is simply ignored like any unretained
/// one.
#[derive(Clone)]
pub(crate) struct ManifestRequestBody {
    pub(crate) slug: String,
    pub(crate) ask_nonce: Option<String>,
    author_npub: Option<String>,
}

/// QURATOR-248 — length caps on the attacker-controlled string fields of a request body. The
/// answered-ask memory ([`SEEN_REQUESTS_MAX`]) is bounded by ENTRY COUNT, so unless every field's
/// length is bounded too, a peer controls the BYTES per entry and the count cap stops bounding
/// memory. These caps restore that: with all three fields bounded, the worst-case key is bounded
/// and the count cap is honest again.
///
/// Derivations — each cap sits ABOVE what production itself produces, because refusing a
/// legitimate ask is a worse failure than the memory defect this fixes:
/// - `slug`: no slug-length constant exists anywhere today (`hb_core::ticket::is_valid_slug` is
///   charset-only), so this is derived from what a slug IS — a directory name on the author's
///   disk. Linux `NAME_MAX` is 255 bytes; Windows allows 255 UTF-16 units, which is at most 765
///   UTF-8 bytes (three-byte BMP chars). 768 is that filesystem ceiling, rounded.
/// - `author_npub`: production mints 63 chars (bech32 npub, `npub_of`). `parse_recipient`
///   (`commands/chat.rs`), which the serve body applies downstream, also accepts hex pubkeys
///   (64) and `hbk1…` full share codes (65-byte payload → 114 chars) and canonicalises them, so
///   the cap covers every form that path accepts. The serve body's own parse remains the real
///   validator; this is the bound for the keys built BEFORE that parse runs (the dedup key and
///   the pace key).
/// - `ask_nonce`: production mints 32 hex chars (`new_ask_nonce`, 128 bits of randomness). 128
///   is 4× that — generous for any future mint shape. No downstream length validation exists;
///   this cap is the only bound.
const SLUG_MAX_BYTES: usize = 768;
const AUTHOR_NPUB_MAX_BYTES: usize = 128;
const ASK_NONCE_MAX_BYTES: usize = 128;

impl ManifestRequestBody {
    /// Parse one DM's content. `None` for an ordinary chat DM (any non-JSON / wrong-tag content),
    /// a non-object, a body missing its `hb` discriminator or its `slug` — the conditions the TS
    /// parser rejects on — and, Rust-only (QURATOR-248), any field over its length cap (the
    /// consts above). That one divergence is deliberate and one-directional (this parser is
    /// stricter): a body the TS parser accepts but this rejects is left for the human's request
    /// card, which is this module's standing fallback for anything the loop will not answer.
    /// `pub(crate)` so the WAN carry suite parses with THIS, not a copy.
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
        // QURATOR-248 — the slug length gate. BYTES, not chars: the dedup key this feeds is
        // byte-sized memory, and NAME_MAX (the derivation above) is itself a byte bound on Linux.
        if slug.len() > SLUG_MAX_BYTES {
            return None;
        }
        let str_field = |name: &str| -> Option<String> {
            match v.get(name).and_then(|f| f.as_str()) {
                // Blank normalises to absent (the TS parser's `!== ''` arms).
                Some(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            }
        };
        let ask_nonce = str_field("ask_nonce");
        let author_npub = str_field("author_npub");
        // QURATOR-248 — an over-long optional field rejects the BODY; it is never normalised to
        // absent. Blank already means "no author pin" and routes the ask to the own-collection
        // body, so silently treating over-long as blank would mis-route an author-bearing ask —
        // the exact Carrier-4 mis-route `approval_body_for` exists to prevent.
        if ask_nonce.as_deref().is_some_and(|s| s.len() > ASK_NONCE_MAX_BYTES)
            || author_npub.as_deref().is_some_and(|s| s.len() > AUTHOR_NPUB_MAX_BYTES)
        {
            return None;
        }
        Some(Self { slug, ask_nonce, author_npub })
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
/// Ceiling on the answered-ask memory.
///
/// ⚠ **Bounded on purpose, and the bound survived being made durable.** Every key carries an
/// attacker-chosen nonce, so a peer can mint unboundedly many distinct keys. Unbounded, the
/// persisted set would be a disk-growth vector a stranger can drive. Overflow CLEARS rather than
/// refuses — the worst a flood achieves is that already-answered asks get answered again, which is
/// harmless for public bytes and is exactly the behaviour that existed before persistence.
///
/// The bound is ENTRY COUNT, so it bounds MEMORY only because each key's BYTES are bounded too
/// (QURATOR-248): `ManifestRequestBody::parse` caps `slug`/`ask_nonce`/`author_npub` at
/// 768/128/128 bytes, and the fourth key component — the sender's npub — is fixed at 63 chars by
/// the NIP-17 seal, not chosen by the sender. Worst case one dedup key is
/// 63 + 3 separators + 768 + 128 + 128 ≈ 1.1 KB, so the whole set's ceiling is ≈ 4.5 MB
/// (4096 × key bytes + per-entry set overhead); a typical key (short slug, 32-char minted nonce,
/// no author) is ~100 bytes, i.e. the "few hundred KB" this cap has always been described as.
/// Before those parse caps the field lengths were attacker-chosen and NEITHER number held.
pub(crate) const SEEN_REQUESTS_MAX: usize = 4096;

/// QURATOR-264 — how many serve attempts one ask gets before the loop gives up and remembers it
/// as answered. Attempts are driven by the ask's own relay re-delivery (one per poll), so this is
/// a retry WINDOW: 36 × the 5 s poll ≈ three minutes — long enough for a relay restart, a network
/// switch or an endpoint rebind to clear; short enough that an ask which can never succeed costs
/// a bounded number of cheap attempts instead of one every poll forever. The serve bodies' `Err`
/// is a bare `String` that cannot distinguish transient from permanent (typing their failures is
/// a `commands/fulfil.rs` change outside this ticket), so the bound IS the discriminator: a
/// transient failure usually clears inside the window, and anything still failing after it is
/// treated as permanent. This is an attempt count, not a volume cap — it bounds nothing about how
/// much is served, only how long one ask is retried (the 2026-09-02 no-caps ruling is untouched).
const SERVE_ATTEMPTS_MAX: u32 = 36;

/// QURATOR-264 — the ceiling on the per-ask attempt counts, the [`SEEN_REQUESTS_MAX`] convention:
/// every key carries an attacker-chosen nonce, so a stranger can mint failing asks without
/// holding any valid triple, and an unbounded map would be a memory-growth vector they can drive.
/// Overflow CLEARS rather than refuses — clearing only ever GRANTS retries of public bytes, never
/// denies one, the same "a cache that protects memory by denying service is the
/// rate-limit-as-denial failure" rule.
const SERVE_ATTEMPT_KEYS_MAX: usize = 1024;

/// Remember that `key` has been answered. Returns **true when it was NEW**, i.e. the caller should
/// answer it; false when it is a repeat to skip.
///
/// Clears at the cap rather than refusing, for the reason on [`SEEN_REQUESTS_MAX`]. Pure so the
/// bound is testable without a loop, a relay or a clock.
pub(crate) fn remember_answered(seen: &mut HashSet<String>, key: String, max: usize) -> bool {
    let is_new = seen.insert(key);
    if seen.len() > max {
        seen.clear();
    }
    is_new
}

/// QURATOR-264 — the dedup key for one ask, single-sourced so the arrival consult, the hold
/// queue's duplicate check and the post-serve remember can never disagree on what "this ask" is.
/// Same shape the loop has always keyed on: sender | author | slug | ask_nonce. Every component
/// is attacker-influenced (the nonce wholly attacker-chosen), which is why both maps it keys are
/// capped ([`SEEN_REQUESTS_MAX`], [`SERVE_ATTEMPT_KEYS_MAX`]).
fn dedup_key_of(from: &str, body: &ManifestRequestBody) -> String {
    format!(
        "{}|{}|{}|{}",
        from,
        body.author_npub.as_deref().unwrap_or(""),
        body.slug,
        body.ask_nonce.as_deref().unwrap_or("")
    )
}

/// QURATOR-264 — the arrival consult: may this request be processed? A fresh arrival is skipped
/// when its ask is already in the answered memory; a held replay is NEVER consulted — the hold
/// queue's liveness must not depend on the memory's contents. In the pre-264 design that coupling
/// was real (the key was INSERTED at first sighting, so a held request's own key sat in the set
/// and the re-check ate it); with consult-only it cannot arise, and this guard keeps it
/// structurally impossible rather than merely currently-false.
fn may_process(seen: &HashSet<String>, dedup_key: &str, is_replay: bool) -> bool {
    is_replay || !seen.contains(dedup_key)
}

/// QURATOR-264 — what a serve attempt did, reduced to what the memory policy needs.
///
/// `Copy` because the caller both hands it to [`apply_serve_outcome`] and re-inspects it for the
/// give-up log line; a fieldless two-variant enum is a byte, so passing by value twice is free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ServeOutcome {
    /// The ticket was minted and DM'd: the ask is answered.
    Served,
    /// The body returned `Err`: nothing reached the asker.
    Failed,
}

/// QURATOR-264 — apply one serve attempt's outcome to the answered-ask memory. Returns `true`
/// when the ask is now REMEMBERED (the caller marks the memory dirty and drops any held copy);
/// `false` when it stays unremembered, so the ask's own relay re-delivery retries it on a later
/// poll.
///
/// `Served` remembers immediately — an answered ask must never be re-served. `Failed` counts the
/// attempt and remembers only at [`SERVE_ATTEMPTS_MAX`]: the give-up bound that keeps a
/// permanently-failing ask from being re-attempted every poll forever. Pure so the policy is
/// testable without a loop, a relay or a clock (the `pace_request` precedent).
fn apply_serve_outcome(
    seen: &mut HashSet<String>,
    attempts: &mut std::collections::HashMap<String, u32>,
    dedup_key: &str,
    outcome: ServeOutcome,
) -> bool {
    match outcome {
        ServeOutcome::Served => {
            attempts.remove(dedup_key);
            remember_answered(seen, dedup_key.to_string(), SEEN_REQUESTS_MAX);
            true
        }
        ServeOutcome::Failed => {
            let count = *attempts
                .entry(dedup_key.to_string())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            // The SEEN_REQUESTS_MAX convention, applied to the attempt map (see that const's doc).
            if attempts.len() > SERVE_ATTEMPT_KEYS_MAX {
                attempts.clear();
            }
            if count >= SERVE_ATTEMPTS_MAX {
                attempts.remove(dedup_key);
                remember_answered(seen, dedup_key.to_string(), SEEN_REQUESTS_MAX);
                true
            } else {
                false
            }
        }
    }
}

pub(crate) fn approval_body_for(body: &ManifestRequestBody) -> ApprovalBody {
    match body.author_npub.as_deref() {
        Some(author) => ApprovalBody::CachedManifest { author: author.to_string() },
        None => ApprovalBody::FullList,
    }
}

/// QURATOR-197 / QURATOR-297 — this loop's gift-wrap inbox filter: a thin delegate to the ONE
/// shared builder (`commands::chat::giftwrap_inbox_filter`), so the request inbox carries
/// STRUCTURALLY the same hardening as the chat inbox (`dm_inbox_filter`) and the ticket poll
/// (`ticket_inbox_filter`) — the budget and window convention can no longer drift between
/// hand-copies. The explicit `.limit()` keeps the fetch budget ours — without it the 5 s poll
/// leaves the response size to the relay's own default (strfry's `maxFilterLimit`; CWE-400) and
/// re-decrypts whatever comes back, every tick. The `since` handed in is ALREADY margined by the
/// caller (`auto_approve_inbox_since` applies it) — the shared builder
/// never subtracts it. `since == 0` (cold cursor) omits the window, so the first poll stays the
/// one full initial pull.
fn auto_approve_inbox_filter(me: PublicKey, since: u64) -> Filter {
    giftwrap_inbox_filter(me, since)
}

/// QURATOR-303 — the caller-side margin for this loop's fetch window: the arithmetic
/// `auto_approve_inbox_filter` deliberately leaves to its caller (the shared builder never
/// subtracts a margin — pinned by `auto_approve_inbox_filter_declares_a_fetch_budget`). The
/// payload is `ticket_inbox_since`'s window line (`fetch_driver.rs`): the shared
/// `DM_FETCH_MARGIN_SECS` wobble allowance for relay-side `since` boundary conditions, then the
/// clamp-to-now discipline. A cold cursor (`0`) stays `0` — the saturating floor is what keeps
/// the first poll the one full initial pull.
fn auto_approve_inbox_since(newest_seen_outer: u64, now: u64) -> u64 {
    newest_seen_outer.saturating_sub(DM_FETCH_MARGIN_SECS).min(now)
}

/// QURATOR-303 — the inbox cursor's advance after a fetched batch. `batch_newest` is
/// attacker-controlled — it is the max outer `created_at` of relay-fetched gift-wraps (NIP-59's
/// outer stamp is arbitrary), and the loop computes it BEFORE `decode_dms`, so even an
/// undecodable junk wrap sets it. The clamp to `now` is the load-bearing half: without it one
/// wrap stamped in the far future pushes `since` permanently past the present and blinds the
/// 5 s serve loop for the life of the process. The outer `.max` keeps the cursor monotonic —
/// never backwards.
fn advance_inbox_cursor(newest_seen_outer: u64, batch_newest: u64, now: u64) -> u64 {
    newest_seen_outer.max(batch_newest.min(now))
}

/// The loop itself. Runs forever; every decision is logged (info for approvals, debug/warn for the
/// human-fallback cases) so a real run produces evidence without spamming idle polls.
///
/// Originally modelled on the WAN harness's own auto-approve loop (deleted 2026-09-23 — the WAN
/// suites now drive THIS loop), with the one deviation that mattered removed: that copy approved
/// *any* asker because it had no human; this loop gates
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
    // QURATOR-184: LOADED from disk, not started empty. Before this, every app start re-fetched the
    // whole relay backlog and re-answered all of it — strfry retains those request-DMs, so the set
    // grows over a node's lifetime and each restart re-answered a bigger one. Owner ruling
    // 2026-09-06: persist the bounded dedup set.
    //
    // QURATOR-264: this set is now written ONLY by a serve outcome (`apply_serve_outcome`) —
    // success, or the SERVE_ATTEMPTS_MAX give-up bound — never at first sighting. Inserting at
    // sighting marked a failed serve as answered, so the ask was never retried (see
    // `apply_serve_outcome`).
    //
    // A failed load is an empty set, never an error: this is a memory, and losing it costs
    // redundant work rather than correctness.
    let mut seen_request_ids: HashSet<String> = store.load_answered_asks().unwrap_or_default();
    tracing::info!(
        remembered = seen_request_ids.len(),
        "auto-approve: loaded the answered-ask memory"
    );
    // Saved at most ONCE PER POLL, not per answered ask. The set is written whole, so a per-ask
    // save would rewrite the entire file for each of them — during a backlog replay that is one
    // full write per ask. Once per poll bounds it to one write per AUTO_APPROVE_POLL_INTERVAL.
    let mut seen_dirty = false;
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
    // QURATOR-264 — per-ask serve-ATTEMPT counts, in-memory like the caps. An unremembered failed
    // ask is retried by its own relay re-delivery (the wrap re-arrives every poll inside the fetch
    // margin), and this map is what stops that retry being forever: at SERVE_ATTEMPTS_MAX attempts
    // the ask is remembered as answered — terminal (see `apply_serve_outcome`). Cleared at
    // SERVE_ATTEMPT_KEYS_MAX, never refused, for the reason on that const.
    let mut serve_attempts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
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
        // own `DM_FETCH_MARGIN_SECS` allowance for relay-side `since` boundary wobble — the shared
        // const, not a copy of its value, so the two windows can't drift (QURATOR-197).
        let since = auto_approve_inbox_since(newest_seen_outer, now_outer);
        let filter = auto_approve_inbox_filter(identity.public_key(), since);
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
        let batch_newest = wraps.iter().map(|w| w.created_at.as_secs()).max().unwrap_or(0);
        newest_seen_outer = advance_inbox_cursor(newest_seen_outer, batch_newest, now_outer);

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
        // `is_replay` is load-bearing: a held request is the loop's OWN copy of an ask that has
        // not been answered, so it must reach `pace_request` whatever the answered memory holds —
        // `may_process` never consults the set for a replay. (Before QURATOR-264 the key was
        // inserted at first SIGHTING, so the re-check genuinely found the held request's own key
        // and ate it; with consult-only that coupling cannot arise, and the guard keeps it
        // structurally impossible.)
        let queue: Vec<(String, ManifestRequestBody, bool)> = replays
            .into_iter()
            .map(|(f, b)| (f, b, true))
            .chain(fresh.map(|(f, b)| (f, b, false)))
            .collect();
        for (from, body, is_replay) in queue {
            // QURATOR-264 — the key is computed for EVERY request, held replays included: a
            // replay's serve outcome must land under the same key as the sighting that first held
            // it, and it is what finally remembers the ask. Single-sourced in `dedup_key_of` so
            // the consult, the hold queue's duplicate check and the remember cannot drift.
            let dedup_key = dedup_key_of(&from, &body);

            // QURATOR-264 — CONSULT ONLY, never insert. `remember_answered` used to fire HERE,
            // before the serve was attempted, so a serve that failed left the ask marked answered
            // and it was never retried — the asker's genuine ask silently failed with no recovery
            // path (and the asker does not re-ask: a refetch fires only on an observed
            // fingerprint change, owner ruling 2026-09-03). The memory is now written only by
            // `apply_serve_outcome`, after the attempt.
            if !may_process(&seen_request_ids, &dedup_key, is_replay) {
                continue;
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
                             throttle bounds the sending side far below this rate. (QURATOR-264: an \
                             unremembered ask re-arrives next poll, so while the queue stays full \
                             this repeats as a deferral rather than a one-time loss.)"
                        );
                    } else if deferred.iter().any(|(f, b)| dedup_key_of(f, b) == dedup_key) {
                        // QURATOR-264 — push-if-absent. An unremembered ask's wrap re-arrives on
                        // every poll inside the fetch margin; without this check each re-arrival
                        // would queue a second copy of an ask already held, and the ask would be
                        // served once per copy.
                        tracing::debug!(
                            sender = %crate::logging::trunc_npub(&from),
                            slug = %body.slug,
                            "auto-approve: paced — already held, not queued twice"
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
            // QURATOR-264 — the ask is remembered HERE, on the attempt's own outcome, never
            // before it: `Served` writes the answered-ask memory (an answered ask must never be
            // re-served), `Failed` leaves it unwritten so the ask's own relay re-delivery retries
            // it on a later poll, until the give-up bound declares the failure permanent.
            let outcome = match send_result {
                Ok(()) => {
                    caps.record(&pair_key, now);
                    tracing::info!(
                        sender = %crate::logging::trunc_npub(&from),
                        slug = %body.slug,
                        which_body = which_body.log_name(),
                        "auto-approve: approved — fresh ticket minted, recorded, grant refreshed, \
                         and DM'd"
                    );
                    ServeOutcome::Served
                }
                Err(e) => {
                    // No budget consumed: a failed body mints no ticket that reaches an asker's
                    // redeem path, and it must not consume the answered-ask memory either
                    // (QURATOR-264) — the ask stays unremembered and its own relay re-delivery
                    // retries it on a later poll, until SERVE_ATTEMPTS_MAX attempts declare the
                    // failure permanent.
                    tracing::warn!(
                        sender = %crate::logging::trunc_npub(&from),
                        slug = %body.slug,
                        which_body = which_body.log_name(),
                        "auto-approve: serve attempt failed: {e} — the ask stays unremembered and \
                         is retried on a later poll (QURATOR-264)"
                    );
                    ServeOutcome::Failed
                }
            };
            if apply_serve_outcome(&mut seen_request_ids, &mut serve_attempts, &dedup_key, outcome)
            {
                seen_dirty = true;
                if matches!(outcome, ServeOutcome::Failed) {
                    tracing::warn!(
                        sender = %crate::logging::trunc_npub(&from),
                        slug = %body.slug,
                        attempts = SERVE_ATTEMPTS_MAX,
                        "auto-approve: give-up bound reached — the ask is now remembered as \
                         answered so it stops being retried. A failure this persistent behaves as \
                         permanent (no cached copy of that author+slug, a collection that no \
                         longer builds); a transient one has outlasted the retry window."
                    );
                }
                // A remembered ask must leave the hold queue too: a held copy replays WITHOUT the
                // arrival consult (`may_process`), so it would otherwise re-serve an ask already
                // answered or given up on. The key match cannot eat a different ask — the nonce
                // and the author are part of the key (pinned by
                // `the_dedup_key_separates_asks_that_differ_only_by_nonce`).
                deferred.retain(|(f, b)| dedup_key_of(f, b) != dedup_key);
            }
        }
        if seen_dirty {
            // Best-effort: a failed write costs redundant work on the next start, never
            // correctness. Logged rather than propagated so a read-only or full disk cannot stop
            // the loop serving.
            match store.save_answered_asks(&seen_request_ids) {
                Ok(()) => seen_dirty = false,
                Err(e) => tracing::warn!(
                    error = %e,
                    "auto-approve: could not persist the answered-ask memory; the backlog may be \
                     re-answered after a restart"
                ),
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
    // Test-only (QURATOR-297): production's inbox filter now goes through
    // `giftwrap_inbox_filter`, so the budget const is no longer used by this file's non-test
    // code — importing it at module scope would be an `unused_imports` warning (a `-D warnings`
    // build failure) in the non-test compilation unit.
    use crate::commands::chat::DM_INBOX_FETCH_LIMIT;
    use hb_core::Identity;
    use nostr::prelude::ToBech32;
    use std::collections::HashMap;

    /// QURATOR-197 — the poll's inbox filter must carry an explicit fetch budget, mirroring
    /// `dm_inbox_filter`'s pin (audit #11, CWE-400): without `.limit()` the 5 s poll leaves the
    /// response size to the relay's own default and re-decrypts whatever comes back, every tick.
    ///
    /// MUTATION (P-10, re-pointed QURATOR-297) — remove `.limit(DM_INBOX_FETCH_LIMIT)` from the
    /// shared builder `giftwrap_inbox_filter` (`commands/chat.rs`; `auto_approve_inbox_filter`
    /// no longer carries the construction itself) and the first two asserts red
    /// (`None != Some(1000)`); remove the `since` arm there and the third reds. This is this
    /// consumer's share of the ONE cross-consumer mutation recorded beside the builder.
    #[test]
    fn auto_approve_inbox_filter_declares_a_fetch_budget() {
        let me = Identity::generate();
        let cold = auto_approve_inbox_filter(me.public_key(), 0);
        assert_eq!(
            cold.limit,
            Some(DM_INBOX_FETCH_LIMIT),
            "the cold-cursor filter declares a budget"
        );
        let warm = auto_approve_inbox_filter(me.public_key(), 1_700_000_000);
        assert_eq!(
            warm.limit,
            Some(DM_INBOX_FETCH_LIMIT),
            "the incremental filter too"
        );
        assert_eq!(
            warm.since.map(|s| s.as_secs()),
            Some(1_700_000_000),
            "a warm cursor bounds the fetch window"
        );
        assert!(cold.since.is_none(), "a cold cursor keeps the one full initial pull");
    }

    /// QURATOR-303 — THE pin for the cursor advance. `batch_newest` is the max outer `created_at`
    /// of relay-fetched gift-wraps — attacker-chosen (NIP-59's outer stamp is arbitrary) and set
    /// even by an undecodable junk wrap, because the loop computes it BEFORE `decode_dms` runs.
    /// Without the clamp-to-now inside `advance_inbox_cursor`, one wrap stamped in the far future
    /// pushes `since` permanently past the present and the 5 s serve loop never sees another ask
    /// for the life of the process.
    ///
    /// MUTATION (P-10, for the orchestrator to apply — resolve by LINE NUMBER, not text, since
    /// these fragments also appear in this comment): auto_approve.rs:567, the body of
    /// `advance_inbox_cursor` — delete the inner `.min(now)` call so the advance takes
    /// `batch_newest` raw, and the first assert reds (the 2099 stamp wins, cursor > now); change
    /// the outer `.max(` to `.min(` on the same line and the never-backwards assert reds (a
    /// stale batch drags the cursor down to 50).
    #[test]
    fn a_future_dated_wrap_cannot_poison_the_inbox_cursor() {
        let now = 1_800_000_000u64; // ≈ 2027-01-15, comfortably before the 2099 stamp below
        assert_eq!(
            advance_inbox_cursor(100, 4_070_908_800, now),
            now,
            "one wrap stamped 2099-01-01 clamps the advance to now, never past it"
        );
        assert_eq!(
            advance_inbox_cursor(1_700_000_000, 50, now),
            1_700_000_000,
            "a batch of stale wraps never drags the cursor backwards"
        );
        assert_eq!(
            advance_inbox_cursor(100, now - 10, now),
            now - 10,
            "an honest recent batch advances the cursor to itself"
        );
    }

    /// QURATOR-303 — the margin application, previously the only one of the three (chat's
    /// `dm_inbox_filter` call site, `ticket_inbox_since`, this one) with no pin: exactly ONE
    /// `DM_FETCH_MARGIN_SECS` subtraction on a warm cursor, and a cold `0` cursor stays `0` —
    /// the saturating floor, no underflow, so the first poll keeps its one full initial pull.
    ///
    /// MUTATION (P-10, resolve by LINE NUMBER): auto_approve.rs:556, the body of
    /// `auto_approve_inbox_since` — delete the `saturating_sub` term and the first assert reds
    /// (margin never applied); change `saturating_sub` to `wrapping_sub` on the same line and
    /// the cold-cursor assert reds (0 − margin wraps to a huge u64, not 0).
    #[test]
    fn auto_approve_inbox_since_applies_the_margin_exactly_once() {
        let now = 1_800_000_000u64;
        assert_eq!(
            auto_approve_inbox_since(now, now),
            now - DM_FETCH_MARGIN_SECS,
            "warm cursor: the wobble allowance is subtracted exactly once"
        );
        assert_eq!(
            auto_approve_inbox_since(0, now),
            0,
            "cold cursor: saturating floor, no underflow"
        );
        assert_eq!(
            auto_approve_inbox_since(DM_FETCH_MARGIN_SECS / 2, now),
            0,
            "a cursor younger than the margin floors at 0 too"
        );
    }

    /// QURATOR-303 — the clamp half of the window arithmetic: a cursor already past `now` (a
    /// hostile future stamp that predates the advance clamp, or a local clock that stepped back)
    /// must clamp down to `now`, so the fetch window can never open in the future. The cursor
    /// input is `now + margin + 100`, so the margin subtraction cannot mask the clamp.
    ///
    /// MUTATION (P-10, resolve by LINE NUMBER): auto_approve.rs:556, the body of
    /// `auto_approve_inbox_since` — delete the trailing `.min(now)` call and this reds (the
    /// assert wants `now`, gets `now + 100`).
    #[test]
    fn auto_approve_inbox_since_clamps_to_now() {
        let now = 1_800_000_000u64;
        assert_eq!(
            auto_approve_inbox_since(now + DM_FETCH_MARGIN_SECS + 100, now),
            now,
            "a cursor past now clamps down to now"
        );
    }

    fn npub_of(id: &Identity) -> String {
        id.public_key().to_bech32().unwrap()
    }

    /// MUTATION (P-10) — in `remember_answered`, change `let is_new = seen.insert(key);` to
    /// `let is_new = { seen.insert(key); true };` → this test reds (every ask reads as new, so the
    /// memory stops suppressing anything and the backlog is answered again on every poll).
    #[test]
    fn a_remembered_ask_is_not_answered_twice() {
        let mut seen = HashSet::new();
        assert!(remember_answered(&mut seen, "k".into(), SEEN_REQUESTS_MAX), "first time is new");
        assert!(
            !remember_answered(&mut seen, "k".into(), SEEN_REQUESTS_MAX),
            "the second sighting must be suppressed — that is the whole point of persisting it"
        );
    }

    /// The cap is what keeps a DURABLE memory from being a disk-growth vector: every key carries an
    /// attacker-chosen nonce, so a peer can mint unboundedly many. It must CLEAR, never refuse —
    /// refusing would turn a flood into a denial of public bytes.
    ///
    /// MUTATION (P-10) — in `remember_answered`, change `if seen.len() > max` to `if false` → this
    /// test reds on the size assert (the set grows without bound, on disk).
    #[test]
    fn the_memory_clears_at_the_cap_rather_than_refusing() {
        let mut seen = HashSet::new();
        let max = 8;
        for i in 0..=max {
            assert!(
                remember_answered(&mut seen, format!("k{i}"), max),
                "a fresh key is always new — the cap must never REFUSE, only forget"
            );
        }
        assert!(
            seen.len() <= max,
            "the set must stay bounded; unbounded, a peer with fresh nonces grows this file forever"
        );
    }


    fn body(slug: &str, author_npub: Option<&str>) -> ManifestRequestBody {
        ManifestRequestBody {
            slug: slug.to_string(),
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

    /// QURATOR-245 — `fingerprint_seen` is accepted on the wire and DELIBERATELY not retained by
    /// this parser. It is the ASKER's observation (what their teaser showed them), consumed on
    /// the asker's side; a re-serve serves the NEWEST cached copy for the ask's `(author, slug)`
    /// (QURATOR-177 Option E), and "do we hold this collection" is enforced inside the serve
    /// body (`send_cached_manifest_inner`'s `newest_cached_for` lookup, `commands/fulfil.rs`).
    /// Wiring the field into the approval would refuse an asker whose teaser lags our cached
    /// snapshot — a refusal that can never resolve into a serve, i.e. a drop of public bytes,
    /// and a silence-unless-willing mechanism the 2026-09-04 probe ruling rules out. This test
    /// pins the other half of the removal: the field's PRESENCE must never make a body fatal. A
    /// removal that accidentally rejected bodies carrying it would break every asker still
    /// sending it — the TS builder does, and that wire shape is frozen.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `ManifestRequestBody::parse`,
    /// immediately after the `slug` extraction (production line `let slug =
    /// v.get("slug").and_then(|s| s.as_str())?.to_string();`, line 240 on the post-QURATOR-245
    /// tree), insert `if v.get("fingerprint_seen").is_some() { return None; }` → the present and
    /// blank arms red (those bodies stop parsing); the absent arm stays green, proving the arms
    /// are genuinely distinguished. The same red fires under the subtler drift this guards
    /// against: switching the parser to strict typed deserialization with
    /// `#[serde(deny_unknown_fields)]`.
    #[test]
    fn a_body_with_fingerprint_seen_still_parses() {
        let present = ManifestRequestBody::parse(
            r#"{"hb":"manifest_request","slug":"films","fingerprint_seen":"fp-1"}"#,
        )
        .expect("a body carrying fingerprint_seen must parse — the wire shape is frozen");
        assert_eq!(present.slug, "films");
        let blank = ManifestRequestBody::parse(
            r#"{"hb":"manifest_request","slug":"films","fingerprint_seen":""}"#,
        )
        .expect("a blank fingerprint_seen is legal on the wire and must parse");
        assert_eq!(blank.slug, "films");
        let absent = ManifestRequestBody::parse(r#"{"hb":"manifest_request","slug":"films"}"#)
            .expect("an absent fingerprint_seen must parse");
        assert_eq!(absent.slug, "films");
        // The asker's fingerprint plays no role in the serve decision: all three wire shapes
        // route identically, because the type cannot carry the field at all. (Re-adding the
        // field also breaks this module at the `body()` helper's struct literal — compile-pinned.)
        for b in [&present, &blank, &absent] {
            assert_eq!(
                approval_body_for(b).log_name(),
                "send_full_list_inner",
                "the fingerprint an asker happened to see must not change the serve routing"
            );
        }
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

    /// QURATOR-248 — the answered-ask memory is bounded by ENTRY COUNT (`SEEN_REQUESTS_MAX`), so
    /// it bounds memory only if every attacker-controlled field of a key is length-bounded at
    /// parse. Each arm pairs the ACCEPT case (exactly at the cap) with the REJECT case (one byte
    /// over), so an off-by-one in either direction reds — and refusing a legitimate ask is the
    /// worse failure, which is why the accept edge is asserted first.
    ///
    /// MUTATION (P-10) — resolved by production line on the post-QURATOR-248 tree (numbering as
    /// of this lane's edit; both anchors sit above the poll loop, so QURATOR-264's loop
    /// restructuring does not move them): the slug gate is the `if slug.len() > SLUG_MAX_BYTES {`
    /// line (production line 286); replace `SLUG_MAX_BYTES` there with `usize::MAX` → the slug
    /// reject arm reds. The optional-field gate is the two-line
    /// `if ask_nonce.as_deref()… || author_npub.as_deref()…` condition (production lines 302-303);
    /// replace `ASK_NONCE_MAX_BYTES` and `AUTHOR_NPUB_MAX_BYTES` with `usize::MAX` → the
    /// ask_nonce and author_npub reject arms red. The at-cap parse asserts stay green under both
    /// mutations, proving the arms distinguish length, not a broken fixture.
    #[test]
    fn over_long_fields_reject_the_body_and_maximal_fields_still_parse() {
        let max_slug = "a".repeat(SLUG_MAX_BYTES);
        let max_nonce = "n".repeat(ASK_NONCE_MAX_BYTES);
        let max_author = "u".repeat(AUTHOR_NPUB_MAX_BYTES);
        // Every field EXACTLY at its cap: legal, and must parse (the off-by-one guard).
        let at_cap = format!(
            r#"{{"hb":"manifest_request","slug":"{max_slug}","ask_nonce":"{max_nonce}","author_npub":"{max_author}"}}"#
        );
        let parsed = ManifestRequestBody::parse(&at_cap)
            .expect("a body with every field exactly at its cap is legal and must parse");
        assert_eq!(parsed.slug, max_slug, "the at-cap slug survives verbatim");
        // ONE BYTE over, per field: the body is a malformed request. It must be rejected as a
        // whole — never normalised to absent (over-long author → absent would mis-route an
        // author-bearing ask to the own-collection body, the Carrier-4 mis-route).
        for (field, over) in [
            (
                "slug",
                format!(r#"{{"hb":"manifest_request","slug":"{}"}}"#, max_slug + "a"),
            ),
            (
                "ask_nonce",
                format!(
                    r#"{{"hb":"manifest_request","slug":"films","ask_nonce":"{}"}}"#,
                    max_nonce + "n"
                ),
            ),
            (
                "author_npub",
                format!(
                    r#"{{"hb":"manifest_request","slug":"films","author_npub":"{}"}}"#,
                    max_author + "u"
                ),
            ),
        ] {
            assert!(
                ManifestRequestBody::parse(&over).is_none(),
                "one byte over the {field} cap must reject the body — an over-long field is a \
                 malformed request, not an absent one"
            );
        }
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

    // ── QURATOR-264: the ask is remembered by its serve OUTCOME, never by its arrival ──
    //
    // The four-way taxonomy these tests pin, in one place:
    //   1. SERVED            → remembered (an answered ask must never be re-served);
    //   2. DEFERRED          → never remembered while held (the `is_replay` property);
    //   3. FAILED, transient → not remembered — the ask's own relay re-delivery retries it;
    //   4. FAILED, permanent → remembered at the SERVE_ATTEMPTS_MAX give-up bound.
    // The serve bodies' `Err` is a bare `String` and cannot say 3 from 4, so the BOUND is the
    // discriminator (see SERVE_ATTEMPTS_MAX's doc).

    /// QURATOR-264, the ticket's own regression: a serve that FAILS must not mark the ask
    /// answered. The memory used to be written at first SIGHTING, before the attempt, so a failed
    /// serve left the ask "answered" and it was never retried — and the asker is NOT a retry
    /// mechanism (a refetch fires only on an observed fingerprint change; there is no manual
    /// retrigger, owner ruling 2026-09-03), so the ask was silently lost. Pins taxonomy arm 3.
    ///
    /// MUTATION (P-10) — resolved by production line: `apply_serve_outcome`, `Failed` arm, the
    /// give-up condition `if count >= SERVE_ATTEMPTS_MAX {` at production line 516 → change to
    /// `if count >= 1 {` (the FIRST failure becomes terminal — the pre-264 defect restored). Both
    /// asserts red. (`a_permanently_failing_ask_is_remembered_at_the_give_up_bound` stays green
    /// under this mutation: its bound is still reached.)
    #[test]
    fn a_failed_serve_does_not_mark_the_ask_answered() {
        let mut seen = HashSet::new();
        let mut attempts = HashMap::new();
        let key = dedup_key_of("npub1peer", &body("vault", None));
        assert!(
            !apply_serve_outcome(&mut seen, &mut attempts, &key, ServeOutcome::Failed),
            "the first failure must NOT remember the ask — nothing was answered, so marking it \
             answered is the pre-264 defect"
        );
        assert!(
            !seen.contains(&key),
            "the answered-ask memory must stay empty for a failed serve"
        );
        assert!(
            may_process(&seen, &key, false),
            "the ask's own relay re-delivery (a fresh sighting on the next poll) must re-decide \
             it — that re-delivery IS the retry mechanism; no local queue is needed"
        );
    }

    /// QURATOR-264, the half the fix must not lose: a SUCCESSFUL serve remembers the ask. The
    /// wrap's re-delivery does not stop once the ask is answered (the fetch margin re-downloads
    /// it for 48 h), so without this the loop would re-serve the same ask every poll forever —
    /// the dedup memory is what prevents infinite re-processing. Pins taxonomy arm 1.
    ///
    /// MUTATION (P-10) — resolved by production line: `apply_serve_outcome`, `Served` arm, delete
    /// the `remember_answered(seen, dedup_key.to_string(), SEEN_REQUESTS_MAX);` line at
    /// production line 504 → the set never receives the key and the `seen.contains` +
    /// `may_process` asserts red.
    #[test]
    fn a_successful_serve_marks_the_ask_answered() {
        let mut seen = HashSet::new();
        let mut attempts = HashMap::new();
        let key = dedup_key_of("npub1peer", &body("vault", None));
        // A prior transient failure counted one attempt; success must clear it, not keep it.
        apply_serve_outcome(&mut seen, &mut attempts, &key, ServeOutcome::Failed);
        assert!(
            apply_serve_outcome(&mut seen, &mut attempts, &key, ServeOutcome::Served),
            "a served ask is remembered — the loop must set the dirty flag and drop held copies"
        );
        assert!(
            seen.contains(&key),
            "an answered ask is in the answered-ask memory"
        );
        assert!(
            !may_process(&seen, &key, false),
            "the wrap WILL re-arrive on later polls inside the fetch margin; the memory is what \
             stops it being re-served forever"
        );
        assert!(
            !attempts.contains_key(&key),
            "success clears the attempt count — a later same-key ask (after a memory clear) \
             starts from zero attempts, not from this ask's history"
        );
    }

    /// QURATOR-264 — the ANTI-INFINITE-LOOP guard, taxonomy arm 4: the failure the serve bodies
    /// most plausibly return is PERMANENT (`send_cached_manifest_inner` errs outright when
    /// `newest_cached_for` finds no cached copy of the asked (author, slug);
    /// `send_full_list_inner` errs when the slug no longer builds or the collection is private).
    /// Not-remembering failures without a bound would re-attempt such an ask on every poll
    /// forever — not "delay, never drop" but a self-inflicted flood, which the ask-throttle ruling
    /// cannot excuse (it paces OUR asks at 1/sec; it says nothing about re-serves).
    ///
    /// MUTATION (P-10) — resolved by production line: `apply_serve_outcome`, `Failed` arm, the
    /// give-up condition `if count >= SERVE_ATTEMPTS_MAX {` at production line 516 → change to
    /// `if false {`. The give-up never fires and the last three asserts red (the ask is never
    /// remembered and the attempt count is never released).
    #[test]
    fn a_permanently_failing_ask_is_remembered_at_the_give_up_bound() {
        let mut seen = HashSet::new();
        let mut attempts = HashMap::new();
        let key = dedup_key_of("npub1peer", &body("films", Some("npub1authorA")));
        for i in 1..SERVE_ATTEMPTS_MAX {
            assert!(
                !apply_serve_outcome(&mut seen, &mut attempts, &key, ServeOutcome::Failed),
                "attempt {i}: inside the retry window the ask must stay unremembered"
            );
        }
        assert!(
            apply_serve_outcome(&mut seen, &mut attempts, &key, ServeOutcome::Failed),
            "the SERVE_ATTEMPTS_MAX-th failure is terminal — the ask is remembered as answered"
        );
        assert!(
            seen.contains(&key),
            "a permanently-failing ask is never attempted again"
        );
        assert!(
            !attempts.contains_key(&key),
            "the give-up releases the attempt count — terminal asks must not accumulate state"
        );
        assert_eq!(attempts.len(), 0, "nothing lingers");
    }

    /// QURATOR-264 — the attempt map is keyed by attacker-chosen nonces (a stranger can mint
    /// failing asks without holding any valid triple: ask for a slug that does not exist), so it
    /// is capped like the answered-ask memory it feeds, and with the same convention: overflow
    /// CLEARS rather than refuses. Clearing only ever GRANTS retries of public bytes; refusing
    /// would be the rate-limit-as-denial failure the caps forbid.
    ///
    /// MUTATION (P-10) — resolved by production line: `apply_serve_outcome`, `Failed` arm, the
    /// cap check `if attempts.len() > SERVE_ATTEMPT_KEYS_MAX {` at production line 513 → change
    /// to `if false {`. The map grows past the cap and the size assert reds.
    #[test]
    fn the_attempt_memory_clears_at_its_cap_rather_than_refusing() {
        let mut seen = HashSet::new();
        let mut attempts = HashMap::new();
        for i in 0..=SERVE_ATTEMPT_KEYS_MAX {
            apply_serve_outcome(
                &mut seen,
                &mut attempts,
                &format!("npub1peer{i}|self|slug{i}|nonce{i}"),
                ServeOutcome::Failed,
            );
        }
        assert!(
            attempts.len() <= SERVE_ATTEMPT_KEYS_MAX,
            "the attempt map must stay bounded; unbounded, a stranger with fresh nonces grows it \
             forever by asking for slugs that do not exist"
        );
    }

    /// QURATOR-264 — taxonomy arm 2, the `is_replay` property must SURVIVE the consult-only
    /// rework: a deferred request replayed on a later poll is never eaten by the answered memory.
    /// The pin is deliberately hostile — it plants the key in the memory (any state that puts it
    /// there; the old insert-at-sighting design did exactly that on first sighting) and asserts
    /// the replay passes anyway — so the property holds by STRUCTURE (replays never consult, via
    /// `may_process`) and not by the current coincidence of states.
    ///
    /// MUTATION (P-10) — resolved by production line: `may_process`'s body
    /// `is_replay || !seen.contains(dedup_key)` at production line 471 → change to
    /// `!seen.contains(dedup_key)`. The replay consults, finds the planted key, and the first
    /// assert reds — the hold queue eating its own requests, the original defect of this seam.
    #[test]
    fn a_held_replay_is_never_eaten_by_the_answered_memory() {
        let mut seen = HashSet::new();
        let key = dedup_key_of("npub1peer", &body("vault", None));
        seen.insert(key.clone());
        assert!(
            may_process(&seen, &key, true),
            "a held replay never consults the answered memory — the hold queue's liveness must \
             not depend on the memory's contents"
        );
        assert!(
            !may_process(&seen, &key, false),
            "a fresh re-delivery of a remembered ask IS suppressed — that is the memory's job"
        );
        seen.remove(&key);
        assert!(
            may_process(&seen, &key, false),
            "with the key absent, a fresh sighting is processed"
        );
    }

    /// QURATOR-264 — what the dedup key separates, now that it single-sources the arrival
    /// consult, the hold-queue duplicate check AND the give-up purge. The purge
    /// (`deferred.retain` on the key, and the push-if-absent check) must never eat a DIFFERENT
    /// ask: an asker re-asking with a fresh nonce is a NEW ask, an own-collection ask is a
    /// different ask from a Carrier-4 re-serve of the same slug, and different askers never
    /// collide.
    ///
    /// MUTATION (P-10) — resolved by production line: in `dedup_key_of`'s `format!`, delete the
    /// `body.ask_nonce.as_deref().unwrap_or("")` argument (production line 460) and its
    /// `"{}|{}|{}"` counterpart separator → the nonce assert reds: two asks differing only by
    /// nonce collapse into one key, so a give-up on the first would silently eat the second.
    #[test]
    fn the_dedup_key_separates_asks_that_differ_only_by_nonce() {
        let ask = |nonce: &str, slug: &str, author: Option<&str>| ManifestRequestBody {
            slug: slug.to_string(),
            ask_nonce: Some(nonce.to_string()),
            author_npub: author.map(|a| a.to_string()),
        };
        let a = ask("n1", "films", Some("npub1authorA"));
        assert_eq!(
            dedup_key_of("npub1peer", &a),
            dedup_key_of("npub1peer", &ask("n1", "films", Some("npub1authorA"))),
            "the same ask always produces the same key"
        );
        assert_ne!(
            dedup_key_of("npub1peer", &a),
            dedup_key_of("npub1peer", &ask("n2", "films", Some("npub1authorA"))),
            "a re-ask with a fresh nonce is a NEW ask — a give-up purge on the old one must not \
             eat it"
        );
        assert_ne!(
            dedup_key_of("npub1peer", &a),
            dedup_key_of("npub1peer", &ask("n1", "films", None)),
            "an own-collection ask and a Carrier-4 re-serve of the same slug are different asks"
        );
        assert_ne!(
            dedup_key_of("npub1peer", &a),
            dedup_key_of("npub1other", &ask("n1", "films", Some("npub1authorA"))),
            "different askers are different asks"
        );
    }

}
