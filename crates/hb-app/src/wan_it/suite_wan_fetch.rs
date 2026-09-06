//! FETCH (QURATOR-164 item 3) — the BACKGROUND FETCH DRIVER over real relays.
//!
//! Proves the thing the unit tests structurally cannot: that a running node notices a collection it
//! holds has moved to a new snapshot and **completes the refresh with nobody clicking anything**.
//!
//! Topology (two parties, driven by the operator in two phases):
//!
//! * **A — the author**: seeds + publishes a collection and answers asks continuously; on phase 2
//!   it REPUBLISHES a changed tree (a new `snapshot_fingerprint`) and keeps answering, including
//!   the ask the driver itself originates.
//!   ⚠ A needs NO flag naming D. It answers whoever asks, exactly as the auto-approve loop does,
//!   which is what removes the circular bootstrap the roles used to have (A wanting D's npub while
//!   D wanted A's).
//! * **D — the driver node**: phase 1 caches A's collection at the OLD fingerprint; phase 2 runs
//!   `fetch_driver::poll_once` and must, unattended, notice the change, ask, and redeem.
//!
//! Two parties on purpose. The carrier claim (D prefers a third-party cache while A is offline) is
//! a strictly larger row needing a third host, and it cannot be meaningful until this one passes:
//! if the driver cannot complete an unattended refresh from the author, it cannot complete one from
//! a carrier either.
//!
//! ## Per-step production-function map (the anti-"harness re-implements the body" table)
//!
//! | Role | Step | Production function |
//! |------|------|----------------------|
//! | A | seed the collection | `wan_it::seed_collection` → `commands::collection::scan_selective` + `DataStore::save_collection_draft` |
//! | A | publish the teaser | `commands::collection::prepare_listing` + `hb_net::publish_listing_capped` |
//! | A | REPUBLISH (new fingerprint) | the same two, over a regenerated seed tree — the fingerprint moves because the TREE moved, never because the harness wrote one |
//! | A | answer asks (continuously) | `wan_it::run_auto_approve_loop` → `commands::fulfil::send_full_list_inner` |
//! | D·1 | ask + cache | `commands::chat::build_manifest_request` + `send_dm_inner`, then `commands::fulfil::redeem_manifest_ticket_inner` |
//! | D·2 | **notice, ask, redeem** | `fetch_driver::poll_once` — the whole row. It reads `manifest_cache::list`, resolves A's listing via `commands::browse::resolve_peer`, decides with `peer_wave::next_action`, asks via `commands::chat::request_manifest_from_inner`, and redeems via `commands::fulfil::redeem_manifest_ticket_inner` |
//! | D·2 | verify under A | `manifest_cache::get_latest` + `hb_core::ManifestEnvelope::verify_author` |
//!
//! ⚠ **The harness supplies NO step of the driver's own work.** `poll_once` is called once and
//! asserted on; every decision inside it is production's. That is deliberate and it is the whole
//! value of the row — a harness that hand-sent the ask or hand-redeemed the ticket would go green
//! while the shipped app sat inert, which is exactly the phantom this project has hit three times.
//!
//! ## Not a CI gate
//!
//! Same status as every `wan_it` suite: manual pre-release harness, never wired into CI.

use std::collections::HashMap;
use std::time::Duration;

use crate::fetch_driver::{poll_once, AskState};
use crate::wan_it::suite_wan_carry::{
    redeem_via_production, save_peer_contact, send_request_dm_to, verify_cached_under,
    CarryInput,
};
use crate::wan_it::tap::Tap;

/// The slug this suite publishes. Distinct from every other suite's so a shared relay set cannot
/// cross-contaminate one row with another's listing.
const FETCH_SLUG: &str = "wan-fetch";

/// How many polls to give the driver before calling the redeem owed. Each poll is one relay round
/// trip; A has to mint and DM a ticket in between.
const DRIVER_POLLS: usize = 8;
/// Between polls. The production loop waits 5 minutes; the row does not, because the cadence is
/// pinned by a unit test and re-proving it here would only cost wall clock.
const POLL_SETTLE: Duration = Duration::from_secs(5);
/// DM poll attempts and the wait between them, for [`poll_dms_newest`].
///
/// 20 x 3s = a full minute, deliberately more patient than carry's 6 attempts. The phases here are
/// sequenced BY HAND across two terminals, and the window has to be wide enough for the operator to
/// start the second role after the first is already polling. Too short a window is what makes the
/// stale-ask race likely: A gives up, or settles on an OLD ask, before D's new one arrives.
const DM_RETRIES: usize = 20;
const DM_SETTLE: Duration = Duration::from_secs(3);
const DM_TIMEOUT: Duration = Duration::from_secs(15);

