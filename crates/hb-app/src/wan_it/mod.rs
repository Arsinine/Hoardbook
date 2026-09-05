//! `hb-wan-it` — headless WAN integration harness (M20 W6 §W6). Drives the **real production
//! presence path** between live endpoints over real NAT-traversed Nostr relays. Never starts the
//! Tauri/webkit runtime — it lives in-crate only to reach `pub(crate)` production internals (the
//! `presence.rs` publish/fetch functions, the `hb_net::fetch_online_presence` aggregate the
//! contact-row pill reads). This is the pattern the retired `hb-p2p-it` harness proved (in-crate
//! bin, serve/probe roles, TAP 13); the payload here drives current modules, not deleted ones.
//!
//! Two roles + one daemon:
//!
//!   hb-wan-it serve --data-dir <dir> --relay <url>...
//!     Real identity seeded from <dir> (load-or-create), publishes the presence beacon via the REAL
//!     production path (`presence::publish_presence`) on the real cadence (`PRESENCE_REFRESH_SECS`).
//!     Prints its npub + share-code for the probe. Binds nothing iroh-related (WAN-M is out of
//!     scope). WAN-P's serve needs only the beacon.
//!
//!   hb-wan-it probe --peer <npub-or-share-code> --relay <url>...
//!                  [--flood-relay <url>... --flood-count <n>]
//!     Runs the WAN-P rows (P1–P5) against a live serve, TAP 13 out. `--flood-relay` arms P3.
//!
//!   hb-wan-it canary [--interval <secs>] [--once]
//!     The thin continuous slice: one pass = M4 (n0 reachable) + R2 (default-relay policy) + P2
//!     (beacon acceptance) + C1 (DM round-trip). Loop every --interval (default 600 s); --once runs
//!     a single pass (the smoke form).
//!
//! **Not in CI.** Manual pre-release gate + the canary daemon. The suite's exit code is nonzero
//! when rows fail; that is correct and intended for the pre-W1 state (P1 on public relays, P4 are
//! expected red).

mod args;
mod suite_wan_c;
mod suite_wan_carry;
mod suite_wan_d;
mod suite_wan_e2e;
mod suite_wan_fetch;
mod suite_wan_m;
mod suite_wan_p;
mod suite_wan_r;
mod suite_wan_t;
mod suite_wan_u;
mod tap;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use nostr::prelude::*;

use crate::commands::collection::{build_slug_manifest, count_items, is_valid_slug, scan_selective};
use crate::identity_state::AppIdentity;
use crate::commands::fulfil::send_full_list_inner;
use hb_core::ticket::TransportTicket;
use crate::net;
use crate::presence;
use crate::store::{DataStore, Settings};
use crate::transport::ManifestSource;
use crate::transport_state::{ensure_endpoint, new_shared_endpoint, Role};

/// Entry point — invoked by the thin `bin/hb-wan-it.rs` wrapper. Matches the retired harness's
/// `run()` shape: dispatch on the first positional, return an ExitCode.
pub(crate) async fn run() -> ExitCode {
    // The lock resolves rustls with both providers enabled (iroh `tls-ring` + reqwest aws-lc-rs),
    // so there is no automatic process default; production installs one EXPLICITLY in `lib.rs`,
    // before `spawn_background_tasks`. (It does NOT arrive as a side effect of binding the iroh
    // endpoint — the 2026-08-04 v0.12.11 log falsified that claim; it was a race the app lost 12
    // times in 32 s at launch. See the `rustls` entry in Cargo.toml.) This harness never runs that
    // startup path, so without this line every `wss://` relay connect panics. Err = a provider is
    // already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args::command(&args) {
        Some("serve") => match run_serve(&args[1..]).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("[serve] error: {e:#}");
                ExitCode::FAILURE
            }
        },
        Some("probe") => match run_probe(&args[1..]).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("[probe] error: {e:#}");
                ExitCode::FAILURE
            }
        },
        Some("canary") => match run_canary(&args[1..]).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("[canary] error: {e:#}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

/// The usage banner — printed on no-args or an unknown command.
fn usage() -> &'static str {
    "usage: hb-wan-it <serve|probe|canary> [options]\n\
     \n\
     WAN integration harness (M20 W6). Drives the REAL production paths (presence + manifest plane)\n\
     between live endpoints. Not in CI — run as a manual pre-release gate (probe from NAT-A against\n\
     serve on NAT-B and the VPSes) or via the canary daemon.\n\
     \n\
     serve  --data-dir <dir> --relay <ws-url>...\n\
            [--seed-dir <dir> --asker-npub <npub>]\n\
            [--auto-approve --e2e-seed-dir <dir> [--republish]]\n\
            Seeds/loads an identity from <dir>, publishes the presence beacon via the production\n\
            path (presence::publish_presence) on the real cadence. Prints npub + share-code.\n\
            With --seed-dir + --asker-npub: also seeds a collection from <dir>, then\n\
            approves through the production command body (send_full_list_inner): binds the endpoint,\n\
            mints + records the ticket, and DMs it to --asker-npub. The probe reads it from its own\n\
            inbox — the launch-gate WAN-M path, now unattended (no ticket=<json> copy-paste).\n\
            With --auto-approve + --e2e-seed-dir: seeds a TRUNCATING collection (large enough to\n\
            exceed the 40 KB teaser budget), publishes the teaser, and runs the auto-approve loop:\n\
            polls the DM inbox for request-DMs and answers each by driving the production approval\n\
            body. This is the serve side of WAN-E2E. --republish rewrites the seed tree mid-run (E2).\n\
     \n\
     probe  --peer <npub|hbk…> --relay <ws-url>...\n\
            [--flood-relay <ws-url>... --flood-count <n>]\n\
            [--suite wan-m]                 ticket arrives by DM; --ticket-json only overrides it\n\
            [--suite wan-e2e]\n\
            [--suite wan-u]\n\
            [--suite carry --role a|c|d]\n\
            [--suite fetch --role a|d --phase 1|2]\n\
            [--suite wan-c]\n\
            [--suite wan-t]\n\
            [--suite wan-d [--flood-relay <ws-url>... --flood-count <n>]]\n\
            [--suite wan-r]\n\
            [--suite wan-m4]\n\
            Runs WAN-P rows P1–P5 against a live serve by default. --flood-relay arms P3\n\
            (VPS strfry only: pass them with --relay, or set HB_CANARY_RELAYS in .env).\n\
            --suite wan-m runs the WAN-M rows (M1 + M9) instead, redeeming the ticket the serve\n\
            DM'd (§W6's --ticket-json remains for targeted iroh isolation runs;\n\
            the E2E suite rides the full DM leg).\n\
            --suite wan-e2e runs the WAN-E2E rows (E1 + E2): the full DM-coordinated pipeline.\n\
            --suite wan-u runs the WAN-U rows (U1–U7): the user-surface live twins (profile,\n\
            collections, add-contact, re-key, private). Probe-plays-both — no serve needed.\n\
            --suite wan-c runs the WAN-C rows (C1–C5): chat over real relays (delivery latency,\n\
            offline catch-up, disjoint relay sets, cursor discipline, blocked drop).\n\
            --suite wan-t runs the WAN-T rows (T1–T5): topics between real clients (public join,\n\
            channel pseudonymity, private invite, leave retract, announce + local filter).\n\
            --suite wan-d runs the WAN-D rows (D1–D4): discovery against real relays (cross-region\n\
            visibility, NIP-65, search-eviction VPS-only, BIGRELAY). --flood-relay arms D3.\n\
            --suite wan-r runs the WAN-R rows (R1–R2): relay-set resilience + the default-relay\n\
            policy watch (the canary row). R2 touches the public defaults — that is its job.\n\
            --suite carry runs the CARRY rows (CA-CD): the 4-party Carrier-4 re-serve over real\n\
            relays (A publishes then goes offline; C caches + re-serves; D asks + redeems).\n\
            --role a|c|d picks the party this process plays; see the carry suite's module doc\n\
            for the two-phase choreography and the flags each role takes.\n\
            --suite fetch runs the FETCH rows (FA1-FA2, FD1-FD3): the BACKGROUND FETCH DRIVER over\n\
            real relays. A publishes, D caches, A republishes a changed tree, and D's poll_once must\n\
            notice, ask and redeem with no operator step in between. Two parties, --role a|d.\n\
            --suite wan-m4 runs the M4 row standalone (n0 canary core — bind_endpoint obtains a\n\
            home relay and the endpoint is reachable through it). Probe-plays-both — no serve needed.\n\
     \n\
     canary [--interval <secs>] [--once]\n\
            The thin continuous slice: one pass = M4 (n0 reachable) + R2 (default-relay policy) +\n\
            P2 (beacon acceptance) + C1 (DM round-trip). Loop every --interval (default 600 s), each\n\
            pass printing a timestamped one-line summary; any row failure prints [ALERT] to stderr.\n\
            --once: a single pass (the smoke uses this); exit nonzero if any row failed."
}

// ---------------------------------------------------------------------------
// serve — real identity, real beacon cadence
// ---------------------------------------------------------------------------

