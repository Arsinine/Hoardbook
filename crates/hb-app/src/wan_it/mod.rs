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
mod suite_wan_d;
mod suite_wan_e2e;
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
use crate::manifest_source::StoreManifestSource;
use crate::net;
use crate::presence;
use crate::store::{DataStore, IssuedTicketRecord, Settings};
use crate::transport::{issue_ticket, ManifestSource};
use crate::transport_state::{ensure_endpoint, new_shared_endpoint, Role};

/// Entry point — invoked by the thin `bin/hb-wan-it.rs` wrapper. Matches the retired harness's
/// `run()` shape: dispatch on the first positional, return an ExitCode.
pub(crate) async fn run() -> ExitCode {
    // The lock resolves rustls with both providers enabled (iroh `tls-ring` + reqwest aws-lc-rs),
    // so there is no automatic process default; production installs one as a side effect of binding
    // the iroh endpoint at startup. This harness never binds iroh, so without this line every
    // `wss://` relay connect panics. Err = a provider is already installed, which is fine.
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
            With --seed-dir + --asker-npub: also seeds a collection from <dir>, binds the manifest\n\
            endpoint via the production path (ensure_endpoint, presets::N0), and mints a ticket the\n\
            probe redeems — the launch-gate WAN-M path. Prints ticket=<json> for the operator to\n\
            hand to the probe (--ticket-json).\n\
            With --auto-approve + --e2e-seed-dir: seeds a TRUNCATING collection (large enough to\n\
            exceed the 40 KB teaser budget), publishes the teaser, and runs the auto-approve loop:\n\
            polls the DM inbox for request-DMs and answers each by driving the production approval\n\
            body. This is the serve side of WAN-E2E. --republish rewrites the seed tree mid-run (E2).\n\
     \n\
     probe  --peer <npub|hbk…> --relay <ws-url>...\n\
            [--flood-relay <ws-url>... --flood-count <n>]\n\
            [--ticket-json <path|inline> --suite wan-m]\n\
            [--suite wan-e2e]\n\
            [--suite wan-u]\n\
            [--suite wan-c]\n\
            [--suite wan-t]\n\
            [--suite wan-d [--flood-relay <ws-url>... --flood-count <n>]]\n\
            [--suite wan-r]\n\
            [--suite wan-m4]\n\
            Runs WAN-P rows P1–P5 against a live serve by default. --flood-relay arms P3\n\
            (VPS strfry only: ws://198.51.100.1:7777, ws://198.51.100.2:7777).\n\
            --ticket-json + --suite wan-m runs the WAN-M rows (M1 + M9) instead, redeeming the\n\
            ticket the serve printed (§W6 authorizes --ticket-json for targeted iroh isolation runs;\n\
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

    // WAN-M: when --seed-dir + --asker-npub are passed, seed a collection, add the asker as a
    // contact in good standing, bind the manifest endpoint via the production path
    // (ensure_endpoint, presets::N0), and mint a ticket the probe redeems. The serve prints
    // ticket=<json>; the operator hands it to the probe via --ticket-json (§W6 authorizes this for
    // targeted iroh isolation runs; the E2E suite rides the full DM leg).
    if let (Some(seed_dir), Some(asker_npub)) =
        (args::flag_value(args, "--seed-dir"), args::flag_value(args, "--asker-npub"))
    {
        // AppIdentity holds ZeroizeOnDrop secrets and is not Clone; re-read it from the store so the
        // SharedIdentity owns its own copy (the live session the accept loop serves from).
        let app_id_for_plane = load_or_create_identity(&store)?;
        let live_npub: crate::identity_state::SharedIdentity =
            std::sync::Arc::new(tokio::sync::RwLock::new(Some(app_id_for_plane)));
        setup_manifest_plane(&store, &live_npub, seed_dir, asker_npub).await?;
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
// → decode_dms), and the production approval body (the same fns send_full_list drives: build_slug_manifest
// → ManifestPayload::seal → ensure_endpoint → issue_ticket → record_issued_ticket → send_dm_inner).
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
/// approval body (the same fns `send_full_list` drives: build manifest → bind endpoint → mint ticket →
/// record → DM the ticket). Runs forever; every approval is logged to stderr.
///
/// The loop is the serve side of WAN-E2E: it replaces the human "Send the full list" click with a
/// harness policy (auto-approve every request-DM from any asker). This is the ONLY deviation from the
/// production path, and it exists because a headless harness cannot click a button — every fn the loop
/// drives IS the production code.
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

    // The identity fields the approval body needs (snapshot once under the lock; AppIdentity is not Clone).
    let (identity, browse_key, transport_key, own_npub) = {
        let guard = live_npub.read().await;
        let id = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no identity loaded for the auto-approve loop"))?;
        (
            id.identity.clone(),
            id.browse_key.clone(),
            id.transport_key.clone(),
            id.npub(),
        )
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
            // Try to parse the DM as a manifest request (the "Ask owner" wire). The body is JSON tagged
            // `manifest_request`. If it doesn't parse, skip — it's a regular chat DM.
            let trimmed = msg.content.trim();
            if !trimmed.starts_with('{') {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("hb").and_then(|h| h.as_str()) != Some("manifest_request") {
                continue;
            }
            let slug = v.get("slug").and_then(|s| s.as_str()).unwrap_or("");
            let ask_nonce = v.get("ask_nonce").and_then(|n| n.as_str());
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

            // Drive the production approval body (the same fns send_full_list drives). If it fails,
            // log and continue — the loop stays alive for the next request.
            if let Err(e) = approve_request(
                store,
                &identity,
                &browse_key,
                &transport_key,
                &own_npub,
                live_npub,
                shared_relay,
                &msg.from,
                slug,
                ask_nonce,
            )
            .await
            {
                eprintln!("[serve] auto-approve failed for {slug}: {e:#}");
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Drive the production approval body for one request-DM: build the manifest, bind the endpoint, mint
/// the ticket, record it, and DM it to the asker. This is the body of `send_full_list` (fulfil.rs),
/// driven directly (the Tauri `State` wrapper is the only thing skipped — every fn called IS
/// production).
#[allow(clippy::too_many_arguments)]
async fn approve_request(
    store: &DataStore,
    identity: &hb_core::Identity,
    browse_key: &crate::identity_state::SessionBrowseKey,
    transport_key: &crate::identity_state::SessionTransportKey,
    own_npub: &str,
    live_npub: &crate::identity_state::SharedIdentity,
    shared_relay: &net::SharedRelay,
    asker_npub: &str,
    slug: &str,
    ask_nonce: Option<&str>,
) -> Result<()> {
    use crate::commands::collection::build_slug_manifest;
    use hb_core::ManifestPayload;

    // (1) Prove the manifest exists and fits (seal = the 8 MiB ceiling). Same check send_full_list
    // runs before promising anything.
    let envelope =
        build_slug_manifest(slug, store, identity, browse_key.bytes()).map_err(|e| anyhow!("{e}"))?;
    ManifestPayload::seal(&envelope)
        .map_err(|e| anyhow!("the E2E collection's manifest is over the transport ceiling: {e}"))?;

    // (2) Save the asker as a contact in good standing (contact_standing returns Good, which
    // authorize_redemption requires). The auto-approve policy trusts any asker — a real human reviews.
    save_asker_contact(store, asker_npub)?;

    // (3) Record a manifest ask the probe's claim gate expects — same nonce the ticket will echo.
    // The harness owns both sides, so the nonce is the one from the request-DM.
    let nonce = ask_nonce.unwrap_or("");
    let fp = envelope.snapshot_fingerprint.clone();
    store.record_manifest_ask(asker_npub, slug, &fp, &rfc3339_now(), nonce)?;

    // (4) Bind the endpoint via the production path (ensure_endpoint, Role::Listen). The accept loop
    // serves manifests for approved tickets.
    let source: Arc<dyn ManifestSource> =
        crate::manifest_source::StoreManifestSource::new(store.clone(), identity.clone(), browse_key.clone());
    let shared_ep = new_shared_endpoint();
    let endpoint = ensure_endpoint(&shared_ep, own_npub, live_npub, transport_key, source, Role::Listen)
        .await
        .map_err(|e| anyhow!("bind endpoint for approval: {e}"))?;

    // (5) Mint the ticket via the production path + persist it (record before the DM, same ordering
    // as send_full_list: a redeemer always presents a ticket we can recognise).
    let request_id = new_request_id();
    let ticket = issue_ticket(&endpoint, &request_id, slug, unix_now(), ask_nonce)?;
    store.record_issued_ticket(&IssuedTicketRecord {
        ticket: ticket.clone(),
        redeemer_npub: asker_npub.to_string(),
        consumed_at: None,
        delivered_bytes: None,
    })?;

    // (6) DM the ticket to the asker via the production NIP-17 path (send_dm_inner). This is the
    // ticket-DM leg the E2E probe polls for.
    let body = serde_json::to_string(&ticket)?;
    let client = net::client(identity, store, shared_relay).await?;
    let recipient = crate::commands::chat::parse_recipient(asker_npub)
        .map_err(|e| anyhow!("parse asker npub for ticket-DM: {e}"))?;
    let own_relays = net::relay_urls(store);
    crate::commands::chat::send_dm_inner(
        &client,
        identity,
        &recipient,
        &body,
        &own_relays,
        net::RELAY_TIMEOUT,
    )
    .await?;
    eprintln!(
        "[serve] auto-approve: ticket minted + DM'd for request {request_id} (slug '{slug}', nonce '{nonce}')"
    );
    // The endpoint lives for the process; forget it so the accept loop keeps running.
    std::mem::forget(endpoint);
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
// contact save (so contact_standing returns Good for the asker), record_manifest_ask (so the
// probe's claim_manifest_ask passes), ensure_endpoint with Role::Listen (the real accept loop +
// in-flight set), issue_ticket + record_issued_ticket (the real mint+authorize path, so
// consumed-exactly-once is the REAL mechanism).
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
) -> Result<()> {
    // Snapshot the identity fields the plane + the manifest build need. Read once under the lock;
    // these are owned copies (Identity is Clone; the keys are regenerable secrets held for the
    // session). The source carries its own snapshot because ManifestSource is sync and cannot await
    // a live handle — same reason transport_state keys the binding to the npub.
    let (identity, browse_key, transport_key, own_npub) = {
        let guard = live_npub.read().await;
        let id = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no identity loaded for the manifest plane"))?;
        (
            id.identity.clone(),
            id.browse_key.clone(),
            id.transport_key.clone(),
            id.npub(),
        )
    };

    // (1) Seed the collection — the production scan path (scan_selective + save_collection_draft).
    // A small collection suffices for M1 (the truncating case belongs to E2E). The slug is fixed so
    // re-seeding a running serve overwrites the same draft.
    let slug = "wan-it";
    seed_collection(store, &identity, &browse_key, seed_dir, slug)?;
    eprintln!("[serve] seeded collection '{slug}' from {seed_dir}");

    // (2) Add the asker as a contact in good standing — contact_standing returns Good, which
    // authorize_redemption requires. browse_key_hex is None on the asker side; the serve never
    // decrypts the asker's listings (it is the owner here).
    save_asker_contact(store, asker_npub)?;
    eprintln!("[serve] asker {asker_npub} saved as a contact (Good standing)");

    // (3) Record a manifest ask the probe's claim gate expects — same nonce the ticket will echo.
    // The harness owns both sides, so the nonce is harness-chosen and known to both.
    let nonce = "wan-it-nonce";
    store.record_manifest_ask(asker_npub, slug, "wan-it", &rfc3339_now(), nonce)?;

    // (4) Bind the endpoint via the production path (ensure_endpoint, Role::Listen). This is the
    // real accept loop + the in-flight set (one ticket, one delivery). StoreManifestSource is the
    // production source over the real data directory.
    let source: Arc<dyn ManifestSource> =
        StoreManifestSource::new(store.clone(), identity.clone(), browse_key.clone());
    let shared = new_shared_endpoint();
    let endpoint = ensure_endpoint(&shared, &own_npub, live_npub, &transport_key, source, Role::Listen)
        .await
        .map_err(|e| anyhow!("bind manifest endpoint: {e}"))?;
    eprintln!("[serve] manifest endpoint bound (presets::N0); accept loop running");

    // (5) Mint the ticket via the production path + persist it (record before the probe can redeem,
    // so a redeemer always presents a ticket we can recognise — the production ordering).
    let request_id = new_request_id();
    let ticket = issue_ticket(&endpoint, &request_id, slug, unix_now(), Some(nonce))?;
    store.record_issued_ticket(&IssuedTicketRecord {
        ticket: ticket.clone(),
        redeemer_npub: asker_npub.to_string(),
        consumed_at: None,
        delivered_bytes: None,
    })?;

    let body = serde_json::to_string(&ticket)?;
    println!("ticket={body}");
    eprintln!(
        "[serve] ticket minted for request {request_id} (slug '{slug}', nonce '{nonce}'); hand to probe via --ticket-json"
    );
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

/// Save `asker_npub` as a contact with no browse-key (the serve never browses the asker). This is
/// enough for `contact_standing` to return `Good`, which `authorize_redemption` requires.
fn save_asker_contact(store: &DataStore, asker_npub: &str) -> Result<()> {
    let contact = crate::store::CachedPeer {
        npub: asker_npub.to_string(),
        source: crate::store::ContactSource::Manual,
        browse_key_hex: None,
        petname: Some("wan-it-asker".to_string()),
        profile: None,
        collections: vec![],
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

/// Mint a request id: 128 bits of randomness, hex. Mirrors `fulfil::new_request_id` — unguessable
/// (the ticket's primary binding) and unique per approval.
fn new_request_id() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
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

    // WAN-U / WAN-C / WAN-T / WAN-D / WAN-R are probe-plays-both: every row constructs its own throwaway
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

/// Run the WAN-M rows against a live serve. Requires the serve's FULL share code (--peer hbk…) — the
/// acceptance gate (`accept_manifest_bytes`) needs the browse-key to decrypt — and `--ticket-json`
/// (the ticket the serve printed). The probe seeds its own identity + store from --data-dir.
async fn run_probe_wan_m(args: &[String], peer_str: &str) -> Result<ExitCode> {
    let ticket_json = args::flag_value(args, "--ticket-json")
        .ok_or_else(|| anyhow!("--suite wan-m requires --ticket-json <path|inline> (the serve's printed ticket)"))?;
    let data_dir = PathBuf::from(
        args::flag_value(args, "--data-dir").unwrap_or("./hb-wan-it-probe-data").to_string(),
    );

    let store = DataStore::new(data_dir.clone());
    // Persist the relay set (parity with serve — net::relay_urls reads Settings).
    let relays = args::collect_relays(args);
    store.save_settings(&Settings { relay_urls: relays.clone(), ..Default::default() })?;
    let app_id = load_or_create_identity(&store)?;

    // The dead-endpoint address for M9: an unroutable iroh EndpointAddr JSON. The transport parses
    // this via serde_json; an id + a TEST-NET-1 (RFC 5737) socket is guaranteed to fail the dial.
    let dead_addr = make_dead_endpoint_addr().await;

    let input = suite_wan_m::build_probe_input(app_id, store, peer_str, ticket_json, &dead_addr).await?;

    println!("# WAN-M probe against serve {}", input.serve_npub);
    println!("# ticket request_id={} slug={} nonce={:?}",
        input.live_ticket.request_id,
        input.live_ticket.slug,
        input.live_ticket.ask_nonce,
    );
    println!("# relay set: {}", relays.join(", "));

    // When --serve-data-dir points at the serve's data dir (single-machine smoke), the probe can read
    // the serve's store to confirm the ticket was consumed — the owner-side half of M1-once.
    let serve_store = args::flag_value(args, "--serve-data-dir").map(|d| DataStore::new(PathBuf::from(d)));

    let mut tap = tap::Tap::new();
    suite_wan_m::run(&mut tap, &input, serve_store.as_ref()).await;
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

    // When --serve-data-dir points at the serve's data dir (single-machine smoke), the probe can read
    // the serve's store to confirm the ticket was consumed — the owner-side half of E1.
    let serve_store =
        args::flag_value(args, "--serve-data-dir").map(|d| DataStore::new(PathBuf::from(d)));

    let mut tap = tap::Tap::new();
    suite_wan_e2e::run(&mut tap, &input, serve_store.as_ref()).await;
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

    // C1 — DM round-trip. Reuse the WAN-C C1 body (send + fetch+unwrap). Needs the relay set.
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
/// M20 W1 discriminator). Hardcoded: the canary is a daemon, not a probe that takes --relay.
fn canary_beacon_relays() -> Vec<String> {
    vec!["ws://198.51.100.1:7777".to_string(), "ws://198.51.100.2:7777".to_string()]
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

    // THROWAWAY calibration probe for HB-106's live repro sizing — not a real regression test,
    // deleted once the target seed size is found. Prints part count + index size for a real
    // directory so the live two-machine test can be sized without repeated network round-trips.
    #[test]
    #[ignore]
    fn hb106_calibrate_split_size() {
        let dir = std::env::var("HB106_SEED_DIR").expect("set HB106_SEED_DIR");
        let tmp = tempfile::tempdir().unwrap();
        let store = DataStore::new(tmp.path().to_path_buf());
        let identity = hb_core::Identity::generate();
        let browse_key = crate::identity_state::SessionBrowseKey::new([9u8; 32]);
        // scan_selective with an empty IncludeSet skips ALL subdirectories (is_included/
        // has_descendant_under both false for an empty checked list) — only root-level loose files
        // are scanned. So a nested seed tree needs every top-level dir explicitly checked.
        let top_level: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.metadata().map(|m| m.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let include = crate::commands::collection::IncludeSet::new(top_level);
        let (scanned, total_bytes) =
            scan_selective(std::path::Path::new(&dir), &include, &globset::GlobSet::empty()).unwrap();
        eprintln!("scanned top-level items={} total_bytes={total_bytes}", scanned.len());
        let listing_json = serde_json::to_string(&scanned).unwrap();
        eprintln!("raw entries json bytes={}", listing_json.len());
        let collection = hb_core::Collection {
            slug: "wan-it".to_string(),
            path_alias: "calib".to_string(),
            description: None,
            item_count: crate::commands::collection::count_items(&scanned),
            est_size: None,
            content_types: vec!["other".to_string()],
            tags: vec![],
            languages: vec![],
            visibility: hb_core::Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: scanned,
        };
        store.save_collection_draft(&collection).unwrap();
        let env = crate::commands::collection::build_slug_manifest(
            "wan-it",
            &store,
            &identity,
            browse_key.bytes(),
        )
        .unwrap();
        let parts = env.open(browse_key.bytes(), &identity.public_key()).unwrap();
        eprintln!(
            "parts={} index_bytes={} (window: >40000 and <=65408 is the target)",
            env.ciphertexts.len(),
            parts[0].len()
        );
        let ciphertext_total: usize = env.ciphertexts.iter().map(|c| c.len()).sum();
        eprintln!("ciphertext_total_bytes={ciphertext_total}");
        match hb_core::ManifestPayload::seal(&env) {
            Ok(payload) => eprintln!("SEAL OK, sealed_bytes={}", payload.as_bytes().len()),
            Err(e) => eprintln!("SEAL FAILED: {e}"),
        }
    }
}