/// Poll for DMs from `expected_sender` and return the **NEWEST** match by `sent_at`, not the first.
///
/// ⚠ **This exists because RELAY STATE OUTLIVES A RUN, and carry's `poll_dms` returns the FIRST
/// match it decodes.** A failed or repeated phase leaves earlier asks and tickets on the relay,
/// addressed to the same npubs and still perfectly decodable. On 2026-09-06 that made role A answer
/// a SUPERSEDED ask: the ticket it minted echoed a stale `ask_nonce`, and D's production claim gate
/// refused it as `Unsolicited` — the gate working exactly as designed, reported as a harness
/// failure. Re-running a phase must be idempotent, and taking the first match is not.
async fn poll_dms_newest<T>(
    input: &CarryInput,
    expected_sender: &str,
    mut inspect: impl FnMut(&crate::commands::chat::ReceivedMessage) -> Option<T>,
    what: &str,
) -> Result<T, String> {
    use crate::commands::chat::decode_dms;
    use hb_net::RelayClient;
    use nostr::prelude::*;
    use std::collections::HashSet;

    let own_npub = input.app_id.npub();
    let allow: HashSet<String> = [expected_sender.to_string()].into_iter().collect();
    let mut wraps_seen = 0usize;
    let mut decoded_seen = 0usize;

    for attempt in 1..=DM_RETRIES {
        if let Ok(client) =
            RelayClient::connect(&input.app_id.identity, &input.relays, DM_TIMEOUT).await
        {
            let wraps = client
                .fetch(
                    Filter::new().kind(Kind::GiftWrap).pubkey(input.app_id.identity.public_key()),
                    DM_TIMEOUT,
                )
                .await;
            client.disconnect().await;
            if let Ok(wraps) = wraps {
                wraps_seen = wraps_seen.max(wraps.len());
                let msgs = decode_dms(&own_npub, &input.app_id.identity, wraps, Some(&allow)).await;
                decoded_seen = decoded_seen.max(msgs.len());
                // Every match, then the newest by the rumor's own send time — NOT the first.
                let mut hits: Vec<(String, T)> = msgs
                    .iter()
                    .filter_map(|m| inspect(m).map(|t| (m.sent_at.clone(), t)))
                    .collect();
                eprintln!(
                    "   fetch DM poll (for {what}) attempt {attempt}: {} wrap(s), {} decoded, {} match(es)",
                    wraps_seen,
                    msgs.len(),
                    hits.len()
                );
                if !hits.is_empty() {
                    hits.sort_by(|a, b| a.0.cmp(&b.0));
                    let (sent_at, found) = hits.pop().expect("non-empty");
                    eprintln!("   fetch DM poll: taking the NEWEST match (sent_at={sent_at})");
                    return Ok(found);
                }
            }
        }
        tokio::time::sleep(DM_SETTLE).await;
    }

    // Wraps arrived but none decoded from the expected sender: an npub mismatch, not a lost DM.
    if wraps_seen > 0 && decoded_seen == 0 {
        return Err(format!(
            "never received {what} from {expected_sender}: {wraps_seen} gift-wrap(s) DID arrive but \
             none decoded as being from that npub — the DM reached this node and was FILTERED OUT, \
             not lost. Compare the npub the sending role printed at ITS startup against the one on \
             this command line; an identity is minted per --data-dir, so a data-dir that was \
             deleted or moved mints a NEW npub that silently stops matching."
        ));
    }
    Err(format!(
        "never received {what} from {expected_sender} after {DM_RETRIES} attempts \
         ({wraps_seen} wrap(s) seen, {decoded_seen} decoded)"
    ))
}

