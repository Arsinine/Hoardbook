//! hb-p2p-it — headless P2P integration harness (Test Plan Layer 3 / Suite P).
//!
//! Drives the **real** iroh protocol code in `node.rs` and `transfer.rs` between live
//! endpoints, so the iroh-first user flows that `hb-it` cannot reach (browse a peer over
//! `/hoardbook/node/1`, direct DMs, file download over `/hoardbook/xfer/1`) are validated
//! over real NAT-traversed QUIC. See TEST_PLAN.md §4.
//!
//! Two roles (same serve-on-VPS / probe-from-client model as the DHT slow path):
//!
//!   hb-p2p-it serve --data-dir <dir> [--relay <url>]...
//!     Seeds a known profile + shared collection + a shareable file, runs the real
//!     node + xfer accept loops, prints `hb_id` and `node_addr`, and (if relays are
//!     given) heartbeats its NodeAddr so a probe can discover it via the relay.
//!
//!   hb-p2p-it probe --peer-hb-id <id> [--relay <url>]... [--node-addr <json>] [--save-dir <dir>]
//!     Discovers the peer's NodeAddr (via the relay or `--node-addr`), runs Suite P,
//!     and emits TAP 13. Exit 0 if all pass, 1 otherwise.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use hb_core::{ChatMessage, Collection, DocType, HoardbookKeypair, Profile, SignedEnvelope, StoredKeypair};
use tokio::sync::Mutex;

use crate::node::{self, SharedDmQueue};
use crate::pex;
use crate::relay::RelayClient;
use crate::store::{DataStore, ShareSettings};
use crate::transfer::{self, DownloadRegistry};

// Fixtures shared by serve (seeds them) and probe (asserts against them).
const DISPLAY_NAME: &str = "p2p-it-peer";
const SHARED_SLUG: &str = "p2p-it-films";
const PRIVATE_SLUG: &str = "p2p-it-private";
const SHARED_FILE: &str = "sample.txt";
const SHARED_CONTENT: &[u8] = b"hoardbook p2p integration test payload\n";
// Throttled share used by the cancel probe (P3d): 256 KiB at 64 KiB/s ≈ 4 s stream,
// long enough to cancel mid-flight even over a fast link.
const SLOW_SLUG: &str = "p2p-it-slow";
const SLOW_FILE: &str = "big.bin";
const SLOW_FILE_SIZE: usize = 256 * 1024;
const SLOW_CAP_KBPS: u32 = 64;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(crate) async fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => match run_serve(&args[1..]).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => { eprintln!("[serve] error: {e:#}"); ExitCode::FAILURE }
        },
        Some("probe") => match run_probe(&args[1..]).await {
            Ok(code) => code,
            Err(e) => { eprintln!("[probe] error: {e:#}"); ExitCode::FAILURE }
        },
        other => {
            eprintln!(
                "usage: hb-p2p-it <serve|probe> [options]\n\
                 \n\
                 serve --data-dir <dir> [--relay <url>]...\n\
                 probe --peer-hb-id <id> [--relay <url>]... [--node-addr <json>] [--save-dir <dir>]"
            );
            if other.is_some() { eprintln!("unknown subcommand: {}", other.unwrap()); }
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build an iroh endpoint with the same preset + ALPNs the app uses (see lib.rs).
async fn build_endpoint(secret_bytes: &[u8; 32]) -> Result<iroh::Endpoint> {
    let secret_key = iroh::SecretKey::from_bytes(secret_bytes);
    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .alpns(vec![transfer::XFER_ALPN.to_vec(), node::NODE_ALPN.to_vec()])
        .bind()
        .await
        .context("bind iroh endpoint")
}

/// Load the persisted identity, or generate + persist a fresh one. The endpoint's
/// iroh identity is derived from the same 32 private bytes, so its node id == hb_id.
fn load_or_create_keypair(store: &DataStore) -> Result<HoardbookKeypair> {
    if let Some(stored) = store.load_keypair()? {
        let bytes: [u8; 32] = hex::decode(&stored.private_key_hex)
            .context("decode stored private key hex")?
            .try_into()
            .map_err(|_| anyhow!("stored private key is not 32 bytes"))?;
        return Ok(HoardbookKeypair::from_bytes(&bytes));
    }
    let kp = HoardbookKeypair::generate();
    let stored = StoredKeypair {
        version: 1,
        hb_id: kp.hb_id(),
        private_key_hex: hex::encode(kp.private_key_bytes()),
    };
    store.save_keypair(&stored)?;
    Ok(kp)
}

fn collect_relays(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--relay" {
            if let Some(v) = args.get(i + 1) { out.push(v.trim_end_matches('/').to_string()); }
        }
        i += 1;
    }
    out
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