/// The real beacon cadence (matches `presence::PRESENCE_REFRESH_SECS` = 300 s). The serve publishes
/// on the production cadence so the probe observes realistic beacon freshness.
const SERVE_BEACON_INTERVAL: Duration = Duration::from_secs(presence::PRESENCE_REFRESH_SECS);
/// First publish fires shortly after launch (the serve needs a moment to connect).
const SERVE_FIRST_DELAY: Duration = Duration::from_secs(5);

async fn run_serve(args: &[String]) -> Result<()> {
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-data").to_string(),
    );
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("serve requires at least one --relay (the beacon is published there)");
    }

    let store = DataStore::new(data_dir.clone());
    // Persist the relay set so `net::relay_urls` (the path `run_presence_loop` reads) returns it.
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    println!("npub={}", app_id.npub());
    println!("share_code={}", app_id.share_code().map_err(|e| anyhow!(e.to_string()))?);
    eprintln!(
        "[serve] presence beacon publishing to {} relay(s) on the real {}s cadence",
        relays.len(),
        presence::PRESENCE_REFRESH_SECS
    );

    // WAN-M: when --seed-dir + --asker-npub are passed, seed a collection, add the asker as a contact
    // in good standing, and APPROVE through the production command body (`send_full_list_inner`),
    // which binds the endpoint, mints the ticket, records it, and DMs it to the asker.
    //
    // The serve no longer prints `ticket=<json>` for an operator to paste into `--ticket-json`. The
    // ticket rides the DM it rides in production, and the probe reads it out of its own inbox — so
    // the two halves are joined by the real channel rather than by a human, and WAN-M can run
    // unattended (which is what makes a Linux-serve / Windows-probe CI leg possible at all).
    if let (Some(seed_dir), Some(asker_npub)) =
        (args::flag_value(args, "--seed-dir"), args::flag_value(args, "--asker-npub"))
    {
        // AppIdentity holds ZeroizeOnDrop secrets and is not Clone; re-read it from the store so the
        // SharedIdentity owns its own copy (the live session the accept loop serves from).
        let app_id_for_plane = load_or_create_identity(&store)?;
        let live_npub: crate::identity_state::SharedIdentity =
            std::sync::Arc::new(tokio::sync::RwLock::new(Some(app_id_for_plane)));
        let shared_relay_manifest = net::new_shared();
        setup_manifest_plane(&store, &live_npub, seed_dir, asker_npub, &shared_relay_manifest)
            .await?;
    }

    // WAN-E2E serve side: when --auto-approve + --e2e-seed-dir are passed, seed a TRUNCATING collection,
    // publish the teaser, and run the auto-approve loop (poll the DM inbox for request-DMs and answer
    // each by driving the production approval body). The loop runs concurrently with the beacon below
    // via tokio::select!.
    let auto_approve = args::flag_value(args, "--auto-approve").is_some();
    let e2e_seed_dir = args::flag_value(args, "--e2e-seed-dir").map(String::from);
    if auto_approve {
        let dir = e2e_seed_dir
            .ok_or_else(|| anyhow!("--auto-approve requires --e2e-seed-dir <dir> (the seed tree to serve)"))?;
        let app_id_for_plane = load_or_create_identity(&store)?;
        let live_npub: crate::identity_state::SharedIdentity =
            std::sync::Arc::new(tokio::sync::RwLock::new(Some(app_id_for_plane)));
        // Seed the truncating collection + publish the teaser.
        setup_e2e_serve(&store, &live_npub, &dir).await?;
        // If --republish was passed, rewrite the seed tree NOW (before the loop starts) so E2's
        // changed-fingerprint leg is primed. The probe re-browses after this and sees the new fp.
        if args::flag_value(args, "--republish").is_some() {
            eprintln!("[serve] --republish: rewriting the seed tree for E2 (fingerprint will change)");
            republish_e2e_seed(&store, &live_npub, &dir).await?;
        }
        // Spawn the auto-approve loop. It runs alongside the beacon loop (both selected below).
        let store_for_approve = store.clone();
        let shared_relay_approve = net::new_shared();
        let live_npub_approve = live_npub.clone();
        let relays_approve = relays.clone();
        tokio::spawn(async move {
            if let Err(e) = run_auto_approve_loop(
                &store_for_approve,
                &live_npub_approve,
                &shared_relay_approve,
                &relays_approve,
            )
            .await
            {
                eprintln!("[serve] auto-approve loop exited: {e:#}");
            }
        });
        // The beacon loop continues in the foreground; the auto-approve loop runs in the spawned task.
        // Both publish to the same relay set via the production path. The beacon keeps presence fresh
        // while the approve loop answers request-DMs.
    }

    // Publish on the real cadence via the production path (presence::publish_presence). This is

    // Publish on the real cadence via the production path (presence::publish_presence). This is
    // the same function run_presence_loop calls; the harness drives it directly because the loop
    // is shaped for the Tauri-managed background task (AppHandle, watch channels).
    let shared_relay = net::new_shared();
    let mut delay = SERVE_FIRST_DELAY;
    loop {
        tokio::time::sleep(delay).await;
        delay = SERVE_BEACON_INTERVAL;
        match net::client(&app_id.identity, &store, &shared_relay).await {
            Ok(client) => {
                match presence::publish_presence(&client, &app_id.identity).await {
                    Ok(outcome) => {
                        let now = unix_now();
                        eprintln!(
                            "[serve] {now} beacon published: {} accepted, {} rejected",
                            outcome.accepted.len(),
                            outcome.rejected.len()
                        );
                    }
                    Err(e) => eprintln!("[serve] beacon publish failed: {e}"),
                }
            }
            Err(e) => eprintln!("[serve] no relay client this cycle: {e}"),
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// WAN-E2E serve — truncating seed, teaser publish, auto-approve loop
//
// All production paths: scan_selective + save_collection_draft (the add-collection scan), the
// production teaser publish (publish_listing_capped), the production DM inbox poll (fetch gift-wraps
// → decode_dms), and the production approval body itself (`send_full_list_inner` — the same fn the
// owner's click drives).
// ---------------------------------------------------------------------------

/// The fixed slug the E2E serve seeds (matches the probe's `SEED_SLUG`).
const E2E_SLUG: &str = "wan-e2e";

/// How many small files to generate for the truncating seed tree. Each file entry serializes to ~120
/// bytes of JSON (`{"name":"file-NNNN.bin","item_type":"File","tags":[],"children":[]}`), so 600 files
/// is ~72 KB of entries alone — comfortably over the 40 KB truncation budget. The harness generates
/// these into a temp dir at serve startup.
const E2E_SEED_FILE_COUNT: usize = 600;

/// Seed a truncating collection from `e2e_seed_dir`, generate enough small files to exceed the 40 KB
/// teaser budget, and publish the teaser via the production path (`publish_listing_capped`). Then bind
/// the manifest endpoint (the accept loop serves the full manifest when a ticket is redeemed).
async fn setup_e2e_serve(
    store: &DataStore,
    live_npub: &crate::identity_state::SharedIdentity,
    e2e_seed_dir: &str,
) -> Result<()> {
    // (1) Generate the seed tree into e2e_seed_dir. If the dir already has the files (a prior run),
    // leave them — the scan reads whatever is there. The --republish flag rewrites them.
    generate_seed_tree(Path::new(e2e_seed_dir), E2E_SEED_FILE_COUNT, 0)?;
    eprintln!(
        "[serve] E2E seed tree: {} files in {e2e_seed_dir}",
        E2E_SEED_FILE_COUNT
    );

    // (2) Scan + save the collection draft (the production add-collection path).
    let (identity, browse_key, transport_key, own_npub) = {
        let guard = live_npub.read().await;
        let id = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no identity loaded for the E2E serve"))?;
        (
            id.identity.clone(),
            id.browse_key.clone(),
            id.transport_key.clone(),
            id.npub(),
        )
    };
    seed_collection(store, &identity, &browse_key, e2e_seed_dir, E2E_SLUG)?;
    eprintln!("[serve] E2E collection '{E2E_SLUG}' seeded");

    // (3) Publish the teaser via the production path (publish_listing_capped). This is the same path
    // commands::collection::publish_collection drives: stamp the snapshot fingerprint, build the listing
    // JSON, truncate if over budget, publish the single event.
    publish_e2e_teaser(store, &identity, &browse_key).await?;
    eprintln!("[serve] E2E teaser published (truncated)");

    // (4) Bind the manifest endpoint via the production path (ensure_endpoint, Role::Listen). This is
    // the real accept loop + the in-flight set — the same binding send_full_list establishes.
    let source: Arc<dyn ManifestSource> =
        crate::manifest_source::StoreManifestSource::new(store.clone(), identity.clone(), browse_key.clone());
    let shared = new_shared_endpoint();
    let endpoint = ensure_endpoint(&shared, &own_npub, live_npub, &transport_key, source, Role::Listen)
        .await
        .map_err(|e| anyhow!("bind E2E manifest endpoint: {e}"))?;
    eprintln!("[serve] E2E manifest endpoint bound (presets::N0); accept loop running");
    // The endpoint lives for the process lifetime; the accept loop serves manifests for every approved
    // ticket. Keeping a handle so it is not dropped.
    std::mem::forget(endpoint);
    Ok(())
}

/// Publish (or re-publish) the E2E teaser via the production path. This is the same sequence
/// `commands::collection::publish_collection` drives: prepare the listing JSON (which stamps the
/// snapshot fingerprint), and call `publish_listing_capped` (which truncates if over budget).
async fn publish_e2e_teaser(
    store: &DataStore,
    identity: &hb_core::Identity,
    browse_key: &crate::identity_state::SessionBrowseKey,
) -> Result<()> {
    use crate::commands::collection::{prepare_listing, LISTING_MAX_BYTES};
    use hb_net::publish_listing_capped;

    // Build the listing JSON with the snapshot fingerprint stamped (the production path).
    let listing_json = prepare_listing(E2E_SLUG, store).map_err(|e| anyhow!(e))?;
    // Publish the teaser via the production path. publish_listing_capped truncates if the listing
    // exceeds LISTING_MAX_BYTES (40 KB), producing a single paywall-teaser event tagged `truncated`.
    let shared_relay = net::new_shared();
    let client = net::client(identity, store, &shared_relay).await?;
    let relays = net::relay_urls(store);
    let published = publish_listing_capped(
        &client,
        identity,
        E2E_SLUG,
        browse_key.bytes(),
        &listing_json,
        LISTING_MAX_BYTES,
    )
    .await
    .map_err(|e| anyhow!("publish E2E teaser: {e}"))?;
    eprintln!(
        "[serve] E2E teaser published: {} part(s), truncated={}, to {} relay(s)",
        published.parts,
        published.truncated,
        relays.len()
    );
    Ok(())
}

/// Rewrite the seed tree with a DIFFERENT file count (adds/removes files), so the snapshot fingerprint
/// changes. Then re-scan + re-save + re-publish the teaser. This is the E2 "changed fingerprint" leg:
/// the probe re-browses and sees a different fingerprint than it recorded from E1.
async fn republish_e2e_seed(
    store: &DataStore,
    live_npub: &crate::identity_state::SharedIdentity,
    e2e_seed_dir: &str,
) -> Result<()> {
    // Generate a DIFFERENT number of files than the original (E2E_SEED_FILE_COUNT + 200). This changes
    // the listing → changes the snapshot fingerprint → the probe's staleness gate fires.
    let new_count = E2E_SEED_FILE_COUNT + 200;
    generate_seed_tree(Path::new(e2e_seed_dir), new_count, 1)?;
    eprintln!("[serve] E2E republish: rewrote seed tree to {new_count} files");

    let (identity, browse_key, _transport_key, _own_npub) = {
        let guard = live_npub.read().await;
        let id = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no identity loaded for E2E republish"))?;
        (
            id.identity.clone(),
            id.browse_key.clone(),
            id.transport_key.clone(),
            id.npub(),
        )
    };
    // Re-scan + re-save the collection draft (overwrites the prior draft — same slug).
    seed_collection(store, &identity, &browse_key, e2e_seed_dir, E2E_SLUG)?;
    // Re-publish the teaser with the NEW fingerprint.
    publish_e2e_teaser(store, &identity, &browse_key).await?;
    eprintln!("[serve] E2E republish complete: teaser carries a new fingerprint");
    Ok(())
}

/// Generate `count` small files in `dir`, each named `file-NNNN.bin`. If `offset` > 0, the files are
/// numbered from `offset` so a republish produces a distinct tree (different names → different
/// fingerprint). The files are tiny (a few bytes each) — only the ENTRY COUNT matters for exceeding
/// the truncation budget, not the file sizes.
fn generate_seed_tree(dir: &Path, count: usize, offset: usize) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| anyhow!("create seed dir {dir:?}: {e}"))?;
    for i in 0..count {
        let name = format!("file-{:04}.bin", offset + i);
        let path = dir.join(&name);
        // A few bytes each — the content does not matter, only the entry count (the listing JSON
        // includes name/type/tags/children per entry, which is what exceeds the 40 KB budget).
        std::fs::write(&path, b"x").map_err(|e| anyhow!("write seed file {path:?}: {e}"))?;
    }
    Ok(())
}