/// Dispatch on `--role`. The operator sequences the phases by hand:
/// phase 1 — A up, D asks and caches; then A phase 2 republishes; then D phase 2 polls.
pub async fn run(tap: &mut Tap, role: &str, input: &CarryInput) {
    let phase = input.flag("--phase").unwrap_or("1").to_string();
    match role {
        "a" => {
            if phase == "1" {
                tap.check(
                    "FA1: author seeds + publishes + answers the driver node's ordinary ask",
                    run_role_a_phase1(input).await,
                );
            } else {
                tap.check(
                    "FA2: author REPUBLISHES a changed tree, then answers the ask the DRIVER sent",
                    run_role_a_phase2(input).await,
                );
            }
        }
        "d" => {
            if phase == "1" {
                tap.check(
                    "FD1: driver node caches the author's collection at the OLD fingerprint",
                    run_role_d_phase1(input).await,
                );
            } else {
                tap.check(
                    "FD2/FD3: poll_once notices the change, asks UNATTENDED, and redeems UNATTENDED",
                    run_role_d_phase2(input).await,
                );
            }
        }
        other => {
            tap.check(
                format!("FETCH: unknown --role '{other}' (expected a|d)"),
                Err("unknown role".to_string()),
            );
        }
    }
}

/// Seed + publish + print the identity facts + answer one ask.
async fn run_role_a_phase1(input: &CarryInput) -> Result<(), String> {
    let seed_dir = input
        .flag("--seed-dir")
        .ok_or_else(|| "role a requires --seed-dir <dir>".to_string())?
        .to_string();
    publish_tree(input, &seed_dir, 24, 0, "FA1").await?;

    // The npub + share code are printed by the probe entry before role dispatch, so they are
    // available even on a run that fails its flag checks.
    serve_asks(input, "FA1").await
}

/// Republish a CHANGED tree, then answer the ask the driver originates.
async fn run_role_a_phase2(input: &CarryInput) -> Result<(), String> {
    let seed_dir = input
        .flag("--seed-dir")
        .ok_or_else(|| "role a requires --seed-dir <dir>".to_string())?
        .to_string();
    // A DIFFERENT tree. `generate_seed_tree` does NOT wipe — it writes `file-{offset+i}.bin` over
    // whatever is already there — so 31 files at offset 1 land alongside phase 1's 24 at offset 0
    // and the tree ends at 32 entries, not 31. Either way the fingerprint moves because the CONTENT
    // moved, which is what the driver watches. A harness that wrote a fingerprint directly would
    // prove nothing about the production path that derives one.
    //
    // ⚠ --seed-dir is SCANNED IN FULL and PUBLISHED as a public teaser (subdirectories included,
    // Windows junctions followed). Point it at a scratch directory, never at real collection data.
    publish_tree(input, &seed_dir, 31, 1, "FA2").await?;
    eprintln!("   FA2 republished — the snapshot fingerprint has moved; the driver should notice");

    serve_asks(input, "FA2").await
}

/// Generate a seed tree, scan it into a draft, and publish the teaser — all production functions.
async fn publish_tree(
    input: &CarryInput,
    seed_dir: &str,
    files: usize,
    seed: usize,
    row: &str,
) -> Result<(), String> {
    super::generate_seed_tree(std::path::Path::new(seed_dir), files, seed)
        .map_err(|e| format!("generate seed tree: {e:#}"))?;
    super::seed_collection(
        &input.store,
        &input.app_id.identity,
        &input.app_id.browse_key,
        seed_dir,
        FETCH_SLUG,
    )
    .map_err(|e| format!("seed collection: {e:#}"))?;

    use crate::commands::collection::{prepare_listing, LISTING_MAX_BYTES};
    use hb_net::publish_listing_capped;
    let listing_json =
        prepare_listing(FETCH_SLUG, &input.store).map_err(|e| format!("prepare listing: {e}"))?;
    let shared_relay = crate::net::new_shared();
    let client = crate::net::client(&input.app_id.identity, &input.store, &shared_relay)
        .await
        .map_err(|e| format!("connect for publish: {e:#}"))?;
    let published = publish_listing_capped(
        &client,
        &input.app_id.identity,
        FETCH_SLUG,
        input.app_id.browse_key.bytes(),
        &listing_json,
        LISTING_MAX_BYTES,
    )
    .await
    .map_err(|e| format!("publish fetch teaser: {e}"))?;
    eprintln!(
        "   {row} '{FETCH_SLUG}' seeded from {seed_dir} ({files} files) and published: {} part(s), truncated={}",
        published.parts, published.truncated
    );
    Ok(())
}

