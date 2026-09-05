//! FETCH (QURATOR-164 item 3) — the BACKGROUND FETCH DRIVER over real relays.
//!
//! Proves the thing the unit tests structurally cannot: that a running node notices a collection it
//! holds has moved to a new snapshot and **completes the refresh with nobody clicking anything**.
//!
//! Topology (two parties, driven by the operator in two phases):
//!
//! * **A — the author**: seeds + publishes a collection, answers D's ordinary ask, then on phase 2
//!   REPUBLISHES a changed tree (a new `snapshot_fingerprint`) and answers the ask the driver
//!   itself originates.
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
//! | A | answer an ask | `wan_it::approve_request` → `commands::fulfil::send_full_list_inner` |
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
    poll_dms, redeem_via_production, save_peer_contact, send_request_dm_to, verify_cached_under,
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
    let asker_npub = input
        .flag("--asker-npub")
        .ok_or_else(|| "role a phase 1 requires --asker-npub <driver-node npub>".to_string())?
        .to_string();

    publish_tree(input, &seed_dir, 24, 0, "FA1").await?;

    let own_npub = input.app_id.npub();
    let share_code = input.app_id.share_code().map_err(|e| format!("share code: {e}"))?;
    println!("# fetch-A npub:  {own_npub}");
    println!("# fetch-A share: {share_code}");

    answer_one_ask(input, &asker_npub, "FA1").await?;
    hold("FA1").await;
    Ok(())
}

/// Republish a CHANGED tree, then answer the ask the driver originates.
async fn run_role_a_phase2(input: &CarryInput) -> Result<(), String> {
    let seed_dir = input
        .flag("--seed-dir")
        .ok_or_else(|| "role a requires --seed-dir <dir>".to_string())?
        .to_string();
    let asker_npub = input
        .flag("--asker-npub")
        .ok_or_else(|| "role a phase 2 requires --asker-npub <driver-node npub>".to_string())?
        .to_string();

    // A DIFFERENT tree — 31 files rather than 24. The fingerprint moves because the CONTENT moved,
    // which is what the driver watches. A harness that wrote a fingerprint directly would prove
    // nothing about the production path that derives one.
    publish_tree(input, &seed_dir, 31, 1, "FA2").await?;
    eprintln!("   FA2 republished — the snapshot fingerprint has moved; the driver should notice");

    answer_one_ask(input, &asker_npub, "FA2").await?;
    hold("FA2").await;
    Ok(())
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

/// Wait for one request-DM and answer it through the production approval body.
async fn answer_one_ask(input: &CarryInput, asker_npub: &str, row: &str) -> Result<(), String> {
    eprintln!("   {row} waiting for the driver node's request-DM (polling)...");
    let (slug, nonce) = poll_dms(
        input,
        asker_npub,
        |msg| {
            // Production's parser, never a copy (QURATOR-183).
            let body = crate::auto_approve::ManifestRequestBody::parse(&msg.content)?;
            Some((body.slug.clone(), body.ask_nonce.clone()))
        },
        "manifest request from the driver node",
    )
    .await?;
    if slug != FETCH_SLUG {
        return Err(format!("the driver node asked for slug '{slug}', not '{FETCH_SLUG}'"));
    }
    let shared_relay = crate::net::new_shared();
    super::approve_request(
        &input.store,
        &input.live_identity(),
        &shared_relay,
        asker_npub,
        &slug,
        nonce.as_deref(),
    )
    .await
    .map_err(|e| format!("approve_request (send_full_list_inner): {e:#}"))?;
    eprintln!("   {row} answered via send_full_list_inner — ticket minted + DM'd");
    Ok(())
}

/// Stay up so the asker can dial this process's iroh endpoint.
async fn hold(row: &str) {
    eprintln!("   {row} holding for the redeem — kill this process when the row reports done");
    for _ in 0..180 {
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
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

    println!("# fetch-D npub:  {}", input.app_id.npub());

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
    send_request_dm_to(input, &content, &author_npub, &author_npub, &author_npub, "", &nonce)
        .await?;
    eprintln!("   FD1 sent the ordinary ask (nonce={nonce})");

    let ticket = poll_dms(
        input,
        &author_npub,
        |msg| {
            let trimmed = msg.content.trim();
            if !trimmed.starts_with('{') {
                return None;
            }
            let t: hb_core::TransportTicket = serde_json::from_str(trimmed).ok()?;
            t.verify_shape().ok()?;
            Some(t)
        },
        "ticket from the author",
    )
    .await?;
    redeem_via_production(input, &ticket, None).await?;

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
    for attempt in 1..=DRIVER_POLLS {
        let outcome = poll_once(&input.store, &live, &shared_relay, &endpoint, &mut states).await;
        eprintln!(
            "   FD2 poll {attempt}: stale={} asked={:?} redeemed={:?}",
            outcome.stale.len(),
            outcome.asked,
            outcome.redeemed
        );
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
        return Err(format!(
            "the driver never asked the author across {DRIVER_POLLS} polls — either it did not \
             notice the fingerprint move, or the wave did not include the author"
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
    verify_cached_under(input, &author_npub)?;
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
