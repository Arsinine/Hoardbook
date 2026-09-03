//! QURATOR-137 slice 3 — the auto-approve loop (owner ruling 2026-08-31, option B).
//!
//! This is the Rust half of "manifest on demand": once the owner has approved a peer once, a later
//! request-DM from that peer is answered **without a human click** — a fresh ticket is minted and
//! DM'd, exactly as the "Send the full list" click does, so the asker's fetch proceeds without a
//! round trip the owner already paid.
//!
//! Where the request is recognised today: production knows the `{"hb":"manifest_request",…}` DM
//! only at render time (`ui/src/lib/request-inbox.ts`, `parseManifestRequest`) — there was NO
//! production Rust handler for an incoming request-DM before this module. This loop is that seam.
//!
//! **The ticket contract is untouched.** Every approval this loop issues goes through
//! [`crate::commands::fulfil::send_full_list_inner`], which mints a FRESH ticket per fetch the
//! same way the click does. Nothing here adds any serve-path check — there is none to modify
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
//! Both must hold, checked in this order (cheapest and most-discriminating first):
//!   1. a standing grant exists for `(sender_npub, author, slug)` — the owner approved this exact
//!      triple before. The AUTHOR is part of the key (see `standing_grant_key`): a grant over this
//!      node's own `films` is not a grant over some third party's `films`.
//!   2. the cap budget allows another approval for this pair (and globally).
//!
//! A live-standing check used to sit between them, requiring `ContactStanding::Good` (re-read
//! every request and again at redeem). It was withdrawn by owner ruling 2026-09-03, QURATOR-177
//! (*"Blocks should only block interaction i.e. chats, it should not meaningfully affect other
//! traffic."*): blocking gates chat/DM interaction only — never the approval mint and never the
//! serve. Grants are permanent (owner ruling 2026-09-03), so for a granted pair there is
//! deliberately no second veto left to re-introduce.
//!
//! A request that fails either check is not an error — it is today's behaviour: the DM stays for
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
use crate::commands::fulfil::send_full_list_inner;
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
    /// succeeded, so a failed `send_full_list_inner` spends no budget.
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

    /// Commit one minted auto-approval. Only called after `send_full_list_inner` returned Ok.
    fn record(&mut self, pair_key: &str, now: u64) {
        self.per_pair.insert(pair_key.to_string(), now);
        self.global.push_back(now);
    }
}

/// A parsed `{"hb":"manifest_request",…}` DM body — the Rust counterpart of the TS
/// `parseManifestRequest` (`request-inbox.ts`). Kept private to this module: production has
/// exactly one consumer (this loop), and the wire shape is `wire_freeze`-pinned on the TS side.
///
/// `author_npub` is `None` when the field is absent **or an empty string** — the same
/// normalisation the TS parser performs (`typeof o.author_npub === 'string' && o.author_npub !== ''`),
/// so "present but blank" can never masquerade as a real author pin and become a bogus grant-key
/// author. `None` means "the asked peer's own collection", the one convention
/// [`hb_core::TransportTicket::author_npub`] already gives `None`.
struct ManifestRequestBody {
    slug: String,
    #[allow(dead_code)] // parsed for wire-shape parity with the TS parser; this loop has no use
    fingerprint_seen: Option<String>,
    ask_nonce: Option<String>,
    author_npub: Option<String>,
}