/// The auto-approve loop: poll the DM inbox for request-DMs, and for each one drive the production
/// approval body (`send_full_list_inner`, the fn the "Send the full list" click drives). Runs forever;
/// every approval is logged to stderr.
///
/// The loop is the serve side of WAN-E2E: it replaces the human "Send the full list" click with a
/// harness policy (auto-approve every request-DM from any asker). This is the ONLY deviation from the
/// production path, and it exists because a headless harness cannot click a button — the approval
/// itself IS the production code, called directly.
async fn run_auto_approve_loop(
    store: &DataStore,
    live_npub: &crate::identity_state::SharedIdentity,
    shared_relay: &net::SharedRelay,
    relays: &[String],
) -> Result<()> {
    use crate::commands::chat::decode_dms;
    use hb_net::RelayClient;
    use std::collections::HashSet;
    use std::time::Duration;

    let poll_interval = Duration::from_secs(5);
    let timeout = Duration::from_secs(15);
    let mut seen_request_ids: HashSet<String> = HashSet::new();

    // The identity fields the inbox poll needs (snapshot once under the lock; AppIdentity is not
    // Clone). Only `identity` + `own_npub` now: the approval itself reads the session secrets from the
    // live `SharedIdentity` inside `send_full_list_inner`, so the loop no longer holds a second copy
    // of them (same reasoning as `setup_manifest_plane`'s snapshot note).
    let (identity, own_npub) = {
        let guard = live_npub.read().await;
        let id = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no identity loaded for the auto-approve loop"))?;
        (id.identity.clone(), id.npub())
    };

    eprintln!("[serve] auto-approve loop started (polling DM inbox every {poll_interval:?})");
    loop {
        // Fetch gift-wrap events addressed to us (kind 1059, pubkey = ours).
        let client = match RelayClient::connect(&identity, relays, timeout).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[serve] auto-approve connect failed: {e}");
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        };
        let wraps = match client
            .fetch(
                Filter::new().kind(Kind::GiftWrap).pubkey(identity.public_key()),
                timeout,
            )
            .await
        {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[serve] auto-approve fetch failed: {e}");
                client.disconnect().await;
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        };
        client.disconnect().await;

        if wraps.is_empty() {
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // Decode all gift-wraps (no contact filter — the harness auto-approves anyone). decode_dms
        // recovers the real sender from the NIP-17 seal.
        let msgs = decode_dms(&own_npub, &identity, wraps, None).await;
        for msg in msgs {
            // Parse with PRODUCTION's parser (QURATOR-183) — never a harness copy. A hand-rolled
            // field read here silently lost the blank-string-to-`None` normalisation that decides
            // whether an ask is Carrier-4, and that is the discriminator the routing below turns on.
            let Some(body) = crate::auto_approve::ManifestRequestBody::parse(&msg.content) else {
                continue; // an ordinary chat DM — not ours
            };
            // Route by PRODUCTION's discriminator, and REFUSE what this loop cannot serve. This
            // loop's only body is `approve_request` → `send_full_list_inner`, which builds THIS
            // node's collection by slug. Handing it an author-bearing (Carrier-4 re-serve) ask
            // would serve the WRONG collection whenever the slugs collide — "films" and "music"
            // collide constantly — which is precisely the mis-route `should_auto_approve`'s
            // deleted step (0) used to prevent. `--suite carry` owns the re-serve path and drives
            // `send_cached_manifest_inner` itself; this loop must say so out loud rather than
            // quietly answer with the wrong bytes.
            if let crate::auto_approve::ApprovalBody::CachedManifest { author } =
                crate::auto_approve::approval_body_for(&body)
            {
                eprintln!(
                    "[serve] REFUSED a Carrier-4 re-serve ask from {} for '{}' by author {author}: \
                     this loop only serves its OWN collections (send_full_list_inner). Use \
                     --suite carry, which drives send_cached_manifest_inner.",
                    msg.from, body.slug
                );
                continue;
            }
            let slug = body.slug.as_str();
            let ask_nonce = body.ask_nonce.as_deref();
            // Dedup by (sender, slug, nonce) so a re-delivered request-DM doesn't trigger a second
            // approval (the production inbox would show it once).
            let dedup_key = format!("{}|{slug}|{}", msg.from, ask_nonce.unwrap_or(""));
            if !seen_request_ids.insert(dedup_key) {
                continue;
            }
            eprintln!(
                "[serve] auto-approve: request-DM from {} for slug '{slug}' (nonce={:?})",
                msg.from,
                ask_nonce
            );

            // Drive the production approval body (send_full_list_inner). If it fails, log and
            // continue — the loop stays alive for the next request.
            if let Err(e) =
                approve_request(store, live_npub, shared_relay, &msg.from, slug, ask_nonce).await
            {
                eprintln!("[serve] auto-approve failed for {slug}: {e:#}");
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Drive the production approval body for one request-DM: `send_full_list_inner` IS the body of the
/// `send_full_list` Tauri command (the command is a marshalling shim over it), so approving here does
/// exactly what the owner's click does — seal the manifest against the 16 MiB ceiling, bind the
/// listening endpoint, mint the ticket, persist the QURATOR-137 standing grant before the DM, and
/// DM the ticket to the asker. (Until QURATOR-177 Option E it also persisted the issued-ticket
/// record; that ledger is deleted by owner ruling.)
async fn approve_request(
    store: &DataStore,
    live_npub: &crate::identity_state::SharedIdentity,
    shared_relay: &net::SharedRelay,
    asker_npub: &str,
    slug: &str,
    ask_nonce: Option<&str>,
) -> Result<()> {
    // (1) Save the asker as a contact — the realistic world the approval runs in (a standing
    // gate used to require Good here; withdrawn by owner ruling 2026-09-03, QURATOR-177, so the
    // save is now realism, not a requirement). The auto-approve policy trusts any asker — a real
    // human reviews.
    // This is the harness's ONLY deviation from the production approval, and it must run BEFORE the
    // approval call: in production the human's contact already exists, so `send_full_list_inner` never
    // creates one.
    save_asker_contact(store, asker_npub)?;

    // (2) Record a manifest ask with the nonce from the request-DM — same nonce the ticket will echo.
    // Kept harness-side (placeholder fingerprint, as in `setup_manifest_plane`) because production
    // writes this record on the ASKER's side, when the request-DM is sent; `send_full_list_inner` has
    // no ask-record step to delegate to. The probe already recorded its own ask with the fingerprint
    // it browsed, and that is the record the claim gate reads.
    let nonce = ask_nonce.unwrap_or("");
    store.record_manifest_ask(asker_npub, asker_npub, slug, "wan-e2e", &rfc3339_now(), nonce)?;

    // (3) Approve through the production command body, exactly as `setup_manifest_plane` does. The
    // harness used to re-implement the steps here one by one, and that copy is how the QURATOR-137
    // grant write stayed invisible: WAN-E2E ran 2/2 green while writing no grant at all, so a green
    // run could not discharge §5 for any grant-related change. No `std::mem::forget` either — the
    // accept loop `ensure_endpoint` spawns holds its own endpoint clone, so the handle can drop.
    //
    // P-10 mutation (resolve by containing function, not text search): delete the
    // `record_standing_grant` call inside `send_full_list_inner` (fulfil.rs) — the serve's grant map
    // ends up empty, and the auto-approve loop's `standing_grant_for` read returns None, so a second
    // request for the same (peer, slug) is left for the human instead of auto-approved. (Until
    // QURATOR-177 this named deleting `record_issued_ticket` and E1's owner-side check reddening;
    // the ledger, that check, and the spent-bit refusal it caused are all deleted — Option E, owner
    // ruling 2026-09-03. What remains load-bearing on this path is the GRANT, the authorization.)
    send_full_list_inner(
        asker_npub.to_string(),
        slug.to_string(),
        ask_nonce.map(|n| n.to_string()),
        live_npub,
        store,
        shared_relay,
        &new_shared_endpoint(),
    )
    .await
    .map_err(|e| anyhow!("send_full_list_inner: {e}"))?;
    eprintln!(
        "[serve] auto-approve: approved via send_full_list_inner (slug '{slug}', nonce '{nonce}') — \
         ticket minted, grant written, and DM'd to {asker_npub}"
    );
    Ok(())
}

fn load_or_create_identity(store: &DataStore) -> Result<AppIdentity> {
    if let Some(stored) = store.load_identity()? {
        return AppIdentity::from_stored(&stored).map_err(|e| anyhow!("load identity: {e}"));
    }
    let id = AppIdentity::generate();
    let stored = id.to_stored().map_err(|e| anyhow!("serialize identity: {e}"))?;
    store.save_identity(&stored)?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// WAN-M serve setup — seed a collection, bind the endpoint, mint a ticket
//
// All production paths: scan_selective + save_collection_draft (the add-collection scan), the
// contact save (realism: production's asker is a known contact), record_manifest_ask (so the
// probe's claim_manifest_ask passes), ensure_endpoint with Role::Listen (the real accept loop +
// in-flight set), and the production approval body send_full_list_inner (the real mint path; the
// record_issued_ticket step it used to perform is deleted with the ledger, QURATOR-177 Option E).
// ---------------------------------------------------------------------------

/// Seed a collection from `seed_dir`, add `asker_npub` as a contact in good standing, bind the
/// manifest endpoint via the production path, and mint a ticket the probe redeems. Prints
/// `ticket=<json>` for the operator to hand to the probe. Idempotent across serve restarts: the
/// collection slug is fixed ("wan-it"), so a re-seed overwrites the same draft.
async fn setup_manifest_plane(
    store: &DataStore,
    live_npub: &crate::identity_state::SharedIdentity,
    seed_dir: &str,
    asker_npub: &str,
    shared_relay: &net::SharedRelay,
) -> Result<()> {
    // Snapshot the identity fields the plane + the manifest build need. Read once under the lock;
    // these are owned copies (Identity is Clone; the keys are regenerable secrets held for the
    // session). The source carries its own snapshot because ManifestSource is sync and cannot await
    // a live handle — same reason transport_state keys the binding to the npub.
    // Only what SEEDING needs. The transport key and own npub used to be snapshotted here too,
    // because this function bound the endpoint and minted the ticket by hand; `send_full_list_inner`
    // reads them from the same SharedIdentity itself, so the harness no longer holds a second copy
    // of the session's secrets.
    let (identity, browse_key) = {
        let guard = live_npub.read().await;
        let id = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no identity loaded for the manifest plane"))?;
        (id.identity.clone(), id.browse_key.clone())
    };

    // (1) Seed the collection — the production scan path (scan_selective + save_collection_draft).
    // A small collection suffices for M1 (the truncating case belongs to E2E). The slug is fixed so
    // re-seeding a running serve overwrites the same draft.
    let slug = "wan-it";
    seed_collection(store, &identity, &browse_key, seed_dir, slug)?;
    eprintln!("[serve] seeded collection '{slug}' from {seed_dir}");

    // (2) Add the asker as a contact — realism, not a gate (no standing is read on the serve path
    // since owner ruling 2026-09-03, QURATOR-177). browse_key_hex is None on the asker side; the
    // serve never decrypts the asker's listings (it is the owner here).
    save_asker_contact(store, asker_npub)?;
    eprintln!("[serve] asker {asker_npub} saved as a contact");

    // (3) Record a manifest ask the probe's claim gate expects — same nonce the ticket will echo.
    // The harness owns both sides, so the nonce is harness-chosen and known to both.
    let nonce = "wan-it-nonce";
    store.record_manifest_ask(asker_npub, asker_npub, slug, "wan-it", &rfc3339_now(), nonce)?;

    // (4) Approve, exactly as the owner's click does: `send_full_list_inner` IS the body of the
    // `send_full_list` Tauri command (the command is a marshalling shim over it). One call now does
    // what steps (4) and (5) used to do by hand — bind the listening endpoint via `ensure_endpoint`,
    // seal the manifest and prove it fits the transport ceiling, mint the ticket, persist the
    // standing grant BEFORE the DM, and DM the ticket to the asker. (Until QURATOR-177 Option E it
    // also persisted the issued-ticket record before the DM; the ledger is deleted by ruling.)
    //
    // **This replaced a hand-copy, and that is the whole point.** The harness used to re-implement
    // the approval sequence here and print `ticket=<json>` for an operator to paste into the probe's
    // `--ticket-json`. That copy is how the 2026-08-27 defect stayed invisible: a step that exists
    // only in production is a step no harness runs. The DM handoff is not a convenience — it is the
    // production channel, and using it removes both the manual step and the divergence.
    send_full_list_inner(
        asker_npub.to_string(),
        slug.to_string(),
        Some(nonce.to_string()),
        live_npub,
        store,
        shared_relay,
        &new_shared_endpoint(),
    )
    .await
    .map_err(|e| anyhow!("send_full_list_inner: {e}"))?;
    eprintln!(
        "[serve] approved via send_full_list_inner: manifest endpoint bound, ticket minted, grant \
         recorded, and DM'd to {asker_npub} (slug '{slug}', nonce '{nonce}')"
    );
    eprintln!("[serve] the probe picks the ticket up from its own inbox — no --ticket-json needed");
    Ok(())
}

/// Scan `seed_dir` into a `Collection` draft and persist it — the production add-collection path
/// (minus the Tauri State + the spawn_blocking deadline, which a headless harness does not need).
fn seed_collection(
    store: &DataStore,
    identity: &hb_core::Identity,
    browse_key: &crate::identity_state::SessionBrowseKey,
    seed_dir: &str,
    slug: &str,
) -> Result<()> {
    let root = Path::new(seed_dir);
    anyhow::ensure!(root.is_dir(), "--seed-dir {seed_dir} is not a directory");
    anyhow::ensure!(is_valid_slug(slug), "derived slug '{slug}' is invalid");

    // BUG FIX (found while live-testing HB-106 against a real nested tree, C:\Games): an EMPTY
    // IncludeSet does NOT mean "the full tree" — `IncludeSet::is_included`/`has_descendant_under`
    // both return false for an empty `checked` list, so `scan_selective` skips every subdirectory
    // and returns only root-level loose files. The comment this replaces claimed the opposite and
    // was never true; nothing had exercised `--seed-dir` against a directory whose content lives in
    // subfolders until now. A headless seed wants the full tree, so every top-level directory is
    // explicitly checked — `scan_selective` then recurses each one in full (an included directory's
    // entire subtree is walked, per its own doc comment).
    // `DirEntry::file_type()` does NOT follow reparse points on Windows (a junction/symlinked
    // subdirectory reports as non-dir) — `metadata()` does, matching `scan_selective_walk`'s own
    // convention (`entry.metadata()?.is_dir()`) so a symlinked/junctioned subtree is included the
    // same as an ordinary one, not silently dropped.
    let top_level_dirs: Vec<String> = std::fs::read_dir(root)
        .map_err(|e| anyhow!("read --seed-dir {seed_dir}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().map(|m| m.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let include = crate::commands::collection::IncludeSet::new(top_level_dirs);
    let globs = globset::GlobSet::empty();
    let (listing, total_bytes) = scan_selective(root, &include, &globs)
        .map_err(|e| anyhow!("scan {seed_dir}: {e}"))?;
    let item_count = count_items(&listing);
    // est_size is display-only metadata; format_size is a private helper, so compute a plain bytes
    // string here rather than widening the helper's visibility for a cosmetic field.
    let est_size = if total_bytes > 0 { Some(format!("{total_bytes} bytes")) } else { None };

    let collection = hb_core::Collection {
        slug: slug.to_string(),
        path_alias: "WAN-IT Seed".to_string(),
        description: None,
        item_count,
        est_size,
        content_types: vec!["other".to_string()],
        tags: vec![],
        languages: vec![],
        visibility: hb_core::Visibility::Public,
        sorted: false,
        last_updated: chrono::Utc::now(),
        listing,
    };
    store.save_collection_draft(&collection)?;

    // Prove the manifest is producible and within the ceiling — the same check send_full_list runs
    // before promising anything (build_slug_manifest + ManifestPayload::seal). A failure here means
    // the seed is too large or empty, and the harness should stop before minting a ticket.
    let envelope = build_slug_manifest(slug, store, identity, browse_key.bytes())
        .map_err(|e| anyhow!("build manifest for '{slug}': {e}"))?;
    hb_core::ManifestPayload::seal(&envelope)
        .map_err(|e| anyhow!("the seeded collection's manifest is over the transport ceiling: {e}"))?;
    Ok(())
}

/// Save `asker_npub` as a contact with no browse-key (the serve never browses the asker). Realism
/// only: no standing is read on the serve path (owner ruling 2026-09-03, QURATOR-177 — blocking
/// gates chat/DM interaction only).
fn save_asker_contact(store: &DataStore, asker_npub: &str) -> Result<()> {
    let contact = crate::store::CachedPeer {
        npub: asker_npub.to_string(),
        source: crate::store::ContactSource::Manual,
        browse_key_hex: None,
        petname: Some("wan-it-asker".to_string()),
        profile: None,
        collections: vec![],
        listings_state: Default::default(), // QURATOR-134 tri-state (not classified on this stub path)
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None,
        local_tags: vec![],
        fingerprint: None,
    };
    store
        .save_contact(&crate::store::CachedPeer::pubkey_hash(asker_npub), &contact)
        .map_err(|e| anyhow!("save asker contact: {e}"))?;
    Ok(())
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// probe — runs WAN-P
// ---------------------------------------------------------------------------

/// Parse the `--peer` argument (an npub OR a full `hbk…` share-code) into the served peer's
/// `PublicKey`. The harness only needs the npub for WAN-P (presence has no address; the browse-key
/// half of the share-code is unused here), so accept either form. `ShareCode::parse` handles both.
fn parse_peer(peer: &str) -> Result<nostr::PublicKey> {
    let share = hb_core::ShareCode::parse(peer).map_err(|e| anyhow!("invalid --peer code: {e}"))?;
    Ok(share.pubkey())
}

async fn run_probe(args: &[String]) -> Result<ExitCode> {
    // Suite selection: --suite wan-m runs the WAN-M rows (M1 + M9) against --ticket-json; --suite
    // wan-e2e runs the WAN-E2E rows (E1 + E2, the full DM-coordinated pipeline); --suite wan-u runs
    // the WAN-U rows (U1–U7, the user-surface live twins of the hb-it L2 browse/publish/private
    // suites); the default is WAN-P (the presence suite W6.1 shipped). §W6 authorizes --ticket-json
    // for targeted iroh isolation runs (the E2E suite rides the full DM leg).
    let suite = args::flag_value(args, "--suite").unwrap_or("wan-p");

    // WAN-M4 (n0 canary core) needs no relays — it uses presets::N0 directly. Dispatch before the
    // relay-set check so `probe --suite wan-m4` works without --relay.
    if suite == "wan-m4" {
        return run_probe_wan_m4(args).await;
    }

    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }

    // WAN-U / WAN-C / WAN-T / WAN-D / WAN-R (and the CARRY suite) are probe-plays-both: every row constructs its own throwaway
    // identities, so they need no --peer. All other suites (wan-p/m/e2e) drive a live serve and
    // require --peer.
    if suite == "wan-u" {
        return run_probe_wan_u(args).await;
    }
    if suite == "wan-c" {
        return run_probe_wan_c(args).await;
    }
    if suite == "wan-t" {
        return run_probe_wan_t(args).await;
    }
    if suite == "wan-d" {
        return run_probe_wan_d(args).await;
    }
    if suite == "wan-r" {
        return run_probe_wan_r(args).await;
    }
    // CARRY (QURATOR-178) is also probe-plays-a-role: each phase is driven by one process playing
    // exactly one party (A, C or D) with its own --data-dir, so it needs no --peer either.
    if suite == "carry" {
        return run_probe_wan_carry(args).await;
    }
    // FETCH (QURATOR-164 item 3) is probe-plays-a-role too: A and D each run one process with
    // their own --data-dir, so no --peer.
    if suite == "fetch" {
        return run_probe_wan_fetch(args).await;
    }

    let peer_str = args::flag_value(args, "--peer")
        .ok_or_else(|| anyhow!("probe requires --peer <npub or hbk… share-code>"))?;
    if suite == "wan-m" {
        return run_probe_wan_m(args, peer_str).await;
    }
    if suite == "wan-e2e" {
        return run_probe_wan_e2e(args, peer_str).await;
    }

    let peer_npub = parse_peer(peer_str)?;

    // P3 arming: --flood-relay (and optionally --flood-count). Absent ⇒ P3 is skipped-with-diagnostic.
    let flood_relays = args::collect_flood_relays(args);
    let flood_count = args::flag_value(args, "--flood-count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(600);
    let flood_ctx = if flood_relays.is_empty() {
        None
    } else {
        Some(suite_wan_p::FloodCtx {
            flood_relays,
            read_relays: relays.clone(),
            flood_count,
        })
    };

    println!("# WAN-P probe against peer {}", peer_npub.to_bech32().unwrap_or_default());
    println!("# relay set: {}", relays.join(", "));
    if let Some(ctx) = &flood_ctx {
        println!(
            "# P3 armed: {} flood relays, flood_count={}",
            ctx.flood_relays.len(),
            ctx.flood_count
        );
    }

    let mut tap = tap::Tap::new();
    suite_wan_p::run(&mut tap, &peer_npub, &relays, flood_ctx.as_ref()).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-M (M1 + M9)
// ---------------------------------------------------------------------------

/// How long the probe waits for the serve's ticket DM before giving up.
///
/// Generous on purpose: this is a real relay round trip across two machines (and, in a CI leg,
/// across two clouds). The serve has to scan and seal a collection before it DMs anything, and the
/// relay's own propagation is the rest. A timeout here is an honest failure — it means the DM never
/// arrived, which is exactly what this leg exists to detect.
const TICKET_DM_WAIT: Duration = Duration::from_secs(180);

/// Poll interval while waiting. Matches the production DM cadence rather than hammering the relay.
const TICKET_DM_POLL: Duration = Duration::from_secs(3);

/// Wait for the serve's transport-ticket DM and return its body, using the SAME primitives the Chat
/// page uses: the production gift-wrap inbox filter, and `decode_dms` (which Schnorr-verifies the
/// seal and recovers the real sender from inside it, never trusting the wrap's author).
///
/// This is the handoff that used to be a human copying `ticket=<json>` out of the serve's stdout.
/// Reading it off the relay instead is not merely more convenient — it means the DM leg is now
/// COVERED by WAN-M rather than stubbed around, which is the same class of gap that let the
/// 2026-08-27 sanitize defect ship.
async fn await_ticket_dm(
    app_id: &AppIdentity,
    store: &DataStore,
    relays: &[String],
) -> Result<String> {
    let shared_relay = net::new_shared();
    let client = net::client(&app_id.identity, store, &shared_relay).await?;
    let me = app_id.identity.public_key();
    let own_npub = app_id.npub();

    let deadline = tokio::time::Instant::now() + TICKET_DM_WAIT;
    let mut last_err = String::from("no ticket DM seen");
    let mut polls = 0u32;

    while tokio::time::Instant::now() < deadline {
        polls += 1;
        // `since = 0`: take the whole inbox. The probe's store is fresh per run, and a ticket that
        // arrived while we were still binding must not be missed by a narrow window.
        match client.fetch(crate::commands::chat::dm_inbox_filter(me, 0), net::RELAY_TIMEOUT).await {
            Ok(wraps) => {
                let n = wraps.len();
                // No contact filter: the serve is not yet in the probe's contact list on a fresh
                // run, and the ticket's authenticity comes from the seal (decode_dms verifies it),
                // not from an address book.
                let msgs =
                    crate::commands::chat::decode_dms(&own_npub, &app_id.identity, wraps, None).await;
                for m in &msgs {
                    if let Ok(t) = serde_json::from_str::<TransportTicket>(&m.content) {
                        if t.verify_shape().is_ok() {
                            eprintln!(
                                "[probe] ticket DM received after {polls} poll(s): request_id={} slug={}",
                                t.request_id, t.slug
                            );
                            return Ok(m.content.clone());
                        }
                    }
                }
                last_err = format!("{n} wrap(s) in the inbox, none carried a valid ticket");
            }
            Err(e) => last_err = format!("inbox fetch failed: {e}"),
        }
        tokio::time::sleep(TICKET_DM_POLL).await;
    }
    Err(anyhow!(
        "no transport ticket arrived by DM within {}s ({last_err}). The serve must be running with \
         --seed-dir + --asker-npub set to THIS probe's npub ({own_npub}), and both sides must share \
         at least one relay: {relays:?}",
        TICKET_DM_WAIT.as_secs()
    ))
}

/// Run the WAN-M rows against a live serve. Requires the serve's FULL share code (--peer hbk…) — the
/// acceptance gate (`accept_manifest_bytes`) needs the browse-key to decrypt.
///
/// **The ticket arrives by DM, the way it does in production.** `--ticket-json` is still accepted as
/// an override for offline/isolation runs, but it is no longer required: with it absent the probe
/// polls its own NIP-17 inbox for the ticket the serve's `send_full_list_inner` DM'd, exactly as the
/// Chat page does. That is what removed the operator copy-paste from the middle of WAN-M and what
/// makes an unattended cross-platform leg (Linux serve, Windows probe) possible.
async fn run_probe_wan_m(args: &[String], peer_str: &str) -> Result<ExitCode> {
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );

    let store = DataStore::new(data_dir.clone());
    // Persist the relay set (parity with serve — net::relay_urls reads Settings).
    let relays = args::collect_relays(args);
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    let ticket_json = match args::flag_value(args, "--ticket-json") {
        Some(explicit) => {
            eprintln!("[probe] --ticket-json supplied; skipping the inbox wait (isolation run)");
            explicit.to_string()
        }
        None => {
            eprintln!(
                "[probe] waiting for the ticket DM from the serve (up to {}s) — this is the \
                 production handoff, not a harness side-channel",
                TICKET_DM_WAIT.as_secs()
            );
            await_ticket_dm(&app_id, &store, &relays).await?
        }
    };

    // The dead-endpoint address for M9: an unroutable iroh EndpointAddr JSON. The transport parses
    // this via serde_json; an id + a TEST-NET-1 (RFC 5737) socket is guaranteed to fail the dial.
    let dead_addr = make_dead_endpoint_addr().await;

    let input =
        suite_wan_m::build_probe_input(app_id, store, peer_str, &ticket_json, &dead_addr).await?;

    println!("# WAN-M probe against serve {}", input.serve_npub);
    println!("# ticket request_id={} slug={} nonce={:?}",
        input.live_ticket.request_id,
        input.live_ticket.slug,
        input.live_ticket.ask_nonce,
    );
    println!("# relay set: {}", relays.join(", "));

    // When --serve-data-dir points at the serve's data dir (single-machine smoke), the probe can read
    // the serve's store to confirm the ticket was consumed — the owner-side half of M1-once.
    // (--serve-data-dir used to open the serve's store for M1-once's owner-side consumed_at
    // confirmation; deleted with the issued-ticket ledger, QURATOR-177 Option E.)

    let mut tap = tap::Tap::new();
    suite_wan_m::run(&mut tap, &input).await;
    Ok(tap.finish())
}

/// A dialable-looking iroh `EndpointAddr` JSON that points at an unroutable address (TEST-NET-1, RFC
/// 5737 — `192.0.2.0/24` is reserved for documentation and guaranteed not to route). The transport's
/// `parse_node_addr` accepts any serde_json `EndpointAddr`; the dial then fails bounded-in-time
/// because nothing answers at that socket. This is how §W6's "ticket whose node_addr points at a dead
/// endpoint" is exercised without standing up a second iroh node.
///
/// Built by serializing a REAL bound endpoint's `addr()` and rewriting the IP to TEST-NET-1, so the
/// JSON's serde shape (PublicKey as a z-base-32 string, TransportAddr as a tagged enum, addrs as a
/// BTreeSet) is exactly what iroh produces — a hand-crafted JSON could not be trusted to match iroh's
/// non-obvious serde representation, and a parse failure would make the dial fail at the wrong layer.
async fn make_dead_endpoint_addr() -> String {
    // Bind a throwaway loopback endpoint just to get a real, serializable addr. The probe never
    // dials THIS endpoint; the addr's IP is rewritten to unroutable. bind_client_endpoint (dial-only,
    // no ALPN) is the cheapest production bind — presets::N0 with no advertised protocol.
    let secret: [u8; 32] = rand::random();
    let ep = match crate::transport::bind_client_endpoint(&secret).await {
        Ok(ep) => ep,
        // If the bind fails (e.g. no network), fall back to a structurally-valid placeholder: the
        // dial still fails, just at the parse layer rather than the connect layer. The M9 row's
        // assertion is "bounded failure", and a parse error is bounded.
        Err(e) => {
            eprintln!("[probe] could not bind a throwaway endpoint for the dead-addr template: {e}");
            return r#"{"id":"caaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","addrs":[]}"#.to_string();
        }
    };
    let addr = ep.addr();
    let mut json = serde_json::to_string(&addr).unwrap_or_default();
    ep.close().await;
    // Rewrite any loopback IP (127.0.0.1 / ::1) in the serialized JSON to a TEST-NET-1 address.
    // The exact string "127.0.0.1" appears in the Ip(SocketAddr) serialization; replacing it makes
    // the address unroutable while preserving the rest of the structure (the id, the port).
    json = json.replace("127.0.0.1", "192.0.2.1");
    json = json.replace("::1", "192.0.2.1");
    json
}

// ---------------------------------------------------------------------------
// probe — WAN-E2E (E1 + E2 — the full DM-coordinated pipeline)
// ---------------------------------------------------------------------------

/// Run the WAN-E2E rows against a live serve. Requires the serve's FULL share code (--peer hbk…) —
/// `accept_manifest_bytes` needs the browse-key to decrypt, and `browse_share_code` needs the full code.
/// The probe seeds its own identity + store from --data-dir. The serve must be running with
/// `--auto-approve` (the auto-approve loop polls the DM inbox and answers request-DMs by driving the
/// production approval body).
async fn run_probe_wan_e2e(args: &[String], peer_str: &str) -> Result<ExitCode> {
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );

    let store = DataStore::new(data_dir.clone());
    // Persist the relay set (parity with serve — net::relay_urls reads Settings).
    let relays = args::collect_relays(args);
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    // The serve's share code is the full hbk… code (npub + browse-key).
    let input = suite_wan_e2e::build_probe_input(app_id, store, peer_str.to_string()).await?;

    println!("# WAN-E2E probe against serve {}", input.serve_npub);
    println!("# relay set: {}", relays.join(", "));

    // (--serve-data-dir used to feed the probe the serve's store for E1's owner-side receipt
    // confirmation; that read is deleted with the issued-ticket ledger, QURATOR-177 Option E.)

    let mut tap = tap::Tap::new();
    suite_wan_e2e::run(&mut tap, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-U (U1–U7 — the user-surface live twins)
// ---------------------------------------------------------------------------

/// Run the WAN-U rows against the live relay set. Probe-plays-both: every row constructs two
/// in-process identities (publisher + browser/resolver) against the live relays, the same shape
/// `hb-it` L2 bodies use. No `serve` is needed — the rows that need serve-side state changes (U4
/// republish, U5 unpublish, U6 re-key) run probe-driven with the probe's own throwaway identity as
/// the publisher (§W6 authorizes this shape; it is how the hb-it L2 bodies already work).
///
/// The probe seeds its own identity + store from --data-dir (U1's add-contact funnel persists the
/// resolved contact here, so the relay set is persisted to Settings for `net::relay_urls` to return).
async fn run_probe_wan_u(args: &[String]) -> Result<ExitCode> {
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );

    let store = DataStore::new(data_dir.clone());
    // Persist the relay set (parity with the other suites — net::relay_urls reads Settings, and U1's
    // resolve_peer reads the relay set from the store via net::relay_urls).
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    let input = suite_wan_u::build_probe_input(app_id, store, relays.clone()).await?;

    println!("# WAN-U probe (probe-plays-both against the live relay set)");
    println!("# relay set: {}", relays.join(", "));

    let mut tap = tap::Tap::new();
    suite_wan_u::run(&mut tap, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-C (C1–C5 — chat over real relays)
// ---------------------------------------------------------------------------

/// Run the WAN-C rows against the live relay set. Probe-plays-both: every row constructs two (or
/// three) in-process identities against the live relays, the same shape `hb-it` L2 bodies use. No
/// `serve` is needed. The probe uses a throwaway data dir for the C4/C5 rows that persist the DM
/// cache / blocklist; the relay set is persisted to Settings for parity with the other suites.
async fn run_probe_wan_c(args: &[String]) -> Result<ExitCode> {
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );

    let store = DataStore::new(data_dir.clone());
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }
    // Persist the relay set (parity with the other suites — net::relay_urls reads Settings).
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;

    let input = suite_wan_c::build_probe_input(store, relays.clone()).await?;

    println!("# WAN-C probe (probe-plays-both against the live relay set)");
    println!("# relay set: {}", relays.join(", "));

    let mut tap = tap::Tap::new();
    suite_wan_c::run(&mut tap, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-T (T1–T5 — topics between real clients)
// ---------------------------------------------------------------------------

/// Run the WAN-T rows against the live relay set. Probe-plays-both: every row constructs two (or
/// three) in-process identities against the live relays, the same shape `hb-it` L2 bodies use. No
/// `serve` is needed.
async fn run_probe_wan_t(args: &[String]) -> Result<ExitCode> {
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );

    let store = DataStore::new(data_dir.clone());
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    // The T3 redeem path persists topic nonces; an identity is needed for the store_topic path the
    // production redeem drives. Load-or-create one (the rows build their own throwaway identities for
    // the relay ops; this identity owns the data dir for nonce persistence parity).
    let app_id = load_or_create_identity(&store)?;

    let input = suite_wan_t::build_probe_input(app_id, store, relays.clone()).await?;

    println!("# WAN-T probe (probe-plays-both against the live relay set)");
    println!("# relay set: {}", relays.join(", "));

    let mut tap = tap::Tap::new();
    suite_wan_t::run(&mut tap, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-D (D1–D4 — discovery against real relays)
// ---------------------------------------------------------------------------

/// Run the WAN-D rows against the live relay set. Probe-plays-both: every row constructs its own
/// throwaway identities against the live relays. D3 is opt-in via --flood-relay (relay citizenship:
/// flood-shaped rows never run against public relays).
async fn run_probe_wan_d(args: &[String]) -> Result<ExitCode> {
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }

    // D3 arming: --flood-relay (and optionally --flood-count). Absent ⇒ D3 is skipped-with-diagnostic.
    let flood_relays = args::collect_flood_relays(args);
    let flood_count = args::flag_value(args, "--flood-count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(60);
    let flood_ctx = if flood_relays.is_empty() {
        None
    } else {
        Some(suite_wan_d::FloodCtx {
            flood_relays,
            read_relays: relays.clone(),
            flood_count,
        })
    };

    let input = suite_wan_d::build_probe_input(relays.clone(), flood_ctx).await?;

    println!("# WAN-D probe (probe-plays-both against the live relay set)");
    println!("# relay set: {}", relays.join(", "));
    if input.flood_ctx.is_some() {
        println!("# D3 armed (flood-count={flood_count})");
    }

    let mut tap = tap::Tap::new();
    suite_wan_d::run(&mut tap, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-R (R1–R2 — relay-set behavior + the default-relay policy watch)
// ---------------------------------------------------------------------------

/// Run the WAN-R rows. R1 uses the live VPS relay set; R2 (the canary row) touches the public
/// defaults — that is its job (one event per kind per relay per run). Probe-plays-both.
async fn run_probe_wan_r(args: &[String]) -> Result<ExitCode> {
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay (the VPS strfry set for R1)");
    }

    let input = suite_wan_r::build_probe_input(relays.clone()).await?;

    println!("# WAN-R probe (R1: degraded set against the live relay set; R2: public-default policy watch)");
    println!("# relay set (R1): {}", relays.join(", "));

    let mut tap = tap::Tap::new();
    suite_wan_r::run(&mut tap, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — CARRY (QURATOR-178 — the 4-party Carrier-4 re-serve)
// ---------------------------------------------------------------------------

/// Run the CARRY suite's rows for ONE party, chosen by `--role a|c|d`. Each phase is a separate
/// `hb-wan-it probe --suite carry --role <x>` invocation with its own `--data-dir`:
///
/// * role A (the author):  `--role a --seed-dir <dir> --asker-npub <C npub>` — seeds + publishes the
///   collection, answers C's ask, then the OPERATOR kills the process (that kill is the "A offline"
///   leg; it is topology, not a row).
/// * role C (the cacher):  `--role c --phase 1 --author-npub <A npub> --author-share-code <hbk…>`
///   (fetch + cache from A), then `--role c --phase 2 --asker-npub <D npub>` (answer D's author-ask
///   by re-serving the cached copy).
/// * role D (the asker):   `--role d --carrier-npub <C npub> --carrier-share-code <hbk…>
///   --author-npub <A npub>` — asks C for A's collection and redeems C's cached copy.
///
/// All relays come from `--relay` as usual (the SG strfry for the documented topology).
async fn run_probe_wan_carry(args: &[String]) -> Result<ExitCode> {
    let role = args::flag_value(args, "--role")
        .ok_or_else(|| anyhow!("probe --suite carry requires --role a|c|d"))?
        .to_string();

    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );
    let store = DataStore::new(data_dir.clone());
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    println!("# CARRY probe — role {role}");
    println!("# relay set: {}", relays.join(", "));

    let input = suite_wan_carry::CarryInput {
        app_id,
        store,
        relays,
        args: args.to_vec(),
    };

    let mut tap = tap::Tap::new();
    suite_wan_carry::run(&mut tap, &role, &input).await;
    Ok(tap.finish())
}

/// Run the FETCH rows (QURATOR-164 item 3) — the background fetch driver over real relays.
///
/// Two parties, sequenced by the operator:
/// * role A (the author):  `--role a --phase 1 --seed-dir <dir> --asker-npub <D npub>`, then
///   `--role a --phase 2 --seed-dir <dir> --asker-npub <D npub>` (republishes a CHANGED tree, so
///   the snapshot fingerprint moves, then answers the ask the driver itself sends).
/// * role D (the driver node): `--role d --phase 1 --author-npub <A npub>
///   --author-share-code <hbk…>` (cache at the OLD fingerprint), then
///   `--role d --phase 2 --author-npub <A npub>` (run `poll_once` and assert it asked AND
///   redeemed with no operator step in between).
async fn run_probe_wan_fetch(args: &[String]) -> Result<ExitCode> {
    let role = args::flag_value(args, "--role")
        .ok_or_else(|| anyhow!("probe --suite fetch requires --role a|d"))?
        .to_string();

    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );
    let store = DataStore::new(data_dir.clone());
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    println!("# FETCH probe — role {role}");
    println!("# relay set: {}", relays.join(", "));

    let input = suite_wan_carry::CarryInput { app_id, store, relays, args: args.to_vec() };

    let mut tap = tap::Tap::new();
    suite_wan_fetch::run(&mut tap, &role, &input).await;
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// probe — WAN-M4 (n0 canary core — standalone)
// ---------------------------------------------------------------------------

/// Run the M4 row standalone (n0 canary core). Binds a listening endpoint via the production path
/// (presets::N0), waits for a home relay, then dials it through the relay path. Probe-plays-both.
async fn run_probe_wan_m4(_args: &[String]) -> Result<ExitCode> {
    println!("# WAN-M4 probe (n0 canary core — bind_endpoint + home-relay reachability)");

    let mut tap = tap::Tap::new();
    tap.check(
        "M4: bind_endpoint obtains a home relay (presets::N0) and the endpoint is reachable through it",
        suite_wan_m::m4_n0_canary().await,
    );
    Ok(tap.finish())
}

// ---------------------------------------------------------------------------
// canary — the thin continuous slice (M20 W6 §W6 cadence)
//
// One pass = M4 (n0 reachable) + R2 (default-relay policy) + P2 (beacon acceptance) + C1 (DM
// round-trip). Loop every --interval (default ~600 s), each pass printing a timestamped one-line
// summary; any row failure prints [ALERT] <row>: <reason> to stderr. --once: a single pass (the smoke
// uses this); exit nonzero if any row failed. Loop mode runs until killed.
// ---------------------------------------------------------------------------

/// The default canary interval (the §W6 recommended cadence, ~10 min).
const CANARY_DEFAULT_INTERVAL: Duration = Duration::from_secs(600);

async fn run_canary(args: &[String]) -> Result<ExitCode> {
    let once = args::flag_value(args, "--once").is_some();
    let interval = args::flag_value(args, "--interval")
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(CANARY_DEFAULT_INTERVAL);

    if once {
        let failed = canary_pass().await;
        // Force-exit: the iroh endpoints spawned by M4 leave background tasks (relay keepalives)
        // that keep the tokio runtime alive after the summary prints. --once is the smoke form; a
        // prompt process exit is the contract. Loop mode runs until killed (SIGTERM handles it).
        std::process::exit(if failed { 1 } else { 0 });
    }

    // Loop mode: run forever, sleep `interval` between passes. Each pass prints a timestamped
    // one-line summary; failures print [ALERT] to stderr. Runs until killed.
    loop {
        canary_pass().await;
        tokio::time::sleep(interval).await;
    }
}

/// Run one canary pass. Returns true if any row failed (for --once exit code). Each row runs with its
/// own short timeout; a failure prints `[ALERT] <row>: <reason>` to stderr and the pass summary prints
/// the row-by-row outcome. The summary is timestamped (the §W6 cadence mandate).
async fn canary_pass() -> bool {
    let now = unix_now();
    let ts = rfc3339_now();
    let mut failures: Vec<(&'static str, String)> = Vec::new();

    // M4 — n0 canary core. The first row because it is the one that catches an n0 fleet outage
    // (production's silent dependency). A long-ish timeout is built into the row (HOME_RELAY_WAIT +
    // M4_DIAL_TIMEOUT = 60 + 90 = 150s); the outer 200s has headroom.
    match tokio::time::timeout(Duration::from_secs(200), suite_wan_m::m4_n0_canary()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => failures.push(("M4", e)),
        Err(_) => failures.push(("M4", "timed out at 150s".to_string())),
    }

    // R2 — default-relay policy watch. The evidence table is printed to stderr by the row; the row
    // fails only on connect failures (a relay down for everyone).
    match tokio::time::timeout(Duration::from_secs(120), suite_wan_r::r2_default_relay_policy_watch()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => failures.push(("R2", e)),
        Err(_) => failures.push(("R2", "timed out at 120s".to_string())),
    }

    // P2 — beacon acceptance. Reuse the WAN-P P2 body (publish a beacon, record per-relay
    // accept/reject). Needs a relay set; the canary uses the VPS strfry backbone.
    let beacon_relays = canary_beacon_relays();
    // An empty backbone is a CONFIGURATION failure, not a pass. Probing an empty relay set would
    // let the canary print "ok" while watching nothing at all.
    const NO_BACKBONE: &str = "HB_CANARY_RELAYS is unset or empty - no backbone to probe (set it in .env)";
    if beacon_relays.is_empty() {
        failures.push(("P2", NO_BACKBONE.to_string()));
    } else {
        match tokio::time::timeout(
            Duration::from_secs(45),
            suite_wan_p::canary_beacon_acceptance(&beacon_relays),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => failures.push(("P2", e)),
            Err(_) => failures.push(("P2", "timed out at 45s".to_string())),
        }
    }

    // C1 — DM round-trip. Reuse the WAN-C C1 body (send + fetch+unwrap). Needs the relay set.
    if beacon_relays.is_empty() {
        failures.push(("C1", NO_BACKBONE.to_string()));
    } else {
        match tokio::time::timeout(
            Duration::from_secs(60),
            suite_wan_c::canary_dm_round_trip(&beacon_relays),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => failures.push(("C1", e)),
            Err(_) => failures.push(("C1", "timed out at 60s".to_string())),
        }
    }

    // The [ALERT] lines (stderr) — one per failure.
    for (row, reason) in &failures {
        eprintln!("[ALERT] {row}: {reason}");
    }

    // The timestamped one-line summary (stdout). "ok" when all four rows passed; otherwise the failing
    // rows named.
    let status = if failures.is_empty() {
        "ok".to_string()
    } else {
        format!("FAIL: {}", failures.iter().map(|(r, _)| r.to_string()).collect::<Vec<_>>().join(","))
    };
    let failed = !failures.is_empty();
    println!("[canary {ts}] {status} (M4+R2+P2+C1) unix={now}");
    failed
}

/// The VPS strfry backbone the canary uses for P2 + C1 (the M5 launch relays, the fallback for the
/// M20 W1 discriminator). The canary is a daemon, not a probe that takes `--relay`, so there is no
/// CLI path here — the addresses come from **`HB_CANARY_RELAYS`** (comma-separated `ws://host:port`),
/// which lives in the **gitignored `.env`**: they are infrastructure and must not sit in a public repo.
///
/// Returns empty when the variable is unset or blank. The caller MUST fail the affected rows rather
/// than probe an empty set — a canary that probes nothing would print "ok" and be indistinguishable
/// from a healthy backbone, which is the exact confident-negative failure this repo keeps shipping.
fn canary_beacon_relays() -> Vec<String> {
    std::env::var("HB_CANARY_RELAYS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// Unit tests for the probe's peer parsing (pure, no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_peer_accepts_a_bare_npub() {
        let id = AppIdentity::generate();
        let npub = id.npub();
        let parsed = parse_peer(&npub).unwrap();
        assert_eq!(parsed, id.public_key());
    }

    #[test]
    fn parse_peer_accepts_a_full_hbk_share_code() {
        let id = AppIdentity::generate();
        let code = id.share_code().unwrap();
        let parsed = parse_peer(&code).unwrap();
        assert_eq!(parsed, id.public_key());
    }

    #[test]
    fn parse_peer_rejects_garbage() {
        assert!(parse_peer("not-a-code").is_err());
    }

    /// The other half of `fulfil_commands_stay_thin_so_the_harness_keeps_covering_them`.
    ///
    /// That guard pins the `#[tauri::command]` bodies as thin shims over `*_inner`, and its whole
    /// rationale is the sentence "the WAN harness calls the inner fn, so anything here is untested".
    /// Nothing verified that sentence. It was false: `approve_request` re-implemented the approval
    /// step by step, so WAN-E2E ran green while never executing `send_full_list_inner` — and
    /// therefore never writing the QURATOR-137 standing grant (`fulfil.rs`, `record_standing_grant`).
    /// A green WAN-E2E could not discharge §5 for any grant-related change, and nothing said so.
    ///
    /// This is the same failure shape as the `sanitize_node_addr` defect (2026-08-27): a harness that
    /// copies a body instead of calling it covers the copy, not the code that ships. A drift guard,
    /// not integration coverage — the `suite_cap.rs` precedent — but it fails loudly on re-divergence,
    /// which is the part no one was getting.
    ///
    /// MUTATION (P-10, resolve by containing function): in `approve_request`, replace the
    /// `send_full_list_inner(` call with any hand-rolled step — e.g. re-add `issue_ticket(` — and
    /// this test reds. Comments are stripped first, so restating the rule in prose cannot satisfy it.
    /// The serve loop must parse with PRODUCTION's parser and route by PRODUCTION's
    /// discriminator — QURATOR-183, the same fix `suite_wan_carry.rs` took. Two things are pinned,
    /// and the second is a real trap rather than tidiness:
    ///
    /// 1. No hand-rolled request-DM tag check. A copy loses the blank-string-to-`None`
    ///    normalisation that decides whether an ask is Carrier-4.
    /// 2. An author-bearing ask is REFUSED, not served. This loop's only body is
    ///    `send_full_list_inner`, which builds THIS node's collection by slug — serving a
    ///    Carrier-4 re-serve ask through it returns the wrong collection on a slug collision,
    ///    which is exactly what `should_auto_approve`'s deleted step (0) used to prevent.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `run_auto_approve_loop`, delete the
    /// `ApprovalBody::CachedManifest` refusal arm → the routing assert reds; or restore a
    /// `serde_json::from_str` + `v.get("hb")` parse → the tag-literal assert reds.
    #[test]
    fn the_serve_loop_parses_via_production_and_refuses_carrier4_asks() {
        let src = include_str!("mod.rs");
        // Comments stripped: documenting the rule must not satisfy it. The test half below quotes
        // the literals this scans for, so slice the production half only.
        let production = &src[..src.find("#[cfg(test)]").expect("test module must exist")];
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !code.contains("\"manifest_request\""),
            "the serve loop must not re-implement the request-DM tag check — that literal belongs \
             to ManifestRequestBody::parse, and a copy covers the copy, not what ships"
        );
        assert!(
            code.contains("ManifestRequestBody::parse"),
            "the serve loop must parse request-DMs with production's parser"
        );
        assert!(
            code.contains("ApprovalBody::CachedManifest"),
            "the serve loop must consult production's routing discriminator and REFUSE an \
             author-bearing ask — its only body is send_full_list_inner, which would serve THIS \
             node's same-named collection instead of the author's"
        );
    }

    #[test]
    fn the_harness_approves_through_send_full_list_inner_and_never_rebuilds_it() {
        let src = include_str!("mod.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let sig = "async fn approve_request(";
        let at = code.find(sig).expect("approve_request not found");
        let end = code[at..].find("\n}").expect("approve_request must terminate") + at;
        let body = &code[at..end];

        assert!(
            body.contains("send_full_list_inner("),
            "approve_request must approve by calling send_full_list_inner — the body the owner's \
             'Send the full list' click runs. Re-implementing the steps here is how WAN-E2E went \
             green while writing no standing grant at all."
        );

        // The steps send_full_list_inner owns. Any one of them reappearing here means the harness
        // has started keeping its own copy again — the exact drift that hid the grant write.
        // (`record_issued_ticket(` was in this list until QURATOR-177 Option E deleted the call
        // from production; a step production no longer performs is not one the harness can
        // re-implement.)
        for step in [
            "ManifestPayload::seal(",
            "ensure_endpoint(",
            "issue_ticket(",
            "send_dm_inner(",
        ] {
            assert!(
                !body.contains(step),
                "approve_request re-implements {step:?}, which send_full_list_inner already does. \
                 The harness would then be testing its own copy instead of the shipping path — \
                 delete it and let the delegated call do the work."
            );
        }
    }
}
