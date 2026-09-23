//! CARRY (QURATOR-178, harness half) — the 4-party Carrier-4 re-serve row over real relays.
//!
//! Topology (one process per party, driven by the operator in two phases):
//!
//! * **A — the author** (a JP VPS in the documented topology): seeds + publishes a collection,
//!   answers C's ordinary ask, then is **killed** by the operator. The kill is topology, not a row.
//! * **C — the cacher** (a NAS-attached AU host): phase 1 asks A and caches A's manifest; phase 2
//!   answers D's AUTHOR-ask by re-serving that cached copy while A is offline.
//! * **D — the asker** (the dev AU host): asks C for **A's** collection and redeems C's cached copy.
//! * **the decoy (QURATOR-178 assertion #3, opt-in)**: with `--decoy-seed-dir <dir>` on role C's
//!   PHASE 1, C ALSO seeds its OWN collection at the SAME slug (`wan-carry`, 7 files vs A's 24), so
//!   phase 2's re-serve runs with a genuine same-slug collision armed: the Carrier-4 resolution must
//!   key on (author_npub, slug) from C's manifest CACHE — the no-fallback fence in
//!   `manifest_source.rs`'s `payload()` — and never fall through to C's own same-slug draft. D is
//!   told the decoy is in play with the bare `--decoy-armed` flag, which renames the row (see
//!   `decoy_status_suffix`); the tree check itself runs armed or not.
//! * relays: the SG strfry relay (one `--relay` set shared by every invocation).
//!
//! The row proves Carrier 4 end to end: D imports A's manifest from C's cache while A is down, the
//! envelope still verifies under **A's** x-only key (not C's), and the provenance marks C as the
//! serving peer (`ImportedManifest.served_by == Some(C)`).
//!
//! ## Per-step production-function map (the anti-"harness re-implements the body" table)
//!
//! Every step that has a production function calls it. A step with no production function named
//! here is harness-side by design, and says so.
//!
//! | Role | Step | Production function |
//! |------|------|----------------------|
//! | A | seed the collection | `wan_it::seed_collection` → `crate::commands::collection::scan_selective` + `DataStore::save_collection_draft` |
//! | A | publish the teaser | `commands::collection::prepare_listing` + `hb_net::publish_listing_capped` (the `publish_e2e_teaser` composition, re-composed here because that helper hardcodes its slug) |
//! | A | answer C's ask | `wan_it::approve_request` → `commands::fulfil::send_full_list_inner` |
//! | C·1 | seed the decoy (opt-in `--decoy-seed-dir`) | `wan_it::seed_collection` — the SAME production add-collection path role A seeds through |
//! | C·1 | send the ask DM | `commands::chat::build_manifest_request` + `commands::chat::send_dm_inner` + `DataStore::record_manifest_ask` |
//! | C·1 | receive A's ticket | `commands::chat::decode_dms` + `hb_core::TransportTicket::verify_shape` |
//! | C·1 | redeem + cache | `commands::fulfil::redeem_manifest_ticket_inner` (claim + `ensure_endpoint` DialOnly + `fetch_manifest` + `commands::browse::accept_manifest_bytes` + spend) |
//! | C·2 | receive D's ask | `commands::chat::decode_dms` + `auto_approve::ManifestRequestBody::parse` + `auto_approve::approval_body_for` (production's parser AND its routing discriminator — QURATOR-183) |
//! | C·2 | answer by re-serving | `commands::fulfil::send_cached_manifest_inner` (cache read + `verify_author` + `ensure_endpoint` Listen + `issue_ticket` + `send_dm_inner`) |
//! | D | send the author-ask DM | `commands::chat::build_manifest_request_for_author` + `send_dm_inner` + `DataStore::record_manifest_ask` |
//! | D | receive C's ticket | `commands::chat::decode_dms` + `TransportTicket::verify_shape` (harness asserts `author_npub == Some(A)`) |
//! | D | redeem C's cached copy | `commands::fulfil::redeem_manifest_ticket_inner` |
//! | D | verify under A | `manifest_cache::get_latest` + `hb_core::ManifestEnvelope::verify_author` (read back from D's own cache, which `accept_manifest_bytes` wrote) |
//!
//! Harness-side by design (each mirrors the documented deviation in `wan_it/mod.rs` /
//! `suite_wan_e2e.rs`): nonce minting (`rand::random`, as in `send_request_dm`), the human decision
//! of WHICH ask C answers (a harness has no human; same deviation as `approve_request`). The
//! request-DM parse is NO LONGER harness-side — QURATOR-183 routed both roles through
//! `auto_approve::ManifestRequestBody::parse`, and C's re-serve decision through
//! `auto_approve::approval_body_for`, so neither can drift from production.
//!
//! ## Not a CI gate
//!
//! Same status as every `wan_it` suite: manual pre-release harness, never wired into CI.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use hb_core::TransportTicket;
use nostr::prelude::*;

use crate::commands::browse::ImportedManifest;
use crate::commands::chat::{parse_recipient, send_dm_inner};
use crate::commands::fulfil::{redeem_manifest_ticket_inner, send_cached_manifest_inner};
use crate::identity_state::{AppIdentity, SharedIdentity};
use crate::store::DataStore;
use crate::transport_state::new_shared_endpoint;
use crate::wan_it::tap::Tap;

/// The slug every role agree on. Small on purpose: the truncation paywall is WAN-E2E's concern,
/// not this row's — Carrier 4 needs a small manifest so the envelope fits one NIP-44 budget and
/// the row measures the re-serve, not split mechanics.
pub(crate) const CARRY_SLUG: &str = "wan-carry";

/// The AUTHOR's seed shape (role A's tree): 24 files at offset 0 — `file-0000.bin`…`file-0023.bin`,
/// exactly what `super::generate_seed_tree(dir, AUTHOR_SEED_FILES, AUTHOR_SEED_OFFSET)` writes.
/// Hoisted out of role A's call site (which passed the literals `24, 0` until QURATOR-178 #3) so
/// role D's colliding-slug check compares against the ONE source of that shape — and so
/// `the_decoy_discriminator_tells_the_authors_tree_from_the_cachers_own` can pin these consts to
/// independent literals (if the const feeds both sides of a check, no mutation of it can red).
const AUTHOR_SEED_FILES: usize = 24;
const AUTHOR_SEED_OFFSET: usize = 0;

/// The DECOY's seed shape (role C's OWN same-slug collection): 7 files at offset 1000 —
/// `file-1000.bin`…`file-1006.bin`. Deliberately a different COUNT *and* a disjoint NAME WINDOW
/// from the author's, so the count check and the name check discriminate independently: either
/// alone catches a decoy serve, and together they name an unexpected third tree as neither.
const DECOY_SEED_FILES: usize = 7;
const DECOY_SEED_OFFSET: usize = 1000;

const RELAY_TIMEOUT: Duration = Duration::from_secs(15);
const SETTLE: Duration = Duration::from_secs(3);
/// DM poll attempts (× [`SETTLE`] = the wait-for-DM window). 20 x 3s = a full minute — the same
/// patience `suite_wan_fetch`'s `DM_RETRIES` set the precedent for, for the reason its comment
/// names: the phases are sequenced BY HAND, here across THREE terminals on three hosts (fetch has
/// two), and the window must be wide enough for the operator to start the next role after this
/// one is already polling. At 6 attempts (~18s) role A polled out before C could even be started
/// on the other host (found live 2026-09-21, setting up the QURATOR-178 run).
const DM_POLL_RETRIES: usize = 20;
const REDEEM_TIMEOUT: Duration = Duration::from_secs(120);
const REDEEM_RETRIES: usize = 3;

/// Everything one party's process needs: its own identity + store + the shared relay set, plus the
/// raw CLI args (each role reads its own flags — `--author-npub`, `--asker-npub`, … — through
/// `super::args::flag_value`).
pub struct CarryInput {
    pub app_id: AppIdentity,
    pub store: DataStore,
    pub relays: Vec<String>,
    pub args: Vec<String>,
}

impl CarryInput {
    pub(super) fn flag<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        super::args::flag_value(&self.args, name)
    }

    /// Presence check for a BARE flag (one that carries no value). `flag` cannot express this: it
    /// returns whatever token FOLLOWS the name, which for a valueless flag is the next argument.
    pub(super) fn flag_present(&self, name: &str) -> bool {
        self.args.iter().any(|a| a == name)
    }

    pub(super) fn live_identity(&self) -> SharedIdentity {
        Arc::new(tokio::sync::RwLock::new(Some(
            AppIdentity {
                identity: self.app_id.identity.clone(),
                browse_key: self.app_id.browse_key.clone(),
                transport_key: self.app_id.transport_key.clone(),
            },
        )))
    }
}