/// Seed a known profile, a public collection, a follower-only collection, and the
/// shared file on disk — the fixtures Suite P asserts against.
fn seed_data(store: &DataStore, kp: &HoardbookKeypair, root: &Path) -> Result<()> {
    let profile = Profile {
        display_name: DISPLAY_NAME.to_string(),
        bio: None, tags: vec![], since: None, est_size: None, languages: vec![],
        contact_hint: None, email: None, location: None, social_links: vec![],
        willing_to: vec![], content_types: vec!["video".to_string()], updated: chrono::Utc::now(),
    };
    store.save_profile_draft(&profile)?;
    store.save_profile_signed(&SignedEnvelope::create(kp, DocType::Profile, &profile)?)?;

    let collection = Collection {
        slug: SHARED_SLUG.to_string(), path_alias: "P2P-IT Films".to_string(),
        description: Some("integration harness collection".to_string()),
        item_count: 1, est_size: None, content_types: vec!["video".to_string()],
        tags: vec!["p2p-it".to_string()], languages: vec![], last_updated: chrono::Utc::now(), listing: vec![],
    };
    store.save_collection_draft(&collection)?;
    store.save_collection_signed(SHARED_SLUG, &SignedEnvelope::create(kp, DocType::Collection, &collection)?)?;

    std::fs::create_dir_all(root).context("create shared root")?;
    std::fs::write(root.join(SHARED_FILE), SHARED_CONTENT).context("write shared file")?;
    std::fs::write(root.join(SLOW_FILE), vec![0xA5u8; SLOW_FILE_SIZE]).context("write slow file")?;
    let root_str = root.to_string_lossy().to_string();

    // Public, anyone may download.
    store.save_share_settings(SHARED_SLUG, &ShareSettings {
        enabled: true, root_path: Some(root_str.clone()), allowed_paths: vec![],
        speed_cap_kbps: None, download_limit: None, require_follow: false,
    })?;
    // Follower-only — a stranger probe must be rejected (P3b).
    store.save_share_settings(PRIVATE_SLUG, &ShareSettings {
        enabled: true, root_path: Some(root_str.clone()), allowed_paths: vec![],
        speed_cap_kbps: None, download_limit: None, require_follow: true,
    })?;
    // Throttled — gives the cancel probe (P3d) time to abort mid-stream.
    store.save_share_settings(SLOW_SLUG, &ShareSettings {
        enabled: true, root_path: Some(root_str), allowed_paths: vec![],
        speed_cap_kbps: Some(SLOW_CAP_KBPS), download_limit: None, require_follow: false,
    })?;
    Ok(())
}