impl ManifestRequestBody {
    /// Parse one DM's content. `None` for an ordinary chat DM (any non-JSON / wrong-tag content),
    /// a non-object, a body missing its `hb` discriminator or its `slug` — the exact conditions
    /// the TS parser rejects on.
    fn parse(content: &str) -> Option<Self> {
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

/// The decision half of the loop, extracted so it is testable without a relay: should this
/// request-DM, arriving from `sender_npub` at unix-seconds `now`, be auto-approved?
///
/// Returns the `pair_key` of the grant that authorised it (the caller's rate-limit key — distinct
/// from the loop's dedup key, which additionally carries the nonce) so the decision and the
/// bookkeeping can never drift apart on what "this pair" means. `None` means "leave it for the
/// human" — which is a normal outcome, not an error, and is never reported to the peer as anything.
fn should_auto_approve(
    store: &DataStore,
    caps: &mut AutoApproveCaps,
    sender_npub: &str,
    body: &ManifestRequestBody,
    now: u64,
) -> Option<String> {
    // (0) Own-collection asks ONLY. `send_full_list_inner` — the one body this loop may call —
    // builds THIS node's collection by slug (`build_slug_manifest`). A request carrying an author
    // is a Carrier-4 RE-SERVE ask (peer D asking this node to re-serve peer A's collection from
    // cache), a different carrier with a different approval body (`send_cached_manifest_inner`)
    // that is still owed owner rulings (QURATOR-79): routing it through the own-collection body
    // would serve the WRONG collection whenever the slugs collide — and common slugs ("films",
    // "music") collide constantly, which is exactly why the author is load-bearing in the grant
    // key. Those asks fall to the human until Carrier 4 lands its own auto path.
    if body.author_npub.is_some() {
        return None;
    }

    // (1) The grant: the owner approved THIS (peer, author, slug) before. Blank-author was already
    // normalised to `None` at parse time, so the key carries "self", not "".
    store
        .standing_grant_for(sender_npub, body.author_npub.as_deref(), &body.slug)
        .ok()
        .flatten()?;

    // (2) The caps. Exhausted budget falls back to the human exactly like (1) does. (A live-standing
    // check used to sit here as step 2, requiring `ContactStanding::Good`; withdrawn by owner
    // ruling 2026-09-03, QURATOR-177 — blocking gates chat/DM interaction only. Do not re-add it:
    // with permanent grants it would be a silent second veto over an approval the owner already
    // gave, and it was never read-access revocation to begin with.)
    let pair_key = crate::store::standing_grant_key(sender_npub, body.author_npub.as_deref(), &body.slug);
    if !caps.allows(&pair_key, now) {
        return None;
    }
    Some(pair_key)
}

/// The loop itself. Runs forever; every decision is logged (info for approvals, debug/warn for the
/// human-fallback cases) so a real run produces evidence without spamming idle polls.
///
/// Modelled on the WAN harness's `run_auto_approve_loop` (`wan_it/mod.rs`), with the one deviation
/// that matters removed: the harness approves *any* asker because it has no human; this loop
/// approves only on a standing grant within cap budget, and creates no contacts.
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
        "auto-approve: loop started (grant + caps gate every approval)"
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

        // Decode with no contact filter — the grant + caps checks below are the filter. A wrap
        // not addressed to us is skipped inside `decode_dms` (NIP-17 seal verification), so a
        // stranger's gift-wrap cannot forge a sender.
        let msgs = decode_dms(&own_npub, &identity, wraps, None).await;
        for msg in msgs {
            let Some(body) = ManifestRequestBody::parse(&msg.content) else {
                continue; // an ordinary chat DM — the inbox UI's, not ours
            };
            let dedup_key = format!(
                "{}|{}|{}|{}",
                msg.from,
                body.author_npub.as_deref().unwrap_or(""),
                body.slug,
                body.ask_nonce.as_deref().unwrap_or("")
            );
            if !seen_request_ids.insert(dedup_key) {
                continue;
            }
            if seen_request_ids.len() > SEEN_REQUESTS_MAX {
                seen_request_ids.clear();
                tracing::debug!(
                    "auto-approve: dedup set full ({SEEN_REQUESTS_MAX}) — cleared; every \
                     re-decided request still runs the full grant+caps gate"
                );
            }

            let now = now_secs();
            // The decision. `None` is today's behaviour — the request stays for the human — logged
            // at debug so the fallback is observable without noise. (The caps state needs no lock:
            // this task alone owns it, and it is only touched between awaits.)
            let Some(pair_key) = should_auto_approve(&store, &mut caps, &msg.from, &body, now)
            else {
                tracing::debug!(
                    sender = %crate::logging::trunc_npub(&msg.from),
                    slug = %body.slug,
                    "auto-approve: left for the human (no grant / caps exhausted)"
                );
                continue;
            };

            // The approval: one call to the production body — the same call the click makes. A
            // FRESH ticket is minted per fetch; no serve-path check exists to touch (QURATOR-177
            // Option E). Endpoint binding, grant-record-before-DM, grant refresh, and the ticket DM
            // all happen inside, outside every DM cache/request lock (see the module doc). The
            // endpoint handle is the app's MANAGED one (passed in at spawn): `ensure_endpoint`
            // reuses the session's single listening plane or binds it here, exactly as the fulfil
            // click's `State<SharedEndpoint>` does — never a second binding of the same secret.
            match send_full_list_inner(
                msg.from.clone(),
                body.slug.clone(),
                body.ask_nonce.clone(),
                &live_npub,
                &store,
                &relay,
                &endpoint,
            )
            .await
            {
                Ok(()) => {
                    caps.record(&pair_key, now);
                    tracing::info!(
                        sender = %crate::logging::trunc_npub(&msg.from),
                        slug = %body.slug,
                        "auto-approve: approved via send_full_list_inner — fresh ticket minted, \
                         recorded, grant refreshed, and DM'd"
                    );
                }
                Err(e) => {
                    // No budget consumed: a failed `send_full_list_inner` mints no ticket that
                    // reaches an asker's redeem path, and the human can still answer the card.
                    tracing::warn!(
                        sender = %crate::logging::trunc_npub(&msg.from),
                        slug = %body.slug,
                        "auto-approve: send_full_list_inner failed — request left for the human: {e}"
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
    use crate::store::{CachedPeer, ContactSource};
    use hb_core::Identity;
    use nostr::prelude::ToBech32;

    /// Loud preconditions everywhere — never `if (!x) return`. A silently no-oping test was
    /// reported here as "passed for real", so every test first asserts the world it needs.
    fn store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path()).unwrap();
        (dir, store)
    }

    fn npub_of(id: &Identity) -> String {
        id.public_key().to_bech32().unwrap()
    }

    /// A saved contact — the realistic shape of a granted peer. Built through the real
    /// `save_contact` path so the test ends where production ends. (It used to assert the contact
    /// read as `ContactStanding::Good`; that vocabulary was deleted 2026-09-03, QURATOR-177, and
    /// the helper keeps its name because every caller still means "a peer we know".)
    fn save_contact_in_good_standing(store: &DataStore, npub: &str) {
        let peer = CachedPeer {
            npub: npub.to_string(),
            source: ContactSource::Manual,
            browse_key_hex: None,
            petname: Some("auto-approve-test".to_string()),
            profile: None,
            collections: vec![],
            listings_state: Default::default(),
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        };
        store
            .save_contact(&CachedPeer::pubkey_hash(npub), &peer)
            .expect("save_contact must succeed on a fresh store");
        assert!(
            store.load_contact(&CachedPeer::pubkey_hash(npub)).expect("contact read").is_some(),
            "precondition: the contact is really saved before the test asserts anything"
        );
    }

    fn body(slug: &str, author_npub: Option<&str>) -> ManifestRequestBody {
        ManifestRequestBody {
            slug: slug.to_string(),
            fingerprint_seen: None,
            ask_nonce: Some("n1".to_string()),
            author_npub: author_npub.map(|a| a.to_string()),
        }
    }

    /// The base case: a grant over the owner's OWN collection (author `None`), a good-standing
    /// peer, caps untouched. This is the one configuration production auto-approves.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `should_auto_approve`, delete the
    /// `store.standing_grant_for(...)` expression (the `?`-terminated statement). The grant check
    /// was the only thing refusing a stranger, so this test reds with
    /// `should_auto_approve returned None`.
    #[test]
    fn granted_peer_in_good_standing_is_auto_approved() {
        let (_dir, store) = store();
        let peer = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer);
        store
            .record_standing_grant(&peer, None, "vault", 1_700_000_000)
            .expect("precondition: grant write must succeed");
        // Loud precondition: the grant is really readable under the exact key slice 3 reads.
        assert!(
            store
                .standing_grant_for(&peer, None, "vault")
                .expect("grant read must succeed")
                .is_some(),
            "precondition: the grant must be in the map before asserting the decision"
        );

        let mut caps = AutoApproveCaps::default();
        let key = should_auto_approve(&store, &mut caps, &peer, &body("vault", None), 1_700_000_500)
            .expect("a granted, good-standing peer with fresh caps MUST auto-approve");
        assert_eq!(
            key,
            crate::store::standing_grant_key(&peer, None, "vault"),
            "the returned key must be the grant's own key"
        );
    }

    // DELETED 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only block interaction i.e.
    // chats, it should not meaningfully affect other traffic."*): `blocked_contact_with_a_grant_
    // is_not_auto_approved` pinned the opposite of the ruling — that a blocked peer's grant was
    // vetoed at the auto-approve gate. It is replaced by the test directly below, which pins the
    // ruled behaviour: the grant alone authorises, blocking changes nothing here, and blocking's
    // remaining enforcement (chat/DM acceptance) stays pinned in `commands/chat.rs` by
    // `proactive_block_refuses_later_dms_and_unblock_restores_acceptance`.

    /// **Blocking does not gate the auto-approve mint** (owner ruling 2026-09-03, QURATOR-177).
    /// A BLOCKED peer holding a standing grant is auto-approved exactly as an unblocked one is:
    /// the grant is permanent, the caps still bind independently, and blocking's enforcement is
    /// chat/DM interaction only. (A blocked peer cannot DELIVER a new ask over chat — that is
    /// chat gating, and it is `commands/chat.rs`'s to enforce — but this test pins the decision
    /// function itself, so a re-introduced standing veto anywhere in `should_auto_approve` reds
    /// it regardless of delivery.)
    ///
    /// MUTATION (P-10) — the orchestrator applies this and must see this test red: in
    /// `should_auto_approve` (this file), re-introduce a standing veto between steps (1) and (2),
    /// e.g. `if store.load_dm_blocked().map(|b| b.iter().any(|n| n == sender_npub)).unwrap_or(false)
    /// { return None; }`. The blocked-peer expectation reds (decision becomes None).
    #[test]
    fn a_blocked_peer_with_a_grant_is_still_auto_approved() {
        let (_dir, store) = store();
        let peer = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer);
        store
            .record_standing_grant(&peer, None, "vault", 1)
            .expect("precondition: grant write must succeed");
        store.save_dm_blocked(std::slice::from_ref(&peer)).unwrap();
        assert!(
            store.load_dm_blocked().unwrap().iter().any(|n| n == &peer),
            "precondition: the peer really is blocked before asserting the decision"
        );

        let mut caps = AutoApproveCaps::default();
        should_auto_approve(&store, &mut caps, &peer, &body("vault", None), 1_700_000_500)
            .expect("a blocked peer with a valid grant MUST still auto-approve (owner ruling 2026-09-03)");

        // The caps are the independent bound: exhausted budget still falls to the human, blocked
        // or not. Same rig as the caps tests below — record a use, ask again inside the window.
        let key = crate::store::standing_grant_key(&peer, None, "vault");
        caps.record(&key, 1_700_000_700);
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("vault", None), 1_700_000_710).is_none(),
            "caps bind a blocked peer exactly as they bind an unblocked one — independent of standing"
        );
    }

    /// No grant → not auto-approved. Today's behaviour preserved for every first ask: the human's
    /// click is still what creates the first grant.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `should_auto_approve`, replace
    /// `.ok().flatten()?` on the `standing_grant_for` chain with `.ok().flatten().or_else(|| Some(
    /// crate::store::StandingGrant { granted_at: 0 }))` — i.e. invent a grant when none exists.
    /// This test reds.
    #[test]
    fn no_grant_is_not_auto_approved() {
        let (_dir, store) = store();
        let peer = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer);
        assert!(
            store
                .standing_grant_for(&peer, None, "vault")
                .expect("grant read must succeed")
                .is_none(),
            "precondition: no grant exists — the peer is a contact but was never approved"
        );

        let mut caps = AutoApproveCaps::default();
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("vault", None), 1_700_000_500)
                .is_none(),
            "a good-standing contact with NO grant must be left for the human"
        );
    }

    /// Two refusals the author and slug are load-bearing for (QURATOR-137, `907f6ba`), plus the
    /// own-collection gate this loop adds:
    ///   - a grant over `films` must not answer an ask about `music` (different slug);
    ///   - a grant over authorA's `films` must not answer an own-collection ask about `films`
    ///     (same slug, different author — the collision the author-in-key exists for);
    ///   - an author-bearing ask NEVER auto-approves, grant or no grant: it is a Carrier-4 re-serve
    ///     ask with a different approval body (`send_cached_manifest_inner`, QURATOR-79 owed), and
    ///     routing it through the own-collection body would serve the wrong collection on slug
    ///     collision.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `should_auto_approve`, change the
    /// `standing_grant_for` call's second argument from `body.author_npub.as_deref()` to `None`
    /// (ignore the requested author). The different-slug assertion still holds (the slug still
    /// mismatches), but the "grant over authorA's films vs own films" one no longer reds on its own
    /// — that refusal is now primarily the (0) author gate; to red THIS test end to end, delete
    /// the `if body.author_npub.is_some() { return None; }` block instead. Both assertions above
    /// that depend on the key (different slug; author-bearing vs own) are covered between the two
    /// mutations.
    #[test]
    fn a_grant_for_a_different_author_or_slug_does_not_answer() {
        let (_dir, store) = store();
        let peer = npub_of(&Identity::generate());
        let author_a = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer);
        store
            .record_standing_grant(&peer, Some(&author_a), "films", 1)
            .expect("precondition: grant write must succeed");
        // A self-author grant over the SAME slug — the collision bait. It exists so the
        // own-collection refusal below is proven to be the AUTHOR, not the absence of any grant.
        store
            .record_standing_grant(&peer, None, "films", 1)
            .expect("precondition: grant write must succeed");
        assert!(
            store
                .standing_grant_for(&peer, Some(&author_a), "films")
                .expect("grant read must succeed")
                .is_some(),
            "precondition: the (authorA, films) grant is in the map"
        );
        assert!(
            store
                .standing_grant_for(&peer, None, "films")
                .expect("grant read must succeed")
                .is_some(),
            "precondition: the (self, films) grant is in the map — the bait the author gate refuses"
        );

        let mut caps = AutoApproveCaps::default();
        // Different slug, own collection — the slug half of the key.
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("music", None), 500).is_none(),
            "a grant over 'films' must not answer an ask about 'music'"
        );
        // Author-bearing ask, exact grant present — refused by the (0) own-collection gate. With
        // the self-author grant over the same slug in the map, only that gate can be refusing.
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("films", Some(&author_a)), 500)
                .is_none(),
            "an author-bearing (Carrier-4 re-serve) ask must never auto-approve through this loop"
        );
        // And the own-collection ask the self-author grant covers still approves — proving the two
        // refusals above are the key and the gate, not a broken fixture.
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("films", None), 500).is_some(),
            "the exact granted own-collection triple must auto-approve"
        );
    }

    /// Blank `author_npub` normalises to `None` at PARSE time, so the grant lookup carries "self"
    /// rather than a bogus "" author. Without this, `""` would probe `"{peer}||{slug}"` — a key
    /// nothing ever writes — and a granted peer's request would silently fall to the human; worse,
    /// the key shape would drift from `standing_grant_key`'s.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `ManifestRequestBody::parse`'s
    /// `str_field` closure, delete the `!s.is_empty()` guard (keep `Some(s) => Some(s.to_string())`).
    /// This test reds: `author_npub` becomes `Some("")` and the grant lookup misses.
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
        // And through the decision: a grant keyed on "self" (author None) answers a blank-author
        // request, which is the whole point of the normalisation.
        let (_d2, store) = store();
        let peer = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer);
        store
            .record_standing_grant(&peer, None, "vault", 1)
            .expect("precondition: grant write must succeed");
        let mut caps = AutoApproveCaps::default();
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &parsed, 1_700_000_500).is_some(),
            "a blank-author request must match the self-author grant exactly as an absent one does"
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

    /// The per-pair cooldown: a second immediate request for the same triple is refused — and the
    /// refusal is a FALLBACK (the decision returns `None`), never an error to the peer. After the
    /// cooldown elapses, the same request approves again.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `AutoApproveCaps::allows`, delete the
    /// `match self.per_pair.get(pair_key) { ... }` and replace with `true`. The `None`-at-+30s
    /// assertion reds.
    #[test]
    fn the_per_pair_cooldown_falls_back_without_erroring_and_then_expires() {
        let (_dir, store) = store();
        let peer = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer);
        store
            .record_standing_grant(&peer, None, "vault", 1)
            .expect("precondition: grant write must succeed");
        let mut caps = AutoApproveCaps::default();
        let t0: u64 = 1_700_000_000;
        let key =
            should_auto_approve(&store, &mut caps, &peer, &body("vault", None), t0)
                .expect("precondition: the first request must auto-approve");
        caps.record(&key, t0); // the loop records only on a successful send
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("vault", None), t0 + 30).is_none(),
            "a second request 30s in must FALL BACK to the human (None), not error"
        );
        assert!(
            should_auto_approve(&store, &mut caps, &peer, &body("vault", None), t0 + 60).is_some(),
            "the same request after the 60s cooldown must auto-approve again"
        );
    }

    /// The global cap: 32 auto-approvals in a rolling 5 minutes, the 33rd falls back — again as a
    /// `None` fallback, never an error — and a request that arrives AFTER the oldest mint leaves
    /// the window approves again.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `AutoApproveCaps::allows`, delete the
    /// `if self.global.len() >= AUTO_APPROVE_GLOBAL_MAX { return false; }` guard. The 33rd-pair
    /// assertion reds.
    #[test]
    fn the_global_cap_falls_back_without_erroring_and_then_expires() {
        let (_dir, store) = store();
        let mut caps = AutoApproveCaps::default();
        let t0: u64 = 1_700_000_000;
        // 32 DISTINCT granted pairs — distinct peers so the per-pair cooldown cannot be what
        // refuses the 33rd. Each is a real grant + real contact, driven through the decision, so
        // the test ends where production ends.
        for i in 0..AUTO_APPROVE_GLOBAL_MAX {
            let peer = npub_of(&Identity::generate());
            save_contact_in_good_standing(&store, &peer);
            store
                .record_standing_grant(&peer, None, "vault", 1)
                .expect("grant write must succeed");
            let key = should_auto_approve(&store, &mut caps, &peer, &body("vault", None), t0)
                .unwrap_or_else(|| panic!("pair {i} must auto-approve — the caps are not full yet"));
            caps.record(&key, t0);
        }
        // The 33rd, at a timestamp still inside the window and for a FRESH pair.
        let peer33 = npub_of(&Identity::generate());
        save_contact_in_good_standing(&store, &peer33);
        store
            .record_standing_grant(&peer33, None, "vault", 1)
            .expect("grant write must succeed");
        assert!(
            should_auto_approve(&store, &mut caps, &peer33, &body("vault", None), t0 + 60).is_none(),
            "the 33rd mint inside the window must FALL BACK to the human (None), not error"
        );
        // Once the oldest mint has left the rolling window, budget exists again.
        assert!(
            should_auto_approve(
                &store,
                &mut caps,
                &peer33,
                &body("vault", None),
                t0 + AUTO_APPROVE_GLOBAL_WINDOW_SECS + 1
            )
            .is_some(),
            "after the window rolls past the first 32 mints, the cap must open again"
        );
    }
}
