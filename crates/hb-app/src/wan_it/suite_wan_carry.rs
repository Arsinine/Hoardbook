//! CARRY (QURATOR-178, harness half) — the 4-party Carrier-4 re-serve row over real relays.
//!
//! Topology (one process per party, driven by the operator in two phases):
//!
//! * **A — the author** (a JP VPS in the documented topology): seeds + publishes a collection,
//!   answers C's ordinary ask, then is **killed** by the operator. The kill is topology, not a row.
//! * **C — the cacher** (a NAS-attached AU host): phase 1 asks A and caches A's manifest; phase 2
//!   answers D's AUTHOR-ask by re-serving that cached copy while A is offline.
//! * **D — the asker** (the dev AU host): asks C for **A's** collection and redeems C's cached copy.
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
//! | C·1 | send the ask DM | `commands::chat::build_manifest_request` + `commands::chat::send_dm_inner` + `DataStore::record_manifest_ask` |
//! | C·1 | receive A's ticket | `commands::chat::decode_dms` + `hb_core::TransportTicket::verify_shape` |
//! | C·1 | redeem + cache | `commands::fulfil::redeem_manifest_ticket_inner` (claim + `ensure_endpoint` DialOnly + `fetch_manifest` + `commands::browse::accept_manifest_bytes` + spend) |
//! | C·2 | receive D's ask | `commands::chat::decode_dms` + `auto_approve::ManifestRequestBody::parse` + `auto_approve::approval_body_for` (production's parser AND its routing discriminator — QURATOR-183) |
//! | C·2 | answer by re-serving | `commands::fulfil::send_cached_manifest_inner` (cache read + `verify_author` + `ensure_endpoint` Listen + `issue_ticket` + `record_standing_grant` + `send_dm_inner`) |
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

const RELAY_TIMEOUT: Duration = Duration::from_secs(15);
const SETTLE: Duration = Duration::from_secs(3);
const DM_POLL_RETRIES: usize = 6;
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
    fn flag<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        super::args::flag_value(&self.args, name)
    }

    fn live_identity(&self) -> SharedIdentity {
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
                    "CC1: cacher asks the author and caches A's manifest (ordinary redeem)",
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
                "CD1: asker asks the cacher for the AUTHOR's collection and redeems the cached copy",
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
fn mint_ask_nonce() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

/// Send an already-built request-DM body to `recipient` via the production NIP-17 send path, and
/// record the ask locally in production ordering (record AFTER the send resolves).
async fn send_request_dm_to(
    input: &CarryInput,
    content: &str,
    recipient_npub: &str,
    asked_peer: &str,
    author_npub_for_key: &str,
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
    let sent_at = chrono::Utc::now().to_rfc3339();
    input
        .store
        .record_manifest_ask(asked_peer, author_npub_for_key, CARRY_SLUG, fingerprint_seen, &sent_at, ask_nonce)
        .map_err(|e| format!("record_manifest_ask: {e}"))?;
    Ok(())
}

/// Poll this party's DM inbox via the production unwrap path (`decode_dms`) and hand every decoded
/// message from `expected_sender` to `inspect`. Retries with a settle sleep; `inspect` returns
/// `Some(T)` when it has found what it wants (a ticket, a request).
async fn poll_dms<T>(
    input: &CarryInput,
    expected_sender: &str,
    mut inspect: impl FnMut(&crate::commands::chat::ReceivedMessage) -> Option<T>,
    what: &str,
) -> Result<T, String> {
    use crate::commands::chat::decode_dms;
    use hb_net::RelayClient;

    let own_npub = input.app_id.npub();
    let mut last_err = String::from("no poll attempt ran");
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

        let allow: HashSet<String> = [expected_sender.to_string()].into_iter().collect();
        let msgs = decode_dms(&own_npub, &input.app_id.identity, wraps, Some(&allow)).await;
        eprintln!("   carry DM poll (for {what}) attempt {attempt}: {} decoded DM(s) from sender", msgs.len());
        for msg in &msgs {
            if let Some(found) = inspect(msg) {
                return Ok(found);
            }
        }
        tokio::time::sleep(SETTLE).await;
        last_err = format!("attempt {attempt}: no {what} DM yet");
    }
    Err(format!("never received {what} from {expected_sender}: {last_err}"))
}