async fn run_serve(args: &[String]) -> Result<()> {
    let data_dir = PathBuf::from(flag_value(args, "--data-dir").unwrap_or("./hb-p2p-it-data"));
    let relays = collect_relays(args);
    let dht_port: u16 = flag_value(args, "--dht-port").and_then(|s| s.parse().ok()).unwrap_or(6882);

    let store = DataStore::new(data_dir.clone());
    let kp = load_or_create_keypair(&store)?;
    let hb_id = kp.hb_id();
    let root = data_dir.join("shared");
    seed_data(&store, &kp, &root)?;

    let secret = *kp.private_key_bytes();
    let endpoint = build_endpoint(&secret).await?;
    let dm_queue: SharedDmQueue = Arc::new(Mutex::new(Vec::new()));
    let registry = Arc::new(DownloadRegistry::new());
    let peer_cache: pex::SharedPeerCache =
        Arc::new(Mutex::new(pex::PeerCache::load(store.clone())));

    tokio::spawn(serve_accept_loop(
        endpoint.clone(), store, hb_id.clone(), dm_queue, registry, peer_cache,
    ));

    // DHT TCP identity server on `dht_port` — what hb-it Suite H (H3/H6–H10) connects to.
    // Reuses the production `dht_service::run_identity_server`, which serves a signed
    // {hb_id, relay_urls, timestamp}. We never cancel it, so keep `_dht_cancel_tx` alive
    // for the process lifetime (dropping it would busy-loop the server's cancel select).
    let relay = Arc::new(RelayClient::new(relays.clone()));
    let identity: crate::SharedIdentity =
        Arc::new(tokio::sync::RwLock::new(Some(HoardbookKeypair::from_bytes(&secret))));
    let (_dht_cancel_tx, dht_cancel_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(crate::dht_service::run_identity_server(
        dht_port, identity, Arc::clone(&relay), dht_cancel_rx,
    ));

    // Give iroh a moment to gather addresses (relay home for NAT, direct for LAN).
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let node_addr = serde_json::to_string(&endpoint.addr()).context("serialize node addr")?;
    println!("hb_id={hb_id}");
    println!("node_addr={node_addr}");
    println!("dht_port={dht_port}");
    eprintln!("[serve] {hb_id} listening; node + xfer + dht-identity({dht_port}) servers up");

    if relays.is_empty() {
        eprintln!("[serve] no --relay given; probe must use --node-addr (heartbeat disabled)");
        std::future::pending::<()>().await;
        return Ok(());
    }

    loop {
        let na = serde_json::to_string(&endpoint.addr()).ok();
        if let Err(e) = relay.send_heartbeat(&kp, na).await {
            eprintln!("[serve] heartbeat error: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

/// The app's unified accept loop, minus Tauri: dispatch by ALPN to the real handlers.
async fn serve_accept_loop(
    endpoint: iroh::Endpoint,
    store: DataStore,
    own_hb_id: String,
    dm_queue: SharedDmQueue,
    registry: Arc<DownloadRegistry>,
    peer_cache: pex::SharedPeerCache,
) {
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let incoming = match endpoint.accept().await {
            Some(inc) => inc,
            None => { eprintln!("[serve] endpoint closed; accept loop exiting"); break; }
        };
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };
        let store = store.clone();
        let hb_id = own_hb_id.clone();
        let dm_queue = dm_queue.clone();
        let registry = registry.clone();
        let pex_state = pex::PexState {
            cache: Arc::clone(&peer_cache),
            endpoint: endpoint.clone(),
        };
        tokio::spawn(async move {
            let _permit = permit;
            let accepting = match incoming.accept() {
                Ok(a) => a,
                Err(e) => { eprintln!("[serve] incoming reject: {e}"); return; }
            };
            let conn = match accepting.await {
                Ok(c) => c,
                Err(e) => { eprintln!("[serve] handshake error: {e}"); return; }
            };
            let alpn = conn.alpn().to_vec();
            if alpn == transfer::XFER_ALPN {
                // Drive the real production handler directly — it owns the bi-stream
                // accept and the graceful-close drain.
                if let Err(e) = transfer::handle_xfer_connection(conn, store, registry).await {
                    eprintln!("[serve] xfer session error: {e}");
                }
            } else if alpn == node::NODE_ALPN {
                // The Tauri-free core of handle_node_connection (primary stream → PEX →
                // drain) — the full wrapper only adds the desktop unread-message
                // notification (needs AppHandle), passed here as a no-op.
                if let Err(e) = node::handle_node_connection_core(
                    &conn, &store, &hb_id, &dm_queue, Some(&pex_state), |_| {},
                )
                .await
                {
                    eprintln!("[serve] node session error: {e}");
                }
            } else {
                eprintln!("[serve] unknown ALPN, dropping");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// probe — Suite P
// ---------------------------------------------------------------------------

async fn run_probe(args: &[String]) -> Result<ExitCode> {
    let peer_hb_id = flag_value(args, "--peer-hb-id")
        .ok_or_else(|| anyhow!("probe requires --peer-hb-id <hb1_...>"))?
        .to_string();
    let relays = collect_relays(args);
    let save_dir = PathBuf::from(
        flag_value(args, "--save-dir")
            .map(str::to_string)
            .unwrap_or_else(|| std::env::temp_dir().join("hb-p2p-it").to_string_lossy().to_string()),
    );
    std::fs::create_dir_all(&save_dir).context("create save dir")?;

    // Discover the peer's NodeAddr: explicit --node-addr, else via the relay (the real chain).
    let relay_client = if relays.is_empty() { None } else { Some(RelayClient::new(relays)) };
    let node_addr = match flag_value(args, "--node-addr") {
        Some(na) => na.to_string(),
        None => {
            let relay = relay_client
                .as_ref()
                .ok_or_else(|| anyhow!("probe needs --node-addr <json> or at least one --relay to discover the peer"))?;
            let peer = relay.fetch_peer(&peer_hb_id).await.context("relay fetch_peer")?;
            peer.node_addr
                .ok_or_else(|| anyhow!("relay has no node_addr for {peer_hb_id} (is the peer online?)"))?
        }
    };

    let client_kp = HoardbookKeypair::generate(); // ephemeral client identity
    let secret = *client_kp.private_key_bytes();
    let endpoint = build_endpoint(&secret).await?;

    let mut tap = Tap::new();
    tap.check("P1: browse profile+collection over iroh",
        probe_browse(&endpoint, &node_addr, &peer_hb_id).await);
    tap.check("P2a: direct DM delivered over iroh",
        probe_dm_ok(&endpoint, &node_addr, &peer_hb_id, &client_kp).await);
    tap.check("P2b: wrong-recipient DM rejected",
        probe_dm_wrong_recipient(&endpoint, &node_addr, &client_kp).await);
    tap.check("P3a: download shared file (bytes + sha256 match)",
        probe_download_ok(&endpoint, &node_addr, &peer_hb_id, &save_dir).await);
    tap.check("P3b: require_follow collection rejects stranger",
        probe_download_requires_follow(&endpoint, &node_addr, &peer_hb_id, &save_dir).await);
    tap.check("P3c: invalid slug rejected",
        probe_download_bad_slug(&endpoint, &node_addr, &peer_hb_id, &save_dir).await);
    tap.check("P3d: cancel aborts mid-download and removes the partial file",
        probe_download_cancel(&endpoint, &node_addr, &peer_hb_id, &save_dir).await);
    tap.check("P5: PEX gossip returns the peer's self-entry",
        probe_pex(&endpoint, &node_addr, &peer_hb_id).await);
    match relay_client {
        Some(ref relay) => tap.check(
            "P4: relay DM store-and-forward via the app's RelayClient",
            probe_dm_relay_roundtrip(relay, &client_kp).await,
        ),
        None => tap.skip("P4: relay DM store-and-forward via the app's RelayClient",
            "no --relay given"),
    }

    endpoint.close().await;
    Ok(tap.finish())
}

/// P1 — browse a peer's profile + collection over `/hoardbook/node/1`.
async fn probe_browse(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str) -> Result<()> {
    let (profile, collections) =
        node::fetch_profile_via_iroh(endpoint, node_addr, peer_hb_id, None).await?;
    let profile = profile.ok_or_else(|| anyhow!("no verified profile returned"))?;
    if profile.display_name != DISPLAY_NAME {
        bail!("display_name = {:?}, expected {DISPLAY_NAME:?}", profile.display_name);
    }
    let col = collections.iter().find(|c| c.slug == SHARED_SLUG)
        .ok_or_else(|| anyhow!("collection {SHARED_SLUG} not in the {} returned", collections.len()))?;
    if !col.content_types.iter().any(|t| t == "video") {
        bail!("collection content_types missing 'video': {:?}", col.content_types);
    }
    Ok(())
}

/// P2a — a DM addressed to the peer is accepted by the node server.
async fn probe_dm_ok(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str, kp: &HoardbookKeypair) -> Result<()> {
    let msg = ChatMessage {
        to: peer_hb_id.to_string(), content: "p2p-it hello".to_string(),
        encrypted: false, sent_at: chrono::Utc::now(),
    };
    let env = SignedEnvelope::create(kp, DocType::Message, &msg)?;
    node::send_dm_via_iroh(endpoint, node_addr, &env, None).await
}

/// P2b — a DM addressed to someone else must be rejected (validate_dm recipient check).
async fn probe_dm_wrong_recipient(endpoint: &iroh::Endpoint, node_addr: &str, kp: &HoardbookKeypair) -> Result<()> {
    let other = HoardbookKeypair::generate();
    let msg = ChatMessage {
        to: other.hb_id(), content: "misaddressed".to_string(),
        encrypted: false, sent_at: chrono::Utc::now(),
    };
    let env = SignedEnvelope::create(kp, DocType::Message, &msg)?;
    match node::send_dm_via_iroh(endpoint, node_addr, &env, None).await {
        Ok(()) => bail!("server accepted a DM addressed to a different recipient"),
        // Assert the *protocol* rejected it, not that the transport merely failed.
        Err(e) => {
            let s = e.to_string();
            if s.contains("rejected") || s.contains("recipient mismatch") {
                Ok(())
            } else {
                bail!("expected a node-server rejection, got transport error: {s}")
            }
        }
    }
}

/// P3a — download the shared file over `/hoardbook/xfer/1`; integrity-checked.
async fn probe_download_ok(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str, save_dir: &Path) -> Result<()> {
    let registry = Arc::new(DownloadRegistry::new());
    let save_path = save_dir.join("downloaded.txt");
    let expected = transfer::sha256_bytes(SHARED_CONTENT);
    let n = transfer::download_file_inner(
        endpoint, node_addr, peer_hb_id,
        SHARED_SLUG, SHARED_FILE, save_path.to_str().context("save path not utf-8")?,
        Some(expected), registry.next_id(), registry.clone(),
        |_ev| {},
    ).await?;
    if n as usize != SHARED_CONTENT.len() {
        bail!("downloaded {n} bytes, expected {}", SHARED_CONTENT.len());
    }
    let got = std::fs::read(&save_path).context("re-read downloaded file")?;
    if got != SHARED_CONTENT {
        bail!("downloaded content does not match the seeded file");
    }
    Ok(())
}

/// P3b — a stranger (no follow relationship) must be denied the follower-only collection.
async fn probe_download_requires_follow(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str, save_dir: &Path) -> Result<()> {
    let registry = Arc::new(DownloadRegistry::new());
    let save_path = save_dir.join("private.txt");
    match transfer::download_file_inner(
        endpoint, node_addr, peer_hb_id,
        PRIVATE_SLUG, SHARED_FILE, save_path.to_str().context("save path not utf-8")?,
        None, registry.next_id(), registry.clone(),
        |_ev| {},
    ).await {
        Ok(_) => bail!("stranger was allowed to download a require_follow-only collection"),
        // Assert the follower-gate denied it specifically, not a generic transport error.
        Err(e) => {
            let s = e.to_string();
            if s.contains("restricted to followers") {
                Ok(())
            } else {
                bail!("expected a follower-gate rejection, got: {s}")
            }
        }
    }
}

/// P3c — a syntactically invalid slug must be rejected before it reaches the filesystem
/// (M7 guard, exercised over the real wire).
async fn probe_download_bad_slug(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str, save_dir: &Path) -> Result<()> {
    let registry = Arc::new(DownloadRegistry::new());
    let save_path = save_dir.join("bad-slug.txt");
    match transfer::download_file_inner(
        endpoint, node_addr, peer_hb_id,
        "../escape", SHARED_FILE, save_path.to_str().context("save path not utf-8")?,
        None, registry.next_id(), registry.clone(),
        |_ev| {},
    ).await {
        Ok(_) => bail!("server accepted a download with an invalid collection slug"),
        Err(e) => {
            let s = e.to_string();
            if s.contains("Invalid collection slug") {
                Ok(())
            } else {
                bail!("expected the slug-validation rejection, got: {s}")
            }
        }
    }
}

/// P3d — cancelling mid-stream aborts the download and removes the partial file.
/// Uses the throttled collection so the stream is guaranteed to still be in flight.
async fn probe_download_cancel(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str, save_dir: &Path) -> Result<()> {
    let registry = Arc::new(DownloadRegistry::new());
    let save_path = save_dir.join("cancelled.bin");
    let save_str = save_path.to_str().context("save path not utf-8")?.to_string();
    let download_id = registry.next_id();

    let ep = endpoint.clone();
    let na = node_addr.to_string();
    let peer = peer_hb_id.to_string();
    let reg = registry.clone();
    let task = tokio::spawn(async move {
        transfer::download_file_inner(
            &ep, &na, &peer,
            SLOW_SLUG, SLOW_FILE, &save_str,
            None, download_id, reg,
            |_ev| {},
        ).await
    });

    // 256 KiB at 64 KiB/s streams for ~4 s; cancel while it is mid-flight.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    if !registry.cancel(download_id).await {
        let _ = task.await;
        bail!("cancel token was no longer registered — download finished before the cancel fired");
    }

    match task.await.context("join download task")? {
        Ok(n) => bail!("download completed ({n} bytes) despite cancellation"),
        Err(e) => {
            let s = e.to_string();
            if !s.to_lowercase().contains("cancelled") {
                bail!("expected the cancellation error, got: {s}");
            }
            if save_path.exists() {
                bail!("partial file was not removed after cancel");
            }
            Ok(())
        }
    }
}

/// P4 — DM relay leg through the app's own RelayClient: encrypt + sign + publish,
/// then fetch back via the authenticated mailbox read and decrypt (AAD-bound).
async fn probe_dm_relay_roundtrip(relay: &RelayClient, kp: &HoardbookKeypair) -> Result<()> {
    let sender = HoardbookKeypair::generate();
    let sent_at = chrono::Utc::now();
    let plaintext = format!("p2p-it relay dm {}", sent_at.timestamp_millis());

    let aad = crate::commands::chat::message_aad(&sender.hb_id(), &kp.hb_id(), &sent_at.to_rfc3339());
    let recipient_pub = hb_core::hb_id_decode(&kp.hb_id())?;
    let ciphertext = sender.encrypt_for(&recipient_pub, &plaintext, &aad)?;
    let msg = ChatMessage { to: kp.hb_id(), content: ciphertext, encrypted: true, sent_at };
    let env = SignedEnvelope::create(&sender, DocType::Message, &msg)?;

    relay.publish("message", &env).await.context("publish DM via RelayClient")?;

    let inbox = relay.fetch_messages(kp).await.context("fetch_messages via RelayClient")?;
    let (from, got) = inbox
        .iter()
        .find(|(from, m)| *from == sender.hb_id() && m.sent_at == sent_at)
        .ok_or_else(|| anyhow!("published DM not returned by the relay mailbox"))?;
    if !got.encrypted {
        bail!("relay returned the DM with encrypted=false");
    }
    let sender_pub = hb_core::hb_id_decode(from)?;
    let decrypted = kp.decrypt_from(&sender_pub, &got.content, &aad)
        .map_err(|e| anyhow!("decrypt failed: {e}"))?;
    if decrypted != plaintext {
        bail!("decrypted content mismatch: {decrypted:?}");
    }
    Ok(())
}

/// P5 — peer address gossip: after a primary request, a PEX stream on the same
/// connection must return a list containing the peer's own (verifiable) self-entry.
async fn probe_pex(endpoint: &iroh::Endpoint, node_addr: &str, peer_hb_id: &str) -> Result<()> {
    let peer_addr: iroh::EndpointAddr =
        serde_json::from_str(node_addr).context("parse peer EndpointAddr")?;
    let conn = endpoint
        .connect(peer_addr, node::NODE_ALPN)
        .await
        .context("iroh connect")?;

    // Primary request first — the server only serves PEX after it.
    let (send, recv) = conn.open_bi().await.context("open_bi")?;
    let _ = node::fetch_profile_via_stream(send, recv, peer_hb_id).await?;

    // Now the gossip stream. We offer an empty list (ephemeral probe identity).
    let (send, recv) = conn.open_bi().await.context("open pex stream")?;
    let theirs = pex::pex_exchange_initiate(send, recv, vec![]).await?;
    conn.close(0u32.into(), b"");

    let self_entry = theirs
        .iter()
        .find(|e| e.hb_id == peer_hb_id)
        .ok_or_else(|| anyhow!("peer's self-entry missing from PEX response ({} entries)", theirs.len()))?;
    let addr_json = self_entry.node_addr.as_ref()
        .ok_or_else(|| anyhow!("peer's self-entry has no node_addr"))?;
    let addr: iroh::EndpointAddr =
        serde_json::from_str(addr_json).context("parse gossiped EndpointAddr")?;
    let expected = iroh::EndpointId::from_bytes(&hb_core::hb_id_decode(peer_hb_id)?)
        .map_err(|e| anyhow!("peer hb_id is not a valid endpoint key: {e}"))?;
    if addr.id != expected {
        bail!("gossiped self-entry id does not match the peer's hb_id");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal TAP 13 writer (trailing plan)
// ---------------------------------------------------------------------------

struct Tap { n: usize, failed: usize }

impl Tap {
    fn new() -> Self {
        println!("TAP version 13");
        Self { n: 0, failed: 0 }
    }
    fn check(&mut self, name: &str, result: Result<()>) {
        self.n += 1;
        match result {
            Ok(()) => println!("ok {} - {name}", self.n),
            Err(e) => {
                self.failed += 1;
                println!("not ok {} - {name}\n  ---\n  detail: {e}\n  ...", self.n);
            }
        }
    }
    fn skip(&mut self, name: &str, reason: &str) {
        self.n += 1;
        println!("ok {} - {name} # SKIP {reason}", self.n);
    }
    fn finish(self) -> ExitCode {
        println!("1..{}", self.n);
        eprintln!("\n{} tests: {} passed, {} failed", self.n, self.n - self.failed, self.failed);
        if self.failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
    }
}