/// Answer asks CONTINUOUSLY, through the production auto-approve loop, until the operator kills
/// this process.
///
/// ⚠ **It must not answer one ask and stop.** An earlier cut did, and it made the choreography
/// order-dependent in a way no amount of "take the newest" could fix: relay state outlives a run,
/// so when A started it found four SUPERSEDED asks already sitting there, answered the newest of
/// those, and went idle — before D had sent anything at all. D's fresh ask then arrived with nobody
/// listening (observed live, 2026-09-06). Production has never behaved that way: `auto_approve`
/// answers every request-DM it sees, for as long as the app runs.
///
/// This is `wan_it::run_auto_approve_loop`, the same helper the `--auto-approve` serve drives, so
/// every approval still goes through `send_full_list_inner`. It also keeps this process up for the
/// asker's dial, which is what the old explicit hold was for.
async fn serve_asks(input: &CarryInput, row: &str) -> Result<(), String> {
    eprintln!("   {row} serving asks continuously (production auto-approve loop) — kill this process when the row reports done");
    let shared_relay = crate::net::new_shared();
    super::run_auto_approve_loop(
        &input.store,
        &input.live_identity(),
        &shared_relay,
        &input.relays,
    )
    .await
    .map_err(|e| format!("auto-approve loop: {e:#}"))
}

/// FD1: an ordinary ask + redeem, so the driver node holds the collection at the OLD fingerprint.
async fn run_role_d_phase1(input: &CarryInput) -> Result<(), String> {
    let author_npub = input
        .flag("--author-npub")
        .ok_or_else(|| "role d phase 1 requires --author-npub <A npub>".to_string())?
        .to_string();
    let author_share_code = input
        .flag("--author-share-code")
        .ok_or_else(|| "role d phase 1 requires --author-share-code <hbk…>".to_string())?
        .to_string();

    // A must be a contact WITH the browse key: `accept_manifest_bytes` reads it to decrypt, and
    // `poll_once` reads it again in phase 2 to resolve A's listing.
    save_peer_contact(input, &author_npub, &author_share_code)?;

    let nonce = crate::wan_it::suite_wan_carry::mint_ask_nonce();
    let content = crate::commands::chat::build_manifest_request(
        FETCH_SLUG,
        "",
        None,
        None,
        Some(nonce.clone()),
    )?;
    // FETCH_SLUG, not carry's. The helper hardcoded a slug until 2026-09-06 and this is the call
    // that exposed it: the ask was recorded under the wrong key and every redeem came back
    // "That link doesn't answer a request you sent".
    send_request_dm_to(
        input,
        &content,
        &author_npub,
        &author_npub,
        &author_npub,
        FETCH_SLUG,
        "",
        &nonce,
    )
    .await?;
    eprintln!("   FD1 sent the ordinary ask (nonce={nonce})");

    // ⚠ The ticket must echo THIS run's nonce. `ask_nonce` exists precisely to bind a ticket to ONE
    // ask, and `claim_manifest_ask` refuses any other as `Unsolicited`. A stale ticket answering a
    // SUPERSEDED ask is still on the relay and still decodes fine, so accepting the first valid one
    // hands the production claim gate a ticket it is right to reject — which reads as a product
    // failure and is not one (observed live, 2026-09-06).
    let ticket = poll_dms_newest(
        input,
        &author_npub,
        |msg| {
            let trimmed = msg.content.trim();
            if !trimmed.starts_with('{') {
                return None;
            }
            let t: hb_core::TransportTicket = serde_json::from_str(trimmed).ok()?;
            t.verify_shape().ok()?;
            if t.ask_nonce.as_deref() != Some(nonce.as_str()) {
                return None;
            }
            Some(t)
        },
        "ticket answering THIS run's ask",
    )
    .await?;
    // The ticket came from A, so A is the claim key's peer segment — the ask was recorded against A.
    redeem_via_production(input, &author_npub, &ticket, None).await?;

    let held = held_fingerprint(input, &author_npub)?;
    eprintln!("   FD1 cached '{FETCH_SLUG}' at fingerprint {held}");
    println!("# fetch-D holds: {held}");
    Ok(())
}