/// Redeem a ticket through the FULL production redeem body — claim gate, dial-only endpoint,
/// fetch, `accept_manifest_bytes` (which writes the cache that is this row's subject), and the
/// spend — with a bounded retry. A failed attempt leaves the ask retryable (the claim is keyed to
/// the same request_id, and `claim_manifest_ask` re-grants on the same id), so retries are safe.
async fn redeem_via_production(
    input: &CarryInput,
    ticket: &TransportTicket,
    newest_fingerprint: Option<&str>,
) -> Result<ImportedManifest, String> {
    let live = input.live_identity();
    let store = input.store.clone();
    let endpoint = new_shared_endpoint();
    let npub = input.app_id.npub();
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
    super::generate_seed_tree(std::path::Path::new(&seed_dir), 24, 0)
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

    // (2) Publish the teaser — the same production composition `publish_e2e_teaser` performs
    // (prepare_listing + publish_listing_capped), re-composed here only because that helper
    // hardcodes E2E_SLUG. The functions called are production.
    {
        use crate::commands::collection::{prepare_listing, LISTING_MAX_BYTES};
        use hb_net::publish_listing_capped;
        let listing_json = prepare_listing(CARRY_SLUG, &input.store).map_err(|e| format!("prepare listing: {e}"))?;
        let shared_relay = crate::net::new_shared();
        let client = crate::net::client(&input.app_id.identity, &input.store, &shared_relay)
            .await
            .map_err(|e| format!("connect for publish: {e:#}"))?;
        let published = publish_listing_capped(
            &client,
            &input.app_id.identity,
            CARRY_SLUG,
            input.app_id.browse_key.bytes(),
            &listing_json,
            LISTING_MAX_BYTES,
        )
        .await
        .map_err(|e| format!("publish carry teaser: {e}"))?;
        eprintln!(
            "   CA1 teaser published: {} part(s), truncated={}, to {} relay(s)",
            published.parts,
            published.truncated,
            input.relays.len()
        );
    }

    // (3) Print the identity facts the operator needs to configure roles C and D (A's npub and
    // full share code). The listening manifest endpoint itself is bound inside `approve_request`
    // below; its accept loop holds its own endpoint handle once spawned.
    let own_npub = input.app_id.npub();
    let share_code = input
        .app_id
        .share_code()
        .map_err(|e| format!("share code: {e}"))?;
    println!("# carry-A npub:  {own_npub}");
    println!("# carry-A share: {share_code}");

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
/// `accept_manifest_bytes`). Flags: `--phase 1 --author-npub <A npub> --author-share-code <hbk…>`.
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
    send_request_dm_to(input, &content, &author_npub, &author_npub, &author_npub, "", &nonce)
        .await?;
    eprintln!("   CC1 sent the ordinary ask to the author (nonce={nonce})");

    // Await A's ticket. A's ticket has author_npub == None (own-collection serve) — asserted, so
    // this leg really is the ordinary carrier before Carrier 4 enters.
    let ticket = poll_dms(
        input,
        &author_npub,
        |msg| {
            let trimmed = msg.content.trim();
            if !trimmed.starts_with('{') {
                return None;
            }
            let t: TransportTicket = serde_json::from_str(trimmed).ok()?;
            t.verify_shape().ok()?;
            Some(t)
        },
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
    let imported = redeem_via_production(input, &ticket, None).await?;
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
    verify_cached_under(input, &author_npub)?;
    println!("# carry-C cache primed for author {author_npub} / slug {CARRY_SLUG}");
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
    // grant and no human. The harness's remaining deviation is only that it has no caps pacer.
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

    // Save D as a contact (realism — `send_cached_manifest_inner` records the grant keyed by D).
    super::save_asker_contact(&input.store, &asker_npub)
        .map_err(|e| format!("save asker contact: {e:#}"))?;

    // THE production re-serve body: cache read + verify-under-A + Listen endpoint + ticket with
    // author_npub = Some(A) + standing grant + ticket DM, all inside.
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

    // Anti-"green while writing no grant" evidence (the QURATOR-137 lesson): the re-serve body must
    // have written the standing grant keyed (D, Some(A), slug) BEFORE the DM.
    let grant = input
        .store
        .standing_grant_for(&asker_npub, Some(&author_npub), &slug)
        .map_err(|e| format!("standing_grant_for: {e}"))?;
    if grant.is_none() {
        return Err("send_cached_manifest_inner completed but no standing grant exists for \
             (asker, author, slug) — the re-serve wrote no authorization"
            .to_string());
    }
    eprintln!("   CC2 standing grant present for (D, Some(A), '{slug}')");

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
/// and NOT under C's. Flags: `--carrier-npub <C npub> --carrier-share-code <hbk…> --author-npub <A
/// npub>`.
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

    // C must be a contact with its browse key for `accept_manifest_bytes` to decrypt C-carried
    // envelopes — production D would have C's share code already.
    save_peer_contact(input, &carrier_npub, &carrier_share_code)?;

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
    send_request_dm_to(input, &content, &carrier_npub, &carrier_npub, &author_npub, "", &nonce)
        .await?;
    eprintln!("   CD1 sent the author-ask to the cacher (nonce={nonce})");

    // Await C's ticket — it MUST name A (`author_npub == Some(A)`): that field is what makes the
    // redeem pin the manifest to A and the cache land under A's key.
    let ticket = poll_dms(
        input,
        &carrier_npub,
        |msg| {
            let trimmed = msg.content.trim();
            if !trimmed.starts_with('{') {
                return None;
            }
            let t: TransportTicket = serde_json::from_str(trimmed).ok()?;
            t.verify_shape().ok()?;
            Some(t)
        },
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

    // Redeem through the full production body. The claim resolves the author from the ticket (A),
    // matching the ask record above; `accept_manifest_bytes` pins to A and caches under A.
    let imported = redeem_via_production(input, &ticket, None).await?;

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
    verify_cached_under(input, &author_npub)?;
    let carrier_pk = hb_core::identity::parse_npub(&carrier_npub)
        .map_err(|e| format!("parse carrier npub: {e}"))?;
    let json = read_cached(input, &author_npub)?;
    let envelope = hb_core::manifest::ManifestEnvelope::from_json(&json)
        .map_err(|e| format!("parse cached envelope: {e}"))?;
    if envelope.verify_author(&carrier_pk).is_ok() {
        return Err("the cached copy verifies under the CARRIER's key — the re-serve passed off C's \
                    own manifest as A's"
            .to_string());
    }
    eprintln!("   CD1 cache copy verifies under the AUTHOR's key and refuses the carrier's");

    println!("# carry-D imported author {author_npub} / slug {CARRY_SLUG} via carrier {carrier_npub}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared tail helpers
// ---------------------------------------------------------------------------

/// Save `peer_npub` as a contact carrying the full share code's browse key — the contact shape
/// `accept_manifest_bytes` loads (same construction as `suite_wan_e2e::build_probe_input`).
fn save_peer_contact(input: &CarryInput, peer_npub: &str, share_code_str: &str) -> Result<(), String> {
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
fn read_cached(input: &CarryInput, npub: &str) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::manifest_cache::get_latest(&input.store.manifest_cache_dir(), npub, CARRY_SLUG, now)
        .ok_or_else(|| format!("no cached copy for ({npub}, {CARRY_SLUG}) — accept_manifest_bytes never wrote it"))
}

/// Assert the cached copy for `(npub, slug)` exists and verifies under that npub's x-only key —
/// the exact read + verify `send_cached_manifest_inner` performs before re-serving.
fn verify_cached_under(input: &CarryInput, npub: &str) -> Result<(), String> {
    let json = read_cached(input, npub)?;
    let envelope = hb_core::manifest::ManifestEnvelope::from_json(&json)
        .map_err(|e| format!("parse cached envelope: {e}"))?;
    let pk = hb_core::identity::parse_npub(npub).map_err(|e| format!("parse npub: {e}"))?;
    envelope
        .verify_author(&pk)
        .map_err(|e| format!("cached copy does not verify under {npub}: {e}"))
}

// ---------------------------------------------------------------------------
// Unit tests — pure parts (no network, no iroh endpoint)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
}