/// Dispatch on `--role`. Each role runs its own rows; the operator sequences the phases by hand
/// (phase 1: A up, C asks; then kill A; phase 2: D asks C).
pub async fn run(tap: &mut Tap, role: &str, input: &CarryInput) {
    match role {
        "a" => {
            tap.check(
                "CA1: author seeds + publishes + answers the cacher's ask (own-collection serve)",
                run_role_a(input).await,
            );
        }
        "c" => {
            let phase = input.flag("--phase").unwrap_or("1").to_string();
            if phase == "1" {
                tap.check(
                    format!(
                        "CC1: cacher asks the author and caches A's manifest (ordinary redeem){}",
                        decoy_status_suffix(input.flag("--decoy-seed-dir").is_some())
                    ),
                    run_role_c_phase1(input).await,
                );
            } else {
                tap.check(
                    "CC2: cacher re-serves the CACHED copy to the asker while the author is offline",
                    run_role_c_phase2(input).await,
                );
            }
        }
        "d" => {
            tap.check(
                format!(
                    "CD1: asker asks the cacher for the AUTHOR's collection and redeems the cached \
                     copy{}",
                    decoy_status_suffix(input.flag_present("--decoy-armed"))
                ),
                run_role_d(input).await,
            );
        }
        other => {
            tap.check(
                format!("CARRY: unknown --role '{other}' (expected a|c|d)"),
                Err("unknown role".to_string()),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Shared legs — DM send (production builders + production gift-wrap send)
// ---------------------------------------------------------------------------

/// Mint the ask nonce harness-side (128 bits, hex) — the same shape `send_request_dm` in
/// suite_wan_e2e uses, because `commands::chat::new_ask_nonce` is private.
pub(super) fn mint_ask_nonce() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

/// Send an already-built request-DM body to `recipient` via the production NIP-17 send path, and
/// record the ask locally in production ordering (record AFTER the send resolves).
// 8 args, all load-bearing: the three npubs are genuinely distinct roles (DM recipient, the
// peer the ask is keyed under, and the AUTHOR whose collection it concerns — for a Carrier-4
// ask those differ), and collapsing them into a struct would hide exactly the distinction the
// key depends on.
#[allow(clippy::too_many_arguments)]
pub(super) async fn send_request_dm_to(
    input: &CarryInput,
    content: &str,
    recipient_npub: &str,
    asked_peer: &str,
    author_npub_for_key: &str,
    slug: &str,
    fingerprint_seen: &str,
    ask_nonce: &str,
) -> Result<(), String> {
    use hb_net::RelayClient;

    let recipient = parse_recipient(recipient_npub)
        .map_err(|e| format!("parse recipient npub: {e}"))?;
    let client = RelayClient::connect(&input.app_id.identity, &input.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("connect for request-DM: {e}"))?;
    send_dm_inner(
        &client,
        &input.app_id.identity,
        &recipient,
        content,
        &input.relays,
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("send_dm_inner (request-DM): {e}"))?;
    client.disconnect().await;

    // Production ordering: the ask trace exists only once the DM actually went out. The key is
    // (asked_peer, author, slug) — for a Carrier-4 ask the AUTHOR is A, not the DM sender C.
    //
    // ⚠ The slug is a PARAMETER, not `CARRY_SLUG`. It was hardcoded until 2026-09-06, which silently
    // broke every sibling suite that reused this helper: the fetch suite recorded its ask under
    // (A, A, "wan-carry") while its DM asked for "wan-fetch" and the returned ticket carried
    // "wan-fetch", so `claim_manifest_ask` looked up a key that did not exist and returned
    // `Unsolicited` — surfacing as "That link doesn't answer a request you sent" through three live
    // runs. `claim_manifest_ask` fails on EITHER a key miss or a nonce mismatch and reports both
    // identically, so the nonce is the tempting half to blame and the key is the one to check.
    let sent_at = chrono::Utc::now().to_rfc3339();
    input
        .store
        .record_manifest_ask(asked_peer, author_npub_for_key, slug, fingerprint_seen, &sent_at, ask_nonce)
        .map_err(|e| format!("record_manifest_ask: {e}"))?;
    Ok(())
}

/// What one selection pass over a poll attempt's decoded DMs found. Pure data: the live poll acts
/// on it, the unit test asserts it — same function, so the test ends where production ends.
struct NewestDmMatch<T> {
    /// The NEWEST DM `inspect` matched, with its `sent_at` (RFC3339, from the inner rumor — the
    /// real send time).
    newest: Option<(T, String)>,
    /// How many DMs matched this attempt — printed by the live poll when > 1, so an operator can
    /// see several matches being OUTRANKED rather than never counted.
    matched: usize,
}

/// Select the DM to act on from ONE poll attempt's decoded DMs: the **NEWEST** one `inspect`
/// matches, never the first.
///
/// ⚠ **Relay state outlives a run, and taking the first match is not idempotent** — the same race
/// `suite_wan_fetch`'s `poll_dms_newest` was fixed for on 2026-09-06 and WAN-E2E's
/// `select_ticket_dm_newest` for on 2026-09-21 (QURATOR-137): a failed or repeated phase leaves
/// earlier asks and tickets on the relay, addressed to the same npubs and still perfectly
/// decodable, so first-match hands a later poll a SUPERSEDED ask or ticket. Selection is on the
/// DM's `sent_at` (RFC3339 sorts lexicographically = chronologically, the same comparison
/// `poll_dms_newest` makes).
///
/// Generic on purpose, unlike E2E's ticket-specific version: carry's poll sites match three
/// different shapes (A a request-DM, C phase 2 an author-ask, C phase 1 and D a ticket), and
/// "newest match" is well-defined for all three — `sent_at` is the rumor's own send time, present
/// on every decoded DM. Pure by design — no network, no relay, no clock — so the selection the
/// live poll makes is exactly the selection a unit test can pin (a guard that rebuilds what it
/// checks is decorative).
fn select_dm_newest<T>(
    msgs: &[crate::commands::chat::ReceivedMessage],
    mut inspect: impl FnMut(&crate::commands::chat::ReceivedMessage) -> Option<T>,
) -> NewestDmMatch<T> {
    let mut pick = NewestDmMatch { newest: None, matched: 0 };
    for msg in msgs {
        // Not the shape this poll site wants — skipped entirely, never counted.
        let Some(found) = inspect(msg) else { continue };
        pick.matched += 1;
        // Newest by the DM's own send time: replace the incumbent only when strictly later, so
        // both inbox orders agree (the same guard E2E's selection makes).
        let is_newer = pick
            .newest
            .as_ref()
            .is_none_or(|(_, sent_at)| msg.sent_at > *sent_at);
        if is_newer {
            pick.newest = Some((found, msg.sent_at.clone()));
        }
    }
    pick
}

/// Poll this party's DM inbox via the production unwrap path (`decode_dms`) and hand every decoded
/// message from `expected_sender` to `inspect`; act on the **NEWEST** match ([`select_dm_newest`]),
/// never the first. Retries with a settle sleep; `inspect` returns `Some(T)` when a DM is the
/// shape this poll site wants (a ticket, a request).
pub(super) async fn poll_dms<T>(
    input: &CarryInput,
    expected_sender: &str,
    mut inspect: impl FnMut(&crate::commands::chat::ReceivedMessage) -> Option<T>,
    what: &str,
) -> Result<T, String> {
    use crate::commands::chat::decode_dms;
    use hb_net::RelayClient;

    let own_npub = input.app_id.npub();
    let mut last_err = String::from("no poll attempt ran");
    // Tracked so a failure can distinguish "nothing arrived" from "something arrived and was
    // filtered out" — see the error at the bottom. Without it both read as "no DM yet".
    let mut wraps_seen = 0usize;
    let mut decoded_seen = 0usize;
    for attempt in 1..=DM_POLL_RETRIES {
        let client = match RelayClient::connect(&input.app_id.identity, &input.relays, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("attempt {attempt}: connect: {e}");
                tokio::time::sleep(SETTLE).await;
                continue;
            }
        };
        let wraps = match client
            .fetch(
                Filter::new().kind(Kind::GiftWrap).pubkey(input.app_id.identity.public_key()),
                RELAY_TIMEOUT,
            )
            .await
        {
            Ok(w) => w,
            Err(e) => {
                last_err = format!("attempt {attempt}: fetch gift-wraps: {e}");
                client.disconnect().await;
                tokio::time::sleep(SETTLE).await;
                continue;
            }
        };
        client.disconnect().await;
        eprintln!("   carry DM poll (for {what}) attempt {attempt}: {} gift-wrap(s)", wraps.len());
        wraps_seen = wraps_seen.max(wraps.len());

        let allow: HashSet<String> = [expected_sender.to_string()].into_iter().collect();
        let msgs = decode_dms(&own_npub, &input.app_id.identity, wraps, Some(&allow)).await;
        decoded_seen = decoded_seen.max(msgs.len());
        eprintln!("   carry DM poll (for {what}) attempt {attempt}: {} decoded DM(s) from sender", msgs.len());
        // NEWEST match, never the first — relay state outlives a run, so a superseded ask or
        // ticket still sitting on the relay must not satisfy this poll (`select_dm_newest`
        // carries the full why).
        let pick = select_dm_newest(&msgs, |m| inspect(m));
        if let Some((found, sent_at)) = pick.newest {
            if pick.matched > 1 {
                eprintln!(
                    "   carry DM poll (for {what}) attempt {attempt}: {} match(es) — taking the NEWEST (sent_at={sent_at})",
                    pick.matched
                );
            }
            return Ok(found);
        }
        tokio::time::sleep(SETTLE).await;
        last_err = format!("attempt {attempt}: no {what} DM yet");
    }
    // ⚠ Gift-wraps arrived but NONE decoded as being from the expected sender. That is an npub
    // MISMATCH in all but pathological cases — the peer is running under a different identity than
    // the one configured here — and saying "no DM yet" instead sends the operator hunting a relay
    // or sealing fault that is not there. Cost a live round trip on 2026-09-06 before the two
    // counts already printed above were read side by side.
    if wraps_seen > 0 && decoded_seen == 0 {
        return Err(format!(
            "never received {what} from {expected_sender}: {wraps_seen} gift-wrap(s) DID arrive, but \
             none decoded as being from that npub — so the DM reached this node and was FILTERED \
             OUT, not lost. Almost certainly an npub mismatch: compare the npub the sending role \
             printed at ITS startup against the one on this command line. An identity is minted per \
             --data-dir, so a data-dir that was deleted, moved or changed mints a NEW npub and the \
             old one silently stops matching."
        ));
    }
    Err(format!("never received {what} from {expected_sender}: {last_err}"))
}

/// The ticket-poll predicate both carry roles share: a DM body that decodes as a `TransportTicket`
/// which is BOTH shape-valid AND an answer to THIS run's ask — the ticket must echo the
/// `ask_nonce` the role minted.
///
/// The echo check is the load-bearing half (fetch's FD1 predicate, observed live 2026-09-06;
/// QURATOR-308): relay state outlives a run, so a stale-but-NEWEST ticket from an earlier ask
/// still decodes and still passes `verify_shape` — newest-match (QURATOR-306) narrows that window
/// but cannot close it, because a prior ticket can postdate a superseded ask. Accepting one hands
/// production's claim gate a ticket it is RIGHT to refuse as `Unsolicited`, which reads as a
/// product failure and is not one.
///
/// `issue_ticket` normalises an empty nonce to `None` (`ticket.rs`), so `Some("")` never appears
/// on the wire: a ticket answering a nonce-less ask carries `None` and correctly fails here.
/// Both carry roles always mint a non-empty nonce (`mint_ask_nonce`), so that is right, not a gap.
///
/// Extracted rather than inlined at each poll so the unit test drives the SAME predicate the live
/// polls use — a hand-rolled copy in the test would be decorative (the QURATOR-183 discipline
/// this file already pins).
fn ticket_answering_ask(
    msg: &crate::commands::chat::ReceivedMessage,
    nonce: &str,
) -> Option<TransportTicket> {
    let trimmed = msg.content.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let t: TransportTicket = serde_json::from_str(trimmed).ok()?;
    t.verify_shape().ok()?;
    if t.ask_nonce.as_deref() != Some(nonce) {
        // Named so a live red distinguishes a NONCE mismatch (this line, with both values) from a
        // sender mismatch (nothing decodes from that npub; `poll_dms`'s tail error says which).
        // Without it, a rejected stale ticket reads as "no DM yet" — the confusing red this row
        // exists to remove, one level down.
        eprintln!(
            "   carry ticket poll: rejected a shape-valid ticket echoing nonce={:?} — this run minted {nonce:?}",
            t.ask_nonce.as_deref().unwrap_or("<none>")
        );
        return None;
    }
    Some(t)
}

/// Redeem a ticket through the FULL production redeem body — claim gate, dial-only endpoint,
/// fetch, `accept_manifest_bytes` (which writes the cache that is this row's subject), and the
/// spend — with a bounded retry. A failed attempt leaves the ask retryable (the claim is keyed to
/// the same request_id, and `claim_manifest_ask` re-grants on the same id), so retries are safe.
pub(super) async fn redeem_via_production(
    input: &CarryInput,
    from_npub: &str,
    ticket: &TransportTicket,
    newest_fingerprint: Option<&str>,
) -> Result<ImportedManifest, String> {
    let live = input.live_identity();
    let store = input.store.clone();
    let endpoint = new_shared_endpoint();
    // ⚠ The redeem body's first argument is the peer the ticket CAME FROM, never this node. It is
    // the first segment of the claim key — `claim_manifest_ask(&npub, &expected_author, …)` — and
    // the `expected_author` fallback (`ticket.author_npub.unwrap_or(npub)`, i.e. "an authorless
    // ticket means the SENDER's own collection") is the proof.
    //
    // This passed `input.app_id.npub()` — OUR OWN — until 2026-09-06, so the claim looked up
    // (us, author, slug) while the ask was recorded under (asked_peer, author, slug). Every redeem
    // fell through as `Unsolicited` and reported "That link doesn't answer a request you sent". It
    // survived because no suite using this helper has ever completed a live run: QURATOR-178 still
    // owes carry's 4-host run, and the fetch suite hit it on its first.
    let npub = from_npub.to_string();
    let ticket_json = serde_json::to_string(ticket)
        .map_err(|e| format!("serialize ticket: {e}"))?;

    let mut last_err = String::new();
    for attempt in 1..=REDEEM_RETRIES {
        let result = tokio::time::timeout(
            REDEEM_TIMEOUT,
            redeem_manifest_ticket_inner(
                npub.clone(),
                ticket_json.clone(),
                newest_fingerprint.map(|s| s.to_string()),
                &live,
                &store,
                &endpoint,
            ),
        )
        .await;
        match result {
            Ok(Ok(imported)) => {
                eprintln!("   carry redeem succeeded on attempt {attempt}");
                return Ok(imported);
            }
            Ok(Err(e)) => {
                last_err = format!("attempt {attempt}: redeem failed: {e}");
                eprintln!("   {last_err}");
            }
            Err(_) => {
                last_err = format!("attempt {attempt}: redeem did not complete within {REDEEM_TIMEOUT:?}");
                eprintln!("   {last_err}");
            }
        }
        if attempt < REDEEM_RETRIES {
            tokio::time::sleep(SETTLE).await;
        }
    }
    Err(format!("redeem did not succeed: {last_err}"))
}

// ---------------------------------------------------------------------------
// Role A — the author
// ---------------------------------------------------------------------------

/// CA1: seed + publish + answer C's ordinary ask, all through production paths. The operator then
/// KILLS this process; "A is offline" is the operator's topology, not a row.
///
/// Flags: `--seed-dir <dir> --asker-npub <C npub>`. The seed tree reuses `generate_seed_tree`
/// (WAN-E2E's helper) so the collection is small but non-empty.
async fn run_role_a(input: &CarryInput) -> Result<(), String> {
    let seed_dir = input
        .flag("--seed-dir")
        .ok_or_else(|| "role a requires --seed-dir <dir>".to_string())?
        .to_string();
    let asker_npub = input
        .flag("--asker-npub")
        .ok_or_else(|| "role a requires --asker-npub <cacher npub>".to_string())?
        .to_string();

    // (1) Seed the collection — production scan + draft save (via the shared WAN-M seeding helper).
    // The shape is the AUTHOR_SEED_* consts, single-sourced: role D's colliding-slug check (its
    // step (5)) compares the imported tree against these, so A's shape and D's expectation cannot
    // drift apart (until QURATOR-178 #3 this site passed the literals `24, 0`).
    super::generate_seed_tree(std::path::Path::new(&seed_dir), AUTHOR_SEED_FILES, AUTHOR_SEED_OFFSET)
        .map_err(|e| format!("generate seed tree: {e:#}"))?;
    super::seed_collection(
        &input.store,
        &input.app_id.identity,
        &input.app_id.browse_key,
        &seed_dir,
        CARRY_SLUG,
    )
    .map_err(|e| format!("seed collection: {e:#}"))?;
    eprintln!("   CA1 collection '{CARRY_SLUG}' seeded from {seed_dir}");

    // (2) Publish the teaser through the ONE shared harness path (`publish_teaser_for` →
    // `stamped_listing_for` → `stamp_for_teaser`, QURATOR-137). This site used to re-compose
    // prepare_listing + publish_listing_capped by hand — only because the E2E helper hardcoded its
    // slug — and the hand-copy dropped the stamp step, so the teaser's `snapshot_fingerprint`
    // drifted from the manifest's on the SAME unchanged tree (latent here: the 24-file seed never
    // truncated; the shared helper takes the slug, so the re-composition has no reason to exist).
    let published = super::publish_teaser_for(
        &input.store,
        &input.app_id.identity,
        &input.app_id.browse_key,
        CARRY_SLUG,
    )
    .await
    .map_err(|e| format!("publish carry teaser: {e:#}"))?;
    eprintln!(
        "   CA1 teaser published: {} part(s), truncated={}, to {} relay(s)",
        published.parts,
        published.truncated,
        input.relays.len()
    );

    // (3) The identity facts the operator needs for roles C and D (A's npub + full share code)
    // are printed by the probe entry before role dispatch, so they are available even on a run
    // that fails its flag checks — carry's bootstrap is circular across all three roles (the
    // fetch-suite precedent, ported 2026-09-21). The listening manifest endpoint itself is bound
    // inside `approve_request` below; its accept loop holds its own endpoint handle once spawned.

    // (4) Wait for C's ordinary ask and answer it through the production approval body.
    // `approve_request` drives `send_full_list_inner` — the real mint + DM path.
    eprintln!("   CA1 waiting for the cacher's request-DM (polling)...");
    let (slug, nonce) = poll_dms(
        input,
        &asker_npub,
        |msg| {
            // Production's parser, never a copy (QURATOR-183).
            let body = crate::auto_approve::ManifestRequestBody::parse(&msg.content)?;
            Some((body.slug.clone(), body.ask_nonce.clone()))
        },
        "manifest request from the cacher",
    )
    .await?;
    if slug != CARRY_SLUG {
        return Err(format!("the cacher asked for slug '{slug}', not '{CARRY_SLUG}'"));
    }
    eprintln!("   CA1 got the cacher's ask for '{slug}' (nonce={nonce:?})");

    let shared_relay = crate::net::new_shared();
    super::approve_request(
        &input.store,
        &input.live_identity(),
        &shared_relay,
        &asker_npub,
        &slug,
        nonce.as_deref(),
    )
    .await
    .map_err(|e| format!("approve_request (send_full_list_inner): {e:#}"))?;
    eprintln!("   CA1 answered via send_full_list_inner — ticket minted + DM'd; kill this process now");

    // HOLD: the cacher dials THIS process's iroh endpoint to redeem, so A must stay up until the
    // cacher's CC1 row reports done — then the operator kills this process, which IS the "A goes
    // offline" topology of phase 2. The accept loop spawned by ensure_endpoint inside
    // send_full_list_inner owns its endpoint handle, so sleeping here keeps it serving.
    eprintln!("   CA1 holding for the cacher's redeem — kill this process once CC1 reports done");
    for _ in 0..180 {
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Role C — the cacher, phase 1 (ask A, cache A's manifest)
// ---------------------------------------------------------------------------

/// CC1: ordinary own-collection ask → A's ticket → production redeem (which writes the cache via
/// `accept_manifest_bytes`). Flags: `--phase 1 --author-npub <A npub> --author-share-code <hbk…>`,
/// plus the OPTIONAL `--decoy-seed-dir <dir>` (QURATOR-178 assertion #3 — see the decoy block at
/// the end of this fn for what it arms and why it is opt-in).
async fn run_role_c_phase1(input: &CarryInput) -> Result<(), String> {
    let author_npub = input
        .flag("--author-npub")
        .ok_or_else(|| "role c phase 1 requires --author-npub <A npub>".to_string())?
        .to_string();
    let author_share_code = input
        .flag("--author-share-code")
        .ok_or_else(|| "role c phase 1 requires --author-share-code <hbk…>".to_string())?
        .to_string();

    // Save A as a contact WITH the browse key — `accept_manifest_bytes` reads it to decrypt.
    save_peer_contact(input, &author_npub, &author_share_code)?;

    // Ask A for its own collection — the production ordinary-ask builder.
    let nonce = mint_ask_nonce();
    let content = crate::commands::chat::build_manifest_request(
        CARRY_SLUG,
        "",        // fingerprint_seen — placeholder, as the WAN-M/E2E harnesses do
        None,      // teaser_event_id
        None,      // mascara_pubkey — vestigial, always None (wire_freeze)
        Some(nonce.clone()),
    )?;
    send_request_dm_to(input, &content, &author_npub, &author_npub, &author_npub, CARRY_SLUG, "", &nonce)
        .await?;
    eprintln!("   CC1 sent the ordinary ask to the author (nonce={nonce})");

    // Await A's ticket. A's ticket has author_npub == None (own-collection serve) — asserted, so
    // this leg really is the ordinary carrier before Carrier 4 enters. It must echo THIS run's
    // nonce (`ticket_answering_ask` carries the why).
    let ticket = poll_dms(
        input,
        &author_npub,
        |msg| ticket_answering_ask(msg, &nonce),
        "ticket from the author",
    )
    .await?;
    if ticket.author_npub.is_some() {
        return Err(format!(
            "the author's ticket carries author_npub={:?}, but an own-collection serve must have None",
            ticket.author_npub
        ));
    }
    eprintln!("   CC1 received the author's ticket (request_id={})", ticket.request_id);

    // Redeem through the full production body — this is what writes A's envelope into C's cache.
    // The ticket came from A, so A is the claim key's peer segment.
    let imported = redeem_via_production(input, &author_npub, &ticket, None).await?;
    if imported.served_by.is_some() {
        return Err(format!(
            "served_by={:?} on a direct serve — carrier-4 provenance must be None here",
            imported.served_by
        ));
    }
    let entries = imported.collection.collection.listing.len();
    if entries == 0 {
        return Err("the cached manifest has an empty tree".to_string());
    }
    eprintln!("   CC1 cached the author's manifest: {entries} entries, served_by=None");

    // The cache is the delivery — read it back through the production reader and prove it verifies
    // under A's key (this is the exact read `send_cached_manifest_inner` will do in phase 2).
    verify_cached_under(input, &author_npub, CARRY_SLUG)?;
    println!("# carry-C cache primed for author {author_npub} / slug {CARRY_SLUG}");

    // QURATOR-178 assertion #3 — the colliding-slug DECOY (opt-in via `--decoy-seed-dir`). When
    // given, C ALSO holds its OWN collection at the SAME slug `wan-carry`, seeded through the
    // PRODUCTION add-collection path (`seed_collection` → `scan_selective` +
    // `save_collection_draft`) — the same path role A seeds through, never a manifest-cache write:
    // C's cache is written only by the redeem above, and the direct-write shortcut is forbidden by
    // `the_carry_suite_caches_by_redeeming_and_never_writes_the_cache_directly`. Seeding a DRAFT is
    // what makes the decoy GENUINE: `build_slug_manifest` serves from `load_collection_draft`, so a
    // broken no-fallback fence in `StoreManifestSource::payload` (manifest_source.rs ~line 90: a
    // cache MISS on a re-serve must ERROR, never fall through to the cacher's own same-slug draft)
    // would find and serve exactly this tree — the mis-route the negative exists to catch. The
    // decoy draft persists in C's data-dir into phase 2 (the same dir the cache lives in), so
    // arming here arms the phase-2 re-serve. No teaser is published for it, so discovery and
    // presence stay clean. The trees are distinguishable BY CONSTRUCTION: 7 files at offset 1000
    // vs A's 24 at offset 0 (the DECOY_SEED_* consts).
    //
    // OPT-IN, not default: the 2026-09-23 positive-only run must stay reproducible unchanged — an
    // unconditional decoy would alter C's store shape and the row names on every future run, and
    // the unarmed choreography is the documented operator baseline. The armed run is a deliberate
    // second configuration, and the row NAME records which one ran (`decoy_status_suffix` in the
    // dispatcher) so an unarmed negative can never read as an exercised one.
    if let Some(decoy_dir) = input.flag("--decoy-seed-dir") {
        super::generate_seed_tree(
            std::path::Path::new(decoy_dir),
            DECOY_SEED_FILES,
            DECOY_SEED_OFFSET,
        )
        .map_err(|e| format!("generate decoy seed tree: {e:#}"))?;
        super::seed_collection(
            &input.store,
            &input.app_id.identity,
            &input.app_id.browse_key,
            decoy_dir,
            CARRY_SLUG,
        )
        .map_err(|e| format!("seed decoy collection: {e:#}"))?;
        // Report the entry count actually on disk (read back), not the const — a leftover file in
        // the operator's decoy dir would otherwise make both prints state a number the scan never
        // saw (`generate_seed_tree` does not clean its dir).
        let n = std::fs::read_dir(decoy_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).count())
            .unwrap_or(DECOY_SEED_FILES);
        eprintln!("   CC1 decoy armed: C's OWN '{CARRY_SLUG}' collection seeded ({n} entries)");
        println!("# carry-C decoy: own '{CARRY_SLUG}' collection seeded ({n} entries)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Role C — the cacher, phase 2 (re-serve the cached copy to D while A is offline)
// ---------------------------------------------------------------------------

/// CC2: wait for D's AUTHOR-ask, then answer it through the production Carrier-4 re-serve body
/// (`send_cached_manifest_inner`) — never a hand-rolled mint. Flags: `--phase 2 --asker-npub <D
/// npub> --author-npub <A npub>`. Runs while A is down: nothing here dials or DMs A.
async fn run_role_c_phase2(input: &CarryInput) -> Result<(), String> {
    let asker_npub = input
        .flag("--asker-npub")
        .ok_or_else(|| "role c phase 2 requires --asker-npub <asker npub>".to_string())?
        .to_string();
    let author_npub = input
        .flag("--author-npub")
        .ok_or_else(|| "role c phase 2 requires --author-npub <A npub>".to_string())?
        .to_string();

    eprintln!("   CC2 waiting for the asker's author-request-DM (polling)...");
    let (slug, nonce, author_from_ask) = poll_dms(
        input,
        &asker_npub,
        |msg| {
            // Production's parser AND production's routing decision (QURATOR-183) — the harness
            // must not decide for itself that this is a re-serve. `approval_body_for` is the same
            // discriminator the auto-approve loop matches on, so if production ever stopped
            // routing an author-bearing ask to the cached-manifest body, this row goes red instead
            // of silently diverging from the code that ships.
            let body = crate::auto_approve::ManifestRequestBody::parse(&msg.content)?;
            let author = match crate::auto_approve::approval_body_for(&body) {
                crate::auto_approve::ApprovalBody::CachedManifest { author } => Some(author),
                crate::auto_approve::ApprovalBody::FullList => None,
            };
            Some((body.slug.clone(), body.ask_nonce.clone(), author))
        },
        "author-request from the asker",
    )
    .await?;
    if slug != CARRY_SLUG {
        return Err(format!("the asker asked for slug '{slug}', not '{CARRY_SLUG}'"));
    }
    // The ask MUST name A as the author — that field is the whole Carrier-4 discriminator on the
    // ask wire, and it is what routes the ask to `send_cached_manifest_inner` rather than the
    // own-collection body. Answering it here is NO LONGER a harness deviation: owner ruling
    // 2026-09-04 (QURATOR-164) deleted `should_auto_approve`'s step (0), so production auto-serves
    // an author-bearing ask too — third-party serving is background infrastructure, needing no
    // approval and no human — QURATOR-164 deleted approvals entirely. The harness's remaining
    // deviation is only that it has no caps pacer.
    let Some(author_from_ask) = author_from_ask else {
        return Err(
            "production's `approval_body_for` routed the asker's request-DM to the OWN-COLLECTION \
             body, not the cached-manifest one — it is not a Carrier-4 ask (no `author_npub`), and \
             re-serving it here would serve the wrong collection on a slug collision"
                .to_string(),
        );
    };
    if author_from_ask != author_npub {
        return Err(format!(
            "the ask names author {author_from_ask} but this cacher was told --author-npub {author_npub}"
        ));
    }
    eprintln!("   CC2 got the asker's author-ask for '{slug}' by {author_npub} (nonce={nonce:?})");

    // Save D as a contact (realism only — nothing on the serve path reads contacts or grants).
    super::save_asker_contact(&input.store, &asker_npub)
        .map_err(|e| format!("save asker contact: {e:#}"))?;

    // THE production re-serve body: cache read + verify-under-A + Listen endpoint + ticket with
    // author_npub = Some(A) + ticket DM, all inside.
    let shared_relay = crate::net::new_shared();
    send_cached_manifest_inner(
        asker_npub.clone(),
        author_npub.clone(),
        slug.clone(),
        nonce,
        &input.live_identity(),
        &input.store,
        &shared_relay,
        &new_shared_endpoint(),
    )
    .await
    .map_err(|e| format!("send_cached_manifest_inner: {e}"))?;
    eprintln!("   CC2 re-served the cached copy via send_cached_manifest_inner — ticket DM'd");

    // The "green while writing nothing" guard that used to sit here checked that the re-serve body
    // had written a standing grant keyed (D, Some(A), slug) before the DM. **Grants were deleted
    // by owner ruling 2026-09-04 (QURATOR-164) — public collections need no approval — so there is
    // no longer a record to assert on.** The row's real evidence is unchanged and stronger: D
    // redeems the ticket and verifies the envelope under A's key (phase D below), which is a
    // property of the bytes rather than of a bookkeeping row.

    // HOLD: the asker dials THIS process's iroh endpoint to redeem the cached copy, so C must
    // stay up until D's CD1 row reports done. The accept loop spawned by ensure_endpoint inside
    // send_cached_manifest_inner owns its endpoint handle, so sleeping here keeps it serving.
    eprintln!("   CC2 holding for the asker's redeem — Ctrl-C when CD1 reports done");
    for _ in 0..180 {
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Role D — the asker
// ---------------------------------------------------------------------------

/// CD1: ask C for **A's** collection (the production author-ask builder), redeem C's ticket, and
/// prove the Carrier-4 properties: ticket names A; the imported copy is served_by == Some(C); D's
/// own cache (written by `accept_manifest_bytes` inside the redeem) verifies under A's x-only key
/// and NOT under C's; and (step 5, QURATOR-178 #3) the imported TREE is A's seed shape, never the
/// cacher's own same-slug decoy. Flags: `--carrier-npub <C npub> --carrier-share-code <hbk…>
/// --author-npub <A npub> --author-share-code <hbk…>`, plus the OPTIONAL bare `--decoy-armed` —
/// passed when C's phase 1 ran with `--decoy-seed-dir`. The flag only RENAMES the row (an armed
/// negative must not read as an unarmed one, nor the reverse); the step-5 tree check itself runs
/// armed or not, because A's seed shape is a suite invariant either way.
async fn run_role_d(input: &CarryInput) -> Result<(), String> {
    let carrier_npub = input
        .flag("--carrier-npub")
        .ok_or_else(|| "role d requires --carrier-npub <C npub>".to_string())?
        .to_string();
    let carrier_share_code = input
        .flag("--carrier-share-code")
        .ok_or_else(|| "role d requires --carrier-share-code <hbk…>".to_string())?
        .to_string();
    let author_npub = input
        .flag("--author-npub")
        .ok_or_else(|| "role d requires --author-npub <A npub>".to_string())?
        .to_string();
    let author_share_code = input
        .flag("--author-share-code")
        .ok_or_else(|| "role d requires --author-share-code <hbk…> (the AUTHOR's, not the carrier's)".to_string())?
        .to_string();

    // C must be a contact with its browse key for the redeem's DM/ticket leg — production D would
    // have C's share code already (it dialled/asked C).
    save_peer_contact(input, &carrier_npub, &carrier_share_code)?;
    // A must ALSO be a contact, carrying A's OWN browse key: `redeem_via_production` calls
    // `accept_manifest_bytes(&expected_author, ...)` pinned to the ticket's `author_npub` (A), never
    // the DM sender (C) — that pin is what makes Carrier-4 authorship attribution correct
    // (fulfil.rs's own comment: "pinning to C made open_manifest's author check refuse every
    // carrier-4 delivery"). Without A as a contact here, `accept_manifest_bytes`'s `load_contact`
    // lookup on A's npub finds nothing and the redeem fails with "Add this peer as a contact..." —
    // found live 2026-09-10, the harness's role D never saved A at all (0 integration coverage
    // before this run; production D would already hold A's share code from having added them).
    save_peer_contact(input, &author_npub, &author_share_code)?;

    // Ask C for A's collection — the production Carrier-4 ask wire (author_npub names A).
    let nonce = mint_ask_nonce();
    let content = crate::commands::chat::build_manifest_request_for_author(
        CARRY_SLUG,
        "",
        None,
        None,
        Some(nonce.clone()),
        &author_npub,
    )?;
    // The ask is keyed on the AUTHOR (A), not the DM sender (C) — the same key resolution
    // `redeem_manifest_ticket_inner` performs via `ticket.author_npub`.
    send_request_dm_to(input, &content, &carrier_npub, &carrier_npub, &author_npub, CARRY_SLUG, "", &nonce)
        .await?;
    eprintln!("   CD1 sent the author-ask to the cacher (nonce={nonce})");

    // Await C's ticket — it MUST name A (`author_npub == Some(A)`): that field is what makes the
    // redeem pin the manifest to A and the cache land under A's key. It must also echo THIS run's
    // nonce (`ticket_answering_ask` carries the why) — a Carrier-4 re-serve answers the ask it was
    // HANDED, and D's claim gate keys on the nonce just as it keys on the author.
    let ticket = poll_dms(
        input,
        &carrier_npub,
        |msg| ticket_answering_ask(msg, &nonce),
        "ticket from the cacher",
    )
    .await?;
    match ticket.author_npub.as_deref() {
        Some(a) if a == author_npub => {}
        other => {
            return Err(format!(
                "the cacher's ticket carries author_npub={other:?}, expected Some({author_npub}) — \
                 not a Carrier-4 re-serve ticket"
            ))
        }
    }
    eprintln!("   CD1 received the cacher's ticket naming the author (request_id={})", ticket.request_id);

    // Redeem through the full production body. The ticket came from C, so C is the claim key's peer
    // segment — matching the ask D recorded against C. The claim resolves the AUTHOR from the ticket
    // (A) separately; `accept_manifest_bytes` pins to A and caches under A.
    let imported = redeem_via_production(input, &carrier_npub, &ticket, None).await?;

    // (1) Carrier-4 provenance: the serving peer is C, not A.
    match imported.served_by.as_deref() {
        Some(s) if s == carrier_npub => {}
        other => {
            return Err(format!(
                "served_by={other:?}, expected Some({carrier_npub}) — the re-serve did not mark the carrier"
            ))
        }
    }
    // (2) The tree is non-empty (a real manifest arrived, not an error shell).
    let entries = imported.collection.collection.listing.len();
    if entries == 0 {
        return Err("the imported manifest has an empty tree".to_string());
    }
    // (3) Not stale (we passed no newest_fingerprint pin).
    if imported.stale {
        return Err("the imported manifest is marked stale despite no fingerprint pin".to_string());
    }
    eprintln!(
        "   CD1 redeemed the cached copy: {entries} entries, served_by=Some(cacher), stale=false"
    );

    // (4) THE authenticity property: the copy now in D's cache verifies under A's x-only key and
    // refuses C's. This is owner ruling ② — the property lives in the signature, never a ledger.
    verify_cached_under(input, &author_npub, CARRY_SLUG)?;
    let carrier_pk = hb_core::identity::parse_npub(&carrier_npub)
        .map_err(|e| format!("parse carrier npub: {e}"))?;
    let json = read_cached(input, &author_npub, CARRY_SLUG)?;
    let envelope = hb_core::manifest::ManifestEnvelope::from_json(&json)
        .map_err(|e| format!("parse cached envelope: {e}"))?;
    if envelope.verify_author(&carrier_pk).is_ok() {
        return Err("the cached copy verifies under the CARRIER's key — the re-serve passed off C's \
                    own manifest as A's"
            .to_string());
    }
    eprintln!("   CD1 cache copy verifies under the AUTHOR's key and refuses the carrier's");

    // (5) QURATOR-178 #3 — the colliding-slug discriminator: the imported tree must be A's SEED
    // SHAPE (24 entries, `file-0000…0023.bin`), never the cacher's decoy at the same slug (7
    // entries, `file-1000…1006.bin`). Runs UNCONDITIONALLY (armed or not): A's shape is a suite
    // invariant, so an armed decoy is caught with no extra flag — the flag only renames the row.
    //
    // WHY this check when (4) already pins the signature — the (a)-vs-(b) reasoning the row owes:
    // (a) — the signature checks — catches every mis-route REACHABLE in this topology that yields
    // a C-signed envelope. The only competing tree C can serve at this slug is its OWN decoy,
    // authored and signed by C, so a fallback serve is C-signed and step (4) reds on it; nothing
    // re-signs C's tree under A's key (that would need A's private key). For THAT class, (4) alone
    // is sufficient and this check is redundant.
    // But (a) is COARSER than the behaviour this row names — "D receives A's wan-carry TREE", not
    // "D receives some A-signed envelope" (CLAUDE.md §9: compare the assertion's cardinality to
    // the behaviour it names). A defect that hands D an A-signed envelope for the WRONG tree
    // passes (4) untouched: a wrong-slot cache read on D's side (`read_cached`'s npub/slug are
    // parameters — the 2026-09-06 hardcoding incident in this very file), or a future serve path
    // resolving (author, slug) to a different A collection. (5) pins the exact tree — right key
    // AND right bytes — so it discriminates a failure class (4) cannot see. Not decorative; kept.
    let names: Vec<String> = imported
        .collection
        .collection
        .listing
        .iter()
        .map(|e| e.name.clone())
        .collect();
    imported_tree_is_the_authors(&names)?;
    eprintln!(
        "   CD1 tree is the AUTHOR's seed shape ({} entries, A's name window — not the decoy's)",
        names.len()
    );

    println!("# carry-D imported author {author_npub} / slug {CARRY_SLUG} via carrier {carrier_npub}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared tail helpers
// ---------------------------------------------------------------------------

/// Save `peer_npub` as a contact carrying the full share code's browse key — the contact shape
/// `accept_manifest_bytes` loads (same construction as `suite_wan_e2e::build_probe_input`).
pub(super) fn save_peer_contact(input: &CarryInput, peer_npub: &str, share_code_str: &str) -> Result<(), String> {
    let share = hb_core::ShareCode::parse(share_code_str)
        .map_err(|e| format!("invalid share code for {peer_npub}: {e}"))?;
    let contact = crate::store::CachedPeer {
        npub: peer_npub.to_string(),
        source: crate::store::ContactSource::Manual,
        browse_key_hex: share.browse_key().map(hex::encode),
        petname: Some("wan-carry-peer".to_string()),
        profile: None,
        collections: vec![],
        listings_state: Default::default(), // QURATOR-134 tri-state (not classified on this stub path)
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None,
        local_tags: vec![],
        fingerprint: None,
    };
    input
        .store
        .save_contact(&crate::store::CachedPeer::pubkey_hash(peer_npub), &contact)
        .map_err(|e| format!("save contact: {e}"))
}

/// Read D's (or C's) own cached copy for `(npub, slug)` back through the production reader.
/// ⚠ The slug is a PARAMETER. It was `CARRY_SLUG` until 2026-09-06 — the same hardcoding that broke
/// `send_request_dm_to`, in the same file, found two commits later because fixing the first one did
/// not prompt a check of its neighbours. A sibling suite reusing this looked up the wrong key and
/// reported "accept_manifest_bytes never wrote it" about a cache entry that was written correctly.
pub(super) fn read_cached(input: &CarryInput, npub: &str, slug: &str) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::manifest_cache::get_latest(&input.store.manifest_cache_dir(), npub, slug, now)
        .ok_or_else(|| format!("no cached copy for ({npub}, {slug}) — accept_manifest_bytes never wrote it"))
}

/// Assert the cached copy for `(npub, slug)` exists and verifies under that npub's x-only key —
/// the exact read + verify `send_cached_manifest_inner` performs before re-serving.
pub(super) fn verify_cached_under(input: &CarryInput, npub: &str, slug: &str) -> Result<(), String> {
    let json = read_cached(input, npub, slug)?;
    let envelope = hb_core::manifest::ManifestEnvelope::from_json(&json)
        .map_err(|e| format!("parse cached envelope: {e}"))?;
    let pk = hb_core::identity::parse_npub(npub).map_err(|e| format!("parse npub: {e}"))?;
    envelope
        .verify_author(&pk)
        .map_err(|e| format!("cached copy does not verify under {npub}: {e}"))
}

// ---------------------------------------------------------------------------
// QURATOR-178 assertion #3 — colliding-slug decoy helpers (pure; unit-tested below)
// ---------------------------------------------------------------------------

/// The file names `super::generate_seed_tree(dir, count, offset)` writes — the SAME format string
/// (`file-{:04}.bin`), re-derived here for the EXPECTED side of the comparison only.
/// `the_decoy_discriminator_tells_the_authors_tree_from_the_cachers_own` seeds REAL trees through
/// `generate_seed_tree` and feeds the read-back names to [`imported_tree_is_the_authors`], so a
/// drift in either format string reds that test — this expectation cannot silently diverge from
/// what the seeder writes.
fn expected_seed_names(count: usize, offset: usize) -> HashSet<String> {
    (0..count).map(|i| format!("file-{:04}.bin", offset + i)).collect()
}

/// Is `names` — the top-level entry names of an imported tree — the AUTHOR's seed shape, never the
/// cacher's decoy at the same slug? Compares as a SET: scan/relay order is not load-bearing here
/// (the snapshot fingerprint is what canonicalises order on the wire). Three-way verdict so the
/// red NAMES the side it matched — the decoy (the mis-route this row exists to catch) or neither
/// (an unexpected tree: a polluted seed dir is a red, never a pass — `generate_seed_tree` does not
/// clean its dir, so operator leftovers must surface, not satisfy).
fn imported_tree_is_the_authors(names: &[String]) -> Result<(), String> {
    let got: HashSet<String> = names.iter().cloned().collect();
    let author = expected_seed_names(AUTHOR_SEED_FILES, AUTHOR_SEED_OFFSET);
    let decoy = expected_seed_names(DECOY_SEED_FILES, DECOY_SEED_OFFSET);
    if got == author {
        return Ok(());
    }
    if got == decoy {
        return Err(format!(
            "the imported tree IS the cacher's own same-slug collection ({DECOY_SEED_FILES} \
             entries, file-{DECOY_SEED_OFFSET:04}….bin) — the re-serve resolved by (C, slug) \
             instead of (author_npub, slug): the no-fallback fence in manifest_source.rs's \
             payload() is broken"
        ));
    }
    Err(format!(
        "the imported tree ({n} entries) matches NEITHER the author's seed shape \
         ({AUTHOR_SEED_FILES} x file-{AUTHOR_SEED_OFFSET:04}….bin) nor the cacher's decoy \
         ({DECOY_SEED_FILES} x file-{DECOY_SEED_OFFSET:04}….bin) — an unexpected tree; check the \
         seed dirs for leftovers",
        n = names.len()
    ))
}

/// The TAP row suffix that keeps an ARMED negative distinguishable from an unarmed one — a skipped
/// negative must never read as a passed one (CLAUDE.md memory: "a skipped row is not a passing
/// row"). Exact strings, asserted by EQUALITY in the test: "NOT armed" contains "armed", so a
/// substring assert could not tell the two branches apart (§9: an assertion coarser than the
/// behaviour it names).
fn decoy_status_suffix(decoy_armed: bool) -> &'static str {
    if decoy_armed {
        " [decoy armed: C holds its own 'wan-carry']"
    } else {
        " [decoy NOT armed: the colliding-slug negative was NOT exercised this run]"
    }
}

// ---------------------------------------------------------------------------
// Unit tests — pure parts (no network, no iroh endpoint)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// CARRY's ask-key discipline, pinned against the PRODUCTION source (the `suite_cap.rs`
    /// precedent): `redeem_manifest_ticket_with_progress` must resolve the claim's author from
    /// `ticket.author_npub`, falling back to the DM sender only when the field is absent. Keying
    /// on the carrier would leave D's author-ask `Unsolicited` and fail closed on every Carrier-4
    /// re-serve — exactly the mis-key this row exists to catch. (`commands/fulfil.rs`'s own unit
    /// test pins the behaviour; this one pins that the WAN harness rides the production body at
    /// all — the QURATOR-178 harness-half contract — by asserting the resolution string survives
    /// in the source this suite compiles against.)
    ///
    /// P-10 MUTATION (must red this test): in `commands/fulfil.rs`, inside
    /// `redeem_manifest_ticket_with_progress`, change
    /// `let expected_author = ticket.author_npub.clone().unwrap_or_else(|| npub.clone());`
    /// (line ~599, the CODE line — not the doc-comment copy in the test module) to
    /// `let expected_author = npub.clone();` — the string vanishes from the function body this
    /// test slices out, and the test reds. (The LIVE CD1 row reds too: `claim_manifest_ask`
    /// returns `Unsolicited` because the claim key (C, C, slug) never matches the ask key
    /// (C, A, slug).)
    #[test]
    fn carrier4_redeem_resolves_the_author_from_the_ticket() {
        let src = include_str!("../commands/fulfil.rs");
        let needle = "let expected_author = ticket.author_npub.clone().unwrap_or_else(|| npub.clone());";
        let in_redeem = src
            .split("pub(crate) async fn redeem_manifest_ticket_with_progress")
            .nth(1)
            .and_then(|body| body.split("\n}\n").next())
            .unwrap_or("");
        assert!(
            in_redeem.contains(needle),
            "redeem_manifest_ticket_with_progress must resolve the claim author from \
             ticket.author_npub (falling back to the DM sender only when absent) — the Carrier-4 \
             ask is keyed on the AUTHOR, and a carrier-keyed claim fails closed as Unsolicited"
        );
    }

    /// The ticket-shape discriminator this suite exists to exercise: `author_npub == Some(A)` is
    /// what makes the redeem pin to A, cache under A, and mark `served_by`. Pin the SERIALIZATION
    /// round-trip so the field cannot silently stop crossing the wire (a suite that polled for a
    /// ticket missing the field would mis-diagnose a re-serve as an ordinary serve).
    ///
    /// P-10 MUTATION (must red this test): in `crates/hb-core/src/ticket.rs`, change
    /// `#[serde(skip_serializing_if = "Option::is_none")] pub author_npub: Option<String>` to
    /// `#[serde(skip)] pub author_npub: Option<String>` — the field stops serializing, the
    /// round-trip below reads `None`, and CD1's `author_npub == Some(A)` assertion would fail on
    /// every live run.
    #[test]
    fn carrier4_ticket_serializes_the_author_npub() {
        let ticket = hb_core::TransportTicket {
            hb: hb_core::ticket::TICKET_TAG.to_string(),
            ticket_v: hb_core::TICKET_V,
            request_id: "req-1".to_string(),
            slug: "wan-carry".to_string(),
            node_addr: "n0-addr".to_string(),
            issued_at: 1_700_000_000,
            ask_nonce: Some("n".to_string()),
            author_npub: Some("npub1author".to_string()),
        };
        let json = serde_json::to_string(&ticket).expect("serialize ticket");
        let back: hb_core::TransportTicket = serde_json::from_str(&json).expect("deserialize ticket");
        assert_eq!(back.author_npub.as_deref(), Some("npub1author"),
            "author_npub must cross the wire — it is the Carrier-4 branch discriminator");
        // And its absence must stay absent (the ordinary own-collection serve).
        let ordinary = hb_core::TransportTicket {
            author_npub: None,
            ..ticket
        };
        let json = serde_json::to_string(&ordinary).expect("serialize ordinary ticket");
        assert!(!json.contains("author_npub"), "None must not serialize the field");
    }
    /// The request-DM parse and the re-serve ROUTING DECISION must come from production, never
    /// from a harness copy. This is the 4th instance of that defect class in this repo
    /// (`sanitize_node_addr` 2026-08-27, `approve_request` 2026-09-01, QURATOR-169) — twice it
    /// surfaced as a phantom PRODUCT defect, once as a phantom GREEN. A hand-rolled field read
    /// also silently loses production's blank-string-to-`None` normalisation, which is the exact
    /// thing that decides whether an ask counts as Carrier-4.
    ///
    /// MUTATION (P-10) — resolved by containing function, not by text: in `run_role_c_phase2`,
    /// replace the `ManifestRequestBody::parse` + `approval_body_for` block with a hand-rolled
    /// `serde_json::from_str` + `v.get("hb")` field read → the tag-literal assert and both
    /// call-count asserts red. Comments are stripped first, so restating the rule in prose
    /// cannot satisfy it.
    #[test]
    fn the_carry_suite_parses_and_routes_through_production_never_a_copy() {
        // Production half only — the test half below quotes the very literals this scans for,
        // the self-referential trap CLAUDE.md §9 records.
        let src = include_str!("suite_wan_carry.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("test module must exist")];
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !code.contains("\"manifest_request\""),
            "the suite must not re-implement the request-DM tag check — that literal belongs to \
             `ManifestRequestBody::parse`, and a copy of it covers the copy, not what ships"
        );
        assert_eq!(
            code.matches("ManifestRequestBody::parse").count(),
            2,
            "both roles (A reading C's ask, C reading D's) must parse with production's parser"
        );
        // The CALL form specifically: this file's own refusal message names the symbol in prose,
        // and prose is not a call. Counting the bare name would have this guard satisfy itself.
        assert_eq!(
            code.matches("approval_body_for(").count(),
            1,
            "C's re-serve must be reached BECAUSE production's discriminator said so — the harness \
             does not get to decide for itself that an ask is a re-serve"
        );
    }

    /// C's cache MUST be populated by driving the production browse path, never by writing the
    /// cache directly. The shortcut (`manifest_cache::put`) is two lines, irresistible, and would
    /// make all four row assertions pass while proving nothing — including the negative in #3,
    /// because the harness would have written the very key the row exists to test.
    ///
    /// Same shape as `sanitize_node_addr` (2026-08-27) and `approve_request` (2026-09-01): a
    /// harness that copies a production path instead of calling it covers the copy, not the code
    /// that ships. Twice that surfaced as a phantom PRODUCT defect, once as a phantom GREEN. This
    /// is a drift guard, not integration coverage (the `suite_cap.rs` precedent) — but it fails
    /// loudly on re-divergence, which is the part nobody was getting.
    ///
    /// MUTATION (P-10): in `run_role_c_phase1`, replace the `redeem_manifest_ticket_inner(` call
    /// with a direct `crate::manifest_cache::put(` write — both halves red. Comments are stripped
    /// first, so restating the rule in prose cannot satisfy it.
    #[test]
    fn the_carry_suite_caches_by_redeeming_and_never_writes_the_cache_directly() {
        // Slice the PRODUCTION half only. Scanning the whole file would make this guard red on
        // its own needle literals below — the self-referential trap that CLAUDE.md §9 records
        // ("a raw whole-page scan reds on the page's own prose"). The model guard,
        // `the_harness_approves_through_send_full_list_inner_and_never_rebuilds_it`, slices a
        // function body for exactly this reason.
        let src = include_str!("suite_wan_carry.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("test module must exist")];
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !code.contains("manifest_cache::put("),
            "the carry suite must never write C's cache directly — populate it by redeeming, so \
             the row tests the keying that production performs rather than the keying the harness \
             chose for itself"
        );
        assert!(
            code.contains("redeem_manifest_ticket_inner("),
            "C must cache by redeeming through the production path, which is what writes the \
             cache entry via accept_manifest_bytes"
        );
    }

    /// A decoded DM, minimal: the newest-selection keys on `sent_at` and reaches `content` only
    /// through the caller's `inspect` closure.
    fn dm(content: String, sent_at: &str) -> crate::commands::chat::ReceivedMessage {
        crate::commands::chat::ReceivedMessage {
            from: "npub1peer".to_string(),
            to: "npub1self".to_string(),
            content,
            sent_at: sent_at.to_string(),
        }
    }

    /// The DM selection must take the NEWEST match, never the first — QURATOR-306, the third site
    /// of the race `suite_wan_fetch`'s `poll_dms_newest` was fixed for on 2026-09-06 and WAN-E2E's
    /// `select_ticket_dm_newest` for on 2026-09-21 (QURATOR-137). Relay state outlives a run, so
    /// a superseded ask or ticket still on the relay must not satisfy a later poll. Pinned for
    /// BOTH closure shapes carry polls with — a ticket (roles C·1 and D) and a request-DM parsed
    /// by PRODUCTION's parser (roles A and C·2) — and for both inbox orders: newest wins
    /// regardless of iteration order.
    ///
    /// P-10 MUTATION (the orchestrator applies this; a worker does not run it): in
    /// `select_dm_newest` (this file, the `is_newer` guard's `is_none_or` closure), replace
    /// `msg.sent_at > *sent_at` with `false`, so only the FIRST hit is ever kept — the
    /// older-first arms below must then fail (they return the older match).
    #[test]
    fn dm_selection_takes_the_newest_match_for_both_carry_shapes() {
        // Shape 1 — the ticket poll (roles C·1 and D): the SAME extracted predicate those sites
        // pass (`ticket_answering_ask`), never a hand-rolled copy — both tickets below echo the
        // nonce "n" this arm polls for, so the newest-selection this pins is the live one's.
        let ticket_json = |request_id: &str| {
            let ticket = TransportTicket {
                hb: hb_core::ticket::TICKET_TAG.to_string(),
                ticket_v: hb_core::TICKET_V,
                request_id: request_id.to_string(),
                slug: CARRY_SLUG.to_string(),
                node_addr: "n0-addr".to_string(),
                issued_at: 1_700_000_000,
                ask_nonce: Some("n".to_string()),
                author_npub: None,
            };
            serde_json::to_string(&ticket).expect("serialize ticket")
        };
        let ticket_inspect =
            |msg: &crate::commands::chat::ReceivedMessage| ticket_answering_ask(msg, "n");
        let older = dm(ticket_json("req-older"), "2026-09-21T00:00:00Z");
        let newer = dm(ticket_json("req-newer"), "2026-09-21T00:01:00Z");
        // Both inbox orders: the NEWER DM wins (first-match returns req-older in the first order).
        for order in [vec![older.clone(), newer.clone()], vec![newer.clone(), older.clone()]] {
            let pick = select_dm_newest(&order, ticket_inspect);
            let (ticket, sent_at) = pick.newest.expect("a ticket match exists");
            assert_eq!(ticket.request_id, "req-newer", "the NEWEST match must win");
            assert_eq!(sent_at, "2026-09-21T00:01:00Z");
            assert_eq!(pick.matched, 2, "both DMs matched — one was outranked, not ignored");
        }

        // Shape 2 — the request-DM poll (roles A and C·2): production's builder and parser, never
        // a hand-rolled wire copy (the QURATOR-183 discipline this file already pins). A
        // NEWER non-matching DM beside the matches must not win — "newest DM" without the
        // predicate would be a different bug.
        let ask_json = |nonce: &str| {
            crate::commands::chat::build_manifest_request(
                CARRY_SLUG,
                "",
                None,
                None,
                Some(nonce.to_string()),
            )
            .expect("build manifest request")
        };
        let ask_inspect = |msg: &crate::commands::chat::ReceivedMessage| -> Option<String> {
            let body = crate::auto_approve::ManifestRequestBody::parse(&msg.content)?;
            Some(body.ask_nonce.clone().unwrap_or_default())
        };
        let ask_old = dm(ask_json("nonce-old"), "2026-09-21T00:00:00Z");
        let ask_new = dm(ask_json("nonce-new"), "2026-09-21T00:02:00Z");
        let chatter = dm("not a request".to_string(), "2026-09-21T00:03:00Z");
        let pick = select_dm_newest(&[chatter, ask_old, ask_new], ask_inspect);
        assert_eq!(
            pick.matched, 2,
            "the chatter DM must not count as a match, however new it is"
        );
        assert_eq!(
            pick.newest.expect("newest ask").0, "nonce-new",
            "the NEWEST ask must win even with a newer non-matching DM beside it"
        );
    }

    /// The ticket polls (roles C·1 and D) must reject a ticket that does not echo THIS run's
    /// `ask_nonce` — QURATOR-308, the carry twin of fetch's FD1 predicate (observed live
    /// 2026-09-06). Relay state outlives a run, so a stale-but-newest ticket from an earlier ask
    /// still decodes and still passes `verify_shape`; accepting it hands production's claim gate a
    /// ticket it rightly refuses as `Unsolicited` — a confusing red that indicts production.
    /// Newest-match (QURATOR-306) narrows that window; this predicate closes it. Drives the
    /// EXTRACTED predicate both live polls pass — not a copy of it.
    ///
    /// P-10 MUTATION (the orchestrator applies this; a worker does not run it): in
    /// `ticket_answering_ask` (this file), replace the echo-check line
    /// `if t.ask_nonce.as_deref() != Some(nonce) {` with `if false {` — the stale-nonce and
    /// nonce-less arms below must then fail (they return a ticket). Re-read the line number
    /// immediately before applying; the numbers below were re-read at hand-off.
    ///   · `ticket_answering_ask` fn:              line 362
    ///   · the `if t.ask_nonce.as_deref() …` line: line 372
    #[test]
    fn ticket_poll_rejects_a_ticket_that_does_not_echo_this_runs_nonce() {
        let ticket_json = |request_id: &str, nonce: Option<&str>| {
            let ticket = TransportTicket {
                hb: hb_core::ticket::TICKET_TAG.to_string(),
                ticket_v: hb_core::TICKET_V,
                request_id: request_id.to_string(),
                slug: CARRY_SLUG.to_string(),
                node_addr: "n0-addr".to_string(),
                issued_at: 1_700_000_000,
                ask_nonce: nonce.map(str::to_string),
                author_npub: None,
            };
            serde_json::to_string(&ticket).expect("serialize ticket")
        };
        let this_run = "nonce-now";
        // The matching-nonce ticket IS taken — the predicate must not become a total rejector.
        assert!(
            ticket_answering_ask(&dm(ticket_json("req-fresh", Some(this_run)), "2026-09-21T00:01:00Z"), this_run)
                .is_some(),
            "a ticket echoing THIS run's nonce must be selected"
        );
        // A stale-nonce ticket answering an EARLIER ask is rejected — even though it is NEWER on
        // the wire, i.e. exactly the stale-but-newest shape QURATOR-308 names.
        assert!(
            ticket_answering_ask(&dm(ticket_json("req-stale", Some("nonce-old")), "2026-09-21T00:09:00Z"), this_run)
                .is_none(),
            "a stale-nonce ticket must be rejected however new it is"
        );
        // A nonce-less ticket (a ticket answering a nonce-less ask — `issue_ticket` normalises
        // empty to None, so Some(\"\") never appears on the wire) is rejected too: both carry
        // roles always mint a nonce.
        assert!(
            ticket_answering_ask(&dm(ticket_json("req-bare", None), "2026-09-21T00:09:00Z"), this_run)
                .is_none(),
            "a nonce-less ticket must be rejected — both carry roles mint a nonce"
        );
        // Composed with the selection layer: a stale-but-newer ticket beside the matching one
        // must not count as a match at all, let alone win as the newest.
        let stale = dm(ticket_json("req-stale", Some("nonce-old")), "2026-09-21T00:09:00Z");
        let fresh = dm(ticket_json("req-fresh", Some(this_run)), "2026-09-21T00:01:00Z");
        let pick = select_dm_newest(&[stale, fresh], |m| ticket_answering_ask(m, this_run));
        assert_eq!(pick.matched, 1, "the stale ticket must not count as a match");
        assert_eq!(
            pick.newest.expect("the matching ticket").0.request_id,
            "req-fresh",
            "the selection must land on the ticket answering THIS run's ask"
        );
    }

    /// QURATOR-178 assertion #3 — the colliding-slug discriminator must tell A's tree from C's OWN
    /// same-slug decoy, and must NAME the side it matched. Seeds REAL trees through the production
    /// seeder (`crate::wan_it::generate_seed_tree`) with the literal shapes 24@0 / 7@1000 —
    /// literals, NOT the consts, so this test also reds if the consts ever drift from what role A
    /// actually seeds (the consts feed both role A's call and the expected side of the helper; only
    /// an independent literal can pin them) — reads the names back, and drives the PRODUCTION
    /// helper `imported_tree_is_the_authors`, never a copy of it. Seeding through the real seeder
    /// is also what pins `expected_seed_names` to `generate_seed_tree`'s format string: a drift in
    /// either reds here.
    ///
    /// P-10 MUTATIONS (the orchestrator applies these; a worker does not run them). Resolve by
    /// containing function (`imported_tree_is_the_authors`, in this file just above the
    /// `#[cfg(test)]` line — re-read the line numbers immediately before applying; they were
    /// ~955/~958 when written):
    ///   · swap the first arm's comparand — change `if got == author {` to `if got == decoy {` —
    ///     the 24-file author arm below errs, reddening the first `expect`.
    ///   · neutralise the decoy arm — change `} else if got == decoy {` to `} else if false {` —
    ///     the decoy tree falls through to the NEITHER branch, whose message lacks "cacher's own",
    ///     reddening the message assert.
    #[test]
    fn the_decoy_discriminator_tells_the_authors_tree_from_the_cachers_own() {
        let names = |count: usize, offset: usize| -> Vec<String> {
            let dir = tempfile::tempdir().expect("tempdir");
            crate::wan_it::generate_seed_tree(dir.path(), count, offset)
                .expect("generate_seed_tree");
            std::fs::read_dir(dir.path())
                .expect("read seed dir")
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        };
        // The author's real shape passes.
        imported_tree_is_the_authors(&names(24, 0)).expect("the author's seed shape must pass");
        // The decoy's real shape FAILS, naming the decoy side — the row's evidence must say WHICH
        // wrong tree arrived, not merely that something did.
        let err = imported_tree_is_the_authors(&names(7, 1000))
            .expect_err("the cacher's own same-slug decoy must fail");
        assert!(
            err.contains("cacher's own"),
            "the refusal must name the decoy side, got: {err}"
        );
        // A tree that is NEITHER shape (one author file swapped for a decoy file) fails on the
        // unexpected branch — a polluted seed dir must be a red, not a pass.
        let mut mixed = names(24, 0);
        mixed.pop();
        mixed.push("file-1000.bin".to_string());
        let err = imported_tree_is_the_authors(&mixed)
            .expect_err("a tree matching neither shape must fail");
        assert!(
            err.contains("NEITHER"),
            "the refusal must say the tree matches neither shape, got: {err}"
        );
    }

    /// A skipped negative must not read as a passed one: the CC1/CD1 row names must distinguish an
    /// ARMED decoy run from an unarmed one, by EXACT string — "NOT armed" contains "armed", so a
    /// substring assert would be satisfiable by the wrong branch (§9: an assertion coarser than
    /// the behaviour it names).
    ///
    /// P-10 MUTATION (the orchestrator applies this): in `decoy_status_suffix` (this file, just
    /// above the `#[cfg(test)]` line), swap the two string literals — the `if` arm returns the
    /// NOT-armed text and the `else` arm the armed text — both asserts below red.
    #[test]
    fn an_unarmed_decoy_row_must_not_read_as_an_armed_one() {
        assert_eq!(
            decoy_status_suffix(true),
            " [decoy armed: C holds its own 'wan-carry']"
        );
        assert_eq!(
            decoy_status_suffix(false),
            " [decoy NOT armed: the colliding-slug negative was NOT exercised this run]"
        );
    }
}