/// FD2/FD3 — the row. Drive `poll_once` and assert the driver did BOTH halves itself.
async fn run_role_d_phase2(input: &CarryInput) -> Result<(), String> {
    let author_npub = input
        .flag("--author-npub")
        .ok_or_else(|| "role d phase 2 requires --author-npub <A npub>".to_string())?
        .to_string();

    let before = held_fingerprint(input, &author_npub)?;
    eprintln!("   FD2 holding {before} before the first poll");

    let live = input.live_identity();
    let shared_relay = crate::net::new_shared();
    let endpoint = crate::transport_state::new_shared_endpoint();
    // The driver's own attempt state, carried across polls exactly as the production loop carries
    // it — that is what makes the backoff and the 3-try cap mean anything here.
    let mut states: HashMap<(String, String), AskState> = HashMap::new();

    let mut asked_author = false;
    let mut redeemed = false;
    let mut saw_stale = false;
    for attempt in 1..=DRIVER_POLLS {
        let outcome = poll_once(&input.store, &live, &shared_relay, &endpoint, &mut states).await;
        eprintln!(
            "   FD2 poll {attempt}: stale={} asked={:?} redeemed={:?}",
            outcome.stale.len(),
            outcome.asked,
            outcome.redeemed
        );
        if !outcome.stale.is_empty() {
            saw_stale = true;
        }
        if outcome.asked.iter().any(|n| n == &author_npub) {
            asked_author = true;
        }
        if outcome.redeemed.iter().any(|s| s == FETCH_SLUG) {
            redeemed = true;
            break;
        }
        tokio::time::sleep(POLL_SETTLE).await;
    }

    if !asked_author {
        // ⚠ Distinguish NOTHING TO DO from a broken watch. `stale=0` on every poll means the held
        // fingerprint already equals the published one — overwhelmingly because a previous phase-2
        // run SUCCEEDED and advanced the cache, which is a passing row being re-run, not a failure.
        // The message used to offer only two causes, both of them defects, and sent the operator
        // looking for a bug in a driver that had correctly done nothing.
        if !saw_stale {
            return Err(format!(
                "nothing was stale across {DRIVER_POLLS} polls: this node already holds {before}, \
                 which is what the author currently publishes — so there is nothing to fetch and \
                 the driver was right not to ask. This is what a SUCCEEDED phase 2 looks like when \
                 it is re-run. To exercise the row again, reset the CONTENT and not the identities: \
                 delete the --seed-dir, then re-run role a phase 1, role d phase 1, role a phase 2, \
                 role d phase 2. Keep both --data-dir values — deleting those mints new npubs."
            ));
        }
        return Err(format!(
            "the driver saw a stale holding but never asked the author across {DRIVER_POLLS} polls \
             — the wave did not include the author, or every attempt was still backing off"
        ));
    }
    eprintln!("   FD2 the driver originated the ask ITSELF — no operator step sent it");

    if !redeemed {
        return Err(format!(
            "the driver asked but never redeemed across {DRIVER_POLLS} polls — the ask half is \
             unattended and the fetch half is not, which is the defect this row exists to catch"
        ));
    }

    let after = held_fingerprint(input, &author_npub)?;
    if after == before {
        return Err(format!(
            "the driver reported a redeem but the cached fingerprint is unchanged ({after}) — the \
             refresh did not land"
        ));
    }
    eprintln!("   FD3 the cache moved {before} -> {after}, unattended");

    // The re-served envelope must still verify under A's key, not whoever handed it over.
    verify_cached_under(input, &author_npub, FETCH_SLUG)?;
    eprintln!("   FD3 the cached envelope verifies under the AUTHOR's key");
    Ok(())
}

/// The fingerprint this node currently holds for the author's fetch-suite collection.
fn held_fingerprint(input: &CarryInput, author_npub: &str) -> Result<String, String> {
    crate::manifest_cache::list(&input.store.manifest_cache_dir())
        .into_iter()
        .find(|k| k.npub == author_npub && k.slug == FETCH_SLUG)
        .map(|k| k.fingerprint)
        .ok_or_else(|| format!("nothing cached for ({author_npub}, {FETCH_SLUG})"))
}
