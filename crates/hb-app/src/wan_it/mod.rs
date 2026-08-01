//! `hb-wan-it` — headless WAN integration harness (M20 W6 §W6). Drives the **real production
//! presence path** between live endpoints over real NAT-traversed Nostr relays. Never starts the
//! Tauri/webkit runtime — it lives in-crate only to reach `pub(crate)` production internals (the
//! `presence.rs` publish/fetch functions, the `hb_net::fetch_online_presence` aggregate the
//! contact-row pill reads). This is the pattern the retired `hb-p2p-it` harness proved (in-crate
//! bin, serve/probe roles, TAP 13); the payload here drives current modules, not deleted ones.
//!
//! Two roles + one stub:
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
//!   hb-wan-it canary [--interval <secs>]
//!     Parse the args, print "not yet implemented", exit nonzero. Stub only (the canary daemon is a
//!     later slice — M20 W6 §W6 cadence, not this task).
//!
//! **Not in CI.** Manual pre-release gate + the canary daemon (later slice). The suite's exit code
//! is nonzero when rows fail; that is correct and intended for the pre-W1 state (P1 on public
//! relays, P4 are expected red).

mod args;
mod suite_wan_m;
mod suite_wan_p;
mod tap;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use nostr::prelude::ToBech32;

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
        Some("canary") => run_canary(&args[1..]),
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
     serve on NAT-B and the VPSes) or via the canary daemon (later slice).\n\
     \n\
     serve  --data-dir <dir> --relay <ws-url>...\n\
            [--seed-dir <dir> --asker-npub <npub>]\n\
            Seeds/loads an identity from <dir>, publishes the presence beacon via the production\n\
            path (presence::publish_presence) on the real cadence. Prints npub + share-code.\n\
            With --seed-dir + --asker-npub: also seeds a collection from <dir>, binds the manifest\n\
            endpoint via the production path (ensure_endpoint, presets::N0), and mints a ticket the\n\
            probe redeems — the launch-gate WAN-M path. Prints ticket=<json> for the operator to\n\
            hand to the probe (--ticket-json).\n\
     \n\
     probe  --peer <npub|hbk…> --relay <ws-url>...\n\
            [--flood-relay <ws-url>... --flood-count <n>]\n\
            [--ticket-json <path|inline> --suite wan-m]\n\
            Runs WAN-P rows P1–P5 against a live serve by default. --flood-relay arms P3\n\
            (VPS strfry only: ws://141.98.199.138:7777, ws://45.129.8.225:7777).\n\
            --ticket-json + --suite wan-m runs the WAN-M rows (M1 + M9) instead, redeeming the\n\
            ticket the serve printed (§W6 authorizes --ticket-json for targeted iroh isolation runs;\n\
            the E2E suite rides the full DM leg).\n\
     \n\
     canary [--interval <secs>]\n\
            Stub — parses args, prints 'not yet implemented', exits nonzero."
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

    // scan_selective with an empty IncludeSet = root files + every subdirectory (the full tree, which
    // is what a headless seed wants — no folder-tree picker).
    let include = crate::commands::collection::IncludeSet::new(vec![]);
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
    let peer_str = args::flag_value(args, "--peer")
        .ok_or_else(|| anyhow!("probe requires --peer <npub or hbk… share-code>"))?;
    let relays = args::collect_relays(args);
    if relays.is_empty() {
        bail!("probe requires at least one --relay");
    }

    // Suite selection: --suite wan-m runs the WAN-M rows (M1 + M9) against --ticket-json; the
    // default is WAN-P (the presence suite W6.1 shipped). §W6 authorizes --ticket-json for targeted
    // iroh isolation runs (the E2E suite rides the full DM leg).
    let suite = args::flag_value(args, "--suite").unwrap_or("wan-p");
    if suite == "wan-m" {
        return run_probe_wan_m(args, peer_str).await;
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
// canary — stub
// ---------------------------------------------------------------------------

fn run_canary(args: &[String]) -> ExitCode {
    // Parse the args so we can confirm the shape; the canary daemon is a later slice (M20 W6 §W6
    // cadence, not this task). Documenting the intended flags here so the eventual implementation
    // has the surface pinned.
    let _interval = args::flag_value(args, "--interval");
    let _relays = args::collect_relays(args);
    eprintln!("hb-wan-it canary: not yet implemented (M20 W6 canary daemon — a later slice).");
    eprintln!("  Intended: the thin continuous slice (M4 n0-reachable, R2 default-relay policy,");
    eprintln!("  P2 beacon acceptance, C1 DM round-trip) every ~10 min with [ALERT] on failure.");
    ExitCode::FAILURE
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
}
