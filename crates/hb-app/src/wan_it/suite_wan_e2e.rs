//! WAN-E2E — the full user path, the strongest regression net (M20 W6 §W6). One row exercises the
//! whole M16+M18 pipeline — **share code → relay → DM → ticket** — with coordination riding the REAL
//! product paths, not `--ticket-json`. This is the difference from WAN-M: WAN-M proved the iroh plane
//! in isolation; WAN-E2E proves the *funnel* that delivers a ticket to a human who asked for it.
//!
//! **E1 — the truncating paywall round-trip.** Serve publishes a collection big enough that the teaser
//! truncates (the listing JSON exceeds [`LISTING_MAX_BYTES`] = 40 KB, the real truncation threshold —
//! NOT an entry count). Probe browses the paywall teaser via the share code (asserts `truncated == true`)
//! → sends the production **request-DM** (the ask-owner path: `build_manifest_request` → `send_dm_inner`,
//! the NIP-17 gift-wrap wire the "Ask owner" button drives) → serve polls its DM inbox, **auto-approves**
//! (harness policy) by driving the production approval body (`send_full_list`'s inner fns: build manifest
//! → bind endpoint → mint ticket → record → DM the ticket) → probe polls DMs for the ticket → redeems it
//! (claim → fetch_manifest → accept_manifest_bytes → spend) → asserts the **full tree** parses with entry
//! count > the truncated teaser's, slug matches → serve-side receipt shows consumed.
//!
//! **E2 — staleness gate over a real link.** The probe sends a fresh request-DM, the serve auto-approves
//! and delivers a ticket, and the probe redeems it — but passes a **synthetic stale fingerprint** (all
//! zeros, guaranteed to differ from any real snapshot fingerprint) as `newest_fingerprint`. The production
//! gate (`open_manifest`: `stale = !envelope.matches_fingerprint(newest_fp)`) fires on ANY mismatch, so
//! `stale == true`. The full tree still delivers (staleness is surfaced, never blocking — the production
//! UX rule). The gate is a symmetric fingerprint mismatch check; a synthetic fingerprint exercises the
//! EXACT production code path over a real link without needing a coordinated republish.
//!
//! **Honest red.** Nothing here is `# TODO`/skip. A leg that fails on environment grounds (relay didn't
//! propagate the DM in time) is an honest `not ok` with the per-step evidence dump. The suite's nonzero
//! exit code is the correct signal for a flaky relay.
//!
//! **Flake policy (P3b precedent):** every DM-poll leg retries ×3 with a settle between attempts (DM
//! propagation over live relays can take seconds); every failure is a recorded data point.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use hb_core::TransportTicket;
use nostr::prelude::ToBech32;
use nostr::prelude::*;

use crate::commands::browse::{accept_manifest_bytes, ImportedManifest};
use crate::commands::chat::{parse_recipient, send_dm_inner};
use crate::identity_state::{AppIdentity, SharedIdentity};
use crate::store::DataStore;
use crate::transport::{fetch_manifest, ManifestSource};
use crate::transport_state::{ensure_endpoint, new_shared_endpoint, Role};
use crate::wan_it::tap::Tap;

// ---------------------------------------------------------------------------
// Constants — timeouts, retries, settle (match WAN-M / WAN-P conventions)
// ---------------------------------------------------------------------------

/// The truncation threshold (the production constant from `commands::collection::LISTING_MAX_BYTES`).
/// A listing whose serialized JSON exceeds this many bytes publishes as a truncated paywall teaser.
/// This is a BYTE budget, not an entry count — the harness generates enough small files to exceed it.
const LISTING_MAX_BYTES: usize = 40_000;

/// Relay handshake/fetch timeout (matches `net::RELAY_TIMEOUT` = 10 s, rounded up for long-haul).
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);

/// Settle between a publish and a read (lets the relay index the event). DM propagation over a live
/// relay can take seconds; this is the same settle WAN-P uses.
const SETTLE: Duration = Duration::from_secs(3);

/// DM-poll legs retry this many times before recording a failure (flake policy). Each attempt connects,
/// fetches the inbox, and checks for the message we expect.
const DM_POLL_RETRIES: u32 = 6;

/// The dial+fetch deadline for the live redeem (same as WAN-M's `LIVE_REDEEM_TIMEOUT`). Generous: hole
/// punching through n0 relays can take tens of seconds on a cold path.
const LIVE_REDEEM_TIMEOUT: Duration = Duration::from_secs(120);

/// Long-haul live redemptions retry this many times before recording a failure (flake policy).
const LIVE_REDEEM_RETRIES: u32 = 3;

/// NIP-59 fuzzes a gift wrap's OUTER `created_at` up to 2 days into the past (matches
/// `chat::DM_FETCH_MARGIN_SECS`). Documented here for reference; the E2E poll uses an unbounded fetch
/// (no since-cursor) because the inbox is polled for a specific ticket-DM, not incrementally cached.
#[allow(dead_code)]
const DM_FETCH_MARGIN_SECS: u64 = 48 * 60 * 60;

// ---------------------------------------------------------------------------
// Probe input — built by run_probe_wan_e2e from the parsed args
// ---------------------------------------------------------------------------

/// The input the E2E probe needs to drive the full pipeline. Built from the parsed args; holds the
/// identity *fields* (not the `AppIdentity`, which owns ZeroizeOnDrop secrets and is not Clone).
pub struct ProbeInput {
    /// The probe's Nostr identity (signs events; Clone).
    pub identity: hb_core::Identity,
    /// The probe's account browse-key (for StoreManifestSource — unused on the redeem path, but
    /// ensure_endpoint needs a source handle).
    pub browse_key: crate::identity_state::SessionBrowseKey,
    /// The probe's transport secret (the manifest plane's node key).
    pub transport_key: crate::identity_state::SessionTransportKey,
    /// The probe's npub (bech32) — used as the owner_npub for ensure_endpoint.
    pub probe_npub: String,
    /// The probe's data store (contacts, manifest asks, manifest cache — the real gate reads these).
    pub store: DataStore,
    /// The serve peer's full share code (npub + browse-key) — `accept_manifest_bytes` needs the
    /// browse-key to decrypt, and `browse_share_code` needs the full code.
    pub serve_share_code: String,
    /// The serve peer's npub (bech32).
    pub serve_npub: String,
}

/// The fixed slug the harness seeds on both sides (matches WAN-M's convention).
const SEED_SLUG: &str = "wan-e2e";

// ---------------------------------------------------------------------------
// run — the two E2E rows
// ---------------------------------------------------------------------------

/// Run the WAN-E2E rows against a live serve. E1 is the full truncating round-trip; E2 is the
/// staleness gate over a republished (changed-fingerprint) collection. Each row is an honest TAP check.
///
/// `serve_store` is the serve's store handle (single-machine smoke only — the probe reads the serve's
/// receipt to confirm the ticket was consumed). Across machines, the orchestrator correlates the TAP
/// output and this leg is informational.
pub async fn run(tap: &mut Tap, probe: &ProbeInput, serve_store: Option<&DataStore>) {
    // E1 — the full truncating paywall round-trip.
    tap.check(
        "E1: browse truncated teaser → request-DM → approve → ticket-DM → redeem → full tree > teaser",
        e1_truncated_round_trip(probe, serve_store).await,
    );

    // E2 — staleness gate fires when newest_fingerprint mismatches the manifest's.
    tap.check(
        "E2: staleness gate fires over a real link (fingerprint mismatch → stale == true, tree still delivers)",
        e2_staleness_after_republish(probe).await,
    );
}

// ---------------------------------------------------------------------------
// E1 — the truncating paywall round-trip
// ---------------------------------------------------------------------------

/// E1: the full pipeline. Browse the truncated teaser → request-DM → (serve auto-approves) → ticket-DM
/// → redeem → assert full tree > teaser, slug matches, receipt consumed.
///
/// The serve side (auto-approve loop) runs as a SEPARATE process (`hb-wan-it serve --auto-approve`). The
/// probe's job here is: browse, ask, wait for the ticket, redeem, assert. The serve's receipt is checked
/// via `serve_store` when the harness runs single-machine.
async fn e1_truncated_round_trip(probe: &ProbeInput, serve_store: Option<&DataStore>) -> Result<(), String> {
    // (1) Browse the teaser via the production path (browse_share_code). Assert truncated == true.
    let teaser = browse_truncated_teaser(probe).await?;
    let teaser_entry_count = teaser.kept_entry_count;
    let teaser_fingerprint = teaser.snapshot_fingerprint.clone();
    eprintln!(
        "   E1 browsed truncated teaser: {} kept entries (of {}), fingerprint={}",
        teaser_entry_count,
        teaser.total_items.unwrap_or(teaser_entry_count),
        teaser_fingerprint.as_deref().unwrap_or("(none)")
    );

    // (2) Send the request-DM via the production path (build_manifest_request → send_dm_inner). This is
    // the exact wire the "Ask owner" button drives. The ask is recorded locally (the production ordering:
    // record AFTER send_dm_inner resolves, so a failed publish leaves no trace).
    let ask_nonce = send_request_dm(probe, &teaser_fingerprint).await?;
    eprintln!("   E1 sent request-DM (nonce={ask_nonce})");

    // (3) Poll the DM inbox for the ticket-DM. The serve's auto-approve loop sees the request, drives
    // send_full_list's body, and DMs the ticket back. This leg is the slow one (DM propagation over a
    // live relay can take seconds).
    let ticket = poll_for_ticket_dm(probe, &ask_nonce).await?;
    eprintln!(
        "   E1 received ticket-DM: request_id={}, slug={}",
        ticket.request_id, ticket.slug
    );

    // (4) Redeem the ticket through the production path (claim → fetch_manifest → accept_manifest_bytes
    // → spend). The acceptance gate runs INSIDE fetch_manifest, same as WAN-M.
    let imported = redeem_ticket(probe, &ticket, teaser_fingerprint.as_deref()).await?;
    eprintln!(
        "   E1 redeemed: full tree has {} entries, stale={}",
        imported.collection.collection.listing.len(),
        imported.stale
    );

    // (5) Assert the full tree > the truncated teaser's kept count, and the slug matches.
    let full_count = imported.collection.collection.listing.len();
    if full_count <= teaser_entry_count {
        return Err(format!(
            "the full tree ({full_count} entries) did not exceed the truncated teaser's kept count \
             ({teaser_entry_count}) — the manifest did not deliver more than the teaser showed"
        ));
    }
    if imported.slug != SEED_SLUG {
        return Err(format!(
            "slug mismatch: expected '{SEED_SLUG}', got '{}'",
            imported.slug
        ));
    }
    // E1's manifest is built from the same collection the teaser came from, so stale must be false.
    if imported.stale {
        return Err(
            "E1 manifest is stale against the teaser it was browsed from — fingerprints should match \
             (the collection was not republished between browse and redeem)"
                .to_string(),
        );
    }

    // (6) Serve-side receipt: when the harness runs single-machine, the serve's store shows the ticket
    // consumed. This is the strongest assertion (the receipt was persisted); across machines it is
    // informational.
    if let Some(serve) = serve_store {
        match serve.load_issued_ticket(&ticket.request_id) {
            Ok(Some(rec)) => match rec.consumed_at {
                Some(ts) => eprintln!(
                    "   E1 owner-side confirmation: ticket consumed_at={ts}, delivered_bytes={:?}",
                    rec.delivered_bytes
                ),
                None => {
                    return Err(
                        "the owner-side store shows the ticket NOT consumed after a successful redeem — \
                         the receipt was not persisted"
                            .to_string(),
                    )
                }
            },
            Ok(None) => {
                return Err(
                    "the owner-side store has no record of this request id — the serve did not mint \
                     this ticket through its store"
                        .to_string(),
                )
            }
            Err(e) => eprintln!("   E1 could not read serve store (informational): {e}"),
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// E2 — staleness gate after republish
// ---------------------------------------------------------------------------

/// E2: exercise the staleness gate over a real link. The probe sends a fresh request-DM, the serve
/// auto-approves and delivers a ticket, and the probe redeems it — but passes a **synthetic stale
/// fingerprint** (all zeros, guaranteed to differ from any real snapshot fingerprint) as
/// `newest_fingerprint`. The production gate (`open_manifest`:
/// `stale = !envelope.matches_fingerprint(newest_fp)`) fires on ANY mismatch, so `stale == true`.
///
/// **Why a synthetic fingerprint instead of a coordinated republish?** In a two-machine test the
/// operator coordinates the republish (kill serve, rewrite seed, restart). In a single-machine smoke
/// the harness cannot trigger the republish between E1 and E2 without a side channel. The staleness
/// gate is a symmetric fingerprint mismatch check — it does not care WHICH side is "newer", only THAT
/// they differ. A synthetic all-zero fingerprint is guaranteed to differ from any real sha256-based
/// snapshot fingerprint, so the gate fires deterministically. This exercises the EXACT production code
/// path (`open_manifest`'s fingerprint comparison) over a real link (the manifest arrives via the
/// production DM → ticket → iroh pipeline), which is what makes E2 the staleness regression net.
///
/// **Production semantics asserted:** `stale == true` when `newest_fingerprint` != manifest fingerprint.
/// The full tree still delivers (staleness is surfaced, never blocking — the production UX rule).
async fn e2_staleness_after_republish(probe: &ProbeInput) -> Result<(), String> {
    // A synthetic fingerprint that is GUARANTEED to differ from any real snapshot fingerprint (a real
    // fingerprint is a sha256 hex string; all-zeros is not a sha256 any real tree produces). This is the
    // "stale newest_fingerprint" the staleness gate exists to detect.
    let stale_fp = "0000000000000000000000000000000000000000000000000000000000000000";

    // (1) Browse the teaser to get the CURRENT fingerprint (for evidence, and to confirm the serve is
    // still reachable).
    let teaser = browse_truncated_teaser(probe).await?;
    eprintln!(
        "   E2 browsed teaser: fingerprint={}",
        teaser.snapshot_fingerprint.as_deref().unwrap_or("(none)")
    );

    // (2) Send a fresh request-DM (new nonce). The auto-approve loop will answer it.
    let ask_nonce = send_request_dm(probe, &teaser.snapshot_fingerprint).await?;
    eprintln!("   E2 sent fresh request-DM (nonce={ask_nonce})");

    // (3) Poll for the ticket-DM.
    let ticket = poll_for_ticket_dm(probe, &ask_nonce).await?;

    // (4) Redeem the ticket, passing the SYNTHETIC STALE fingerprint as `newest_fingerprint`. The
    // production gate compares the manifest's real fingerprint against `stale_fp` → mismatch →
    // stale == true. This is the exact code path `redeem_manifest_ticket` drives when a browser views
    // a teaser whose fingerprint differs from the delivered manifest's.
    let imported = redeem_ticket(probe, &ticket, Some(stale_fp)).await?;
    eprintln!(
        "   E2 redeemed with synthetic stale fingerprint: stale={}",
        imported.stale
    );

    // (5) The assertion: the staleness gate FIRES.
    if !imported.stale {
        return Err(
            "the staleness gate did NOT fire with a synthetic stale fingerprint — the manifest's \
             fingerprint matched the probe's synthetic newest_fingerprint, which should be impossible \
             (all-zeros is not a real sha256). The gate may not be comparing fingerprints correctly."
                .to_string(),
        );
    }
    // The full tree still delivered (staleness is surfaced, never blocking — the production UX rule).
    let full_count = imported.collection.collection.listing.len();
    if full_count == 0 {
        return Err("the stale manifest delivered an empty tree — staleness is surfaced but the tree still imports".to_string());
    }
    eprintln!("   E2 staleness gate fired correctly (stale == true), full tree still delivered ({full_count} entries)");

    Ok(())
}

// ---------------------------------------------------------------------------
// Legs — browse teaser, send request-DM, poll for ticket-DM, redeem
// ---------------------------------------------------------------------------

/// The result of browsing a truncated teaser: the kept entry count, total items, and snapshot
/// fingerprint. All three are browse-time signals pulled from the rendered listing's meta.
struct BrowsedTeaser {
    kept_entry_count: usize,
    total_items: Option<usize>,
    snapshot_fingerprint: Option<String>,
}

/// Browse the serve's teaser via the production path (`hb_net::browse_share_code`), assert it truncated,
/// and return the browse-time signals. Retries ×3 (flake policy).
async fn browse_truncated_teaser(probe: &ProbeInput) -> Result<BrowsedTeaser, String> {
    use hb_net::{browse_share_code, browse_peer_listings, RelayClient};

    let share = hb_core::ShareCode::parse(&probe.serve_share_code)
        .map_err(|e| format!("parse serve share code: {e}"))?;
    let relays = probe
        .store
        .load_settings()
        .map_err(|e| format!("load settings: {e}"))?
        .map(|s| s.relay_urls)
        .unwrap_or_default();
    if relays.is_empty() {
        return Err("no relays configured on the probe store".to_string());
    }

    let browse_key = share.browse_key().ok_or_else(|| {
        "the serve share code has no browse-key — accept_manifest_bytes needs one".to_string()
    })?;

    let mut last_err = String::new();
    for attempt in 1..=DM_POLL_RETRIES {
        // A fresh client each attempt. browse_share_code + browse_peer_listings both need a connected
        // client; we hold one open for both legs of this attempt, then disconnect.
        let client = match RelayClient::connect(&probe.identity, &relays, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("attempt {attempt}: connect: {e}");
                tokio::time::sleep(SETTLE).await;
                continue;
            }
        };
        // THE production browse path: browse_share_code resolves the teaser. Then browse_peer_listings
        // fetches the listing families (the browse-key-encrypted teasers). We pass the serve's own
        // relays as the seed set.
        let browse = match browse_share_code(&client, &share, "", &relays, &relays, RELAY_TIMEOUT).await {
            Ok(b) => b,
            Err(e) => {
                last_err = format!("attempt {attempt}: browse_share_code: {e}");
                client.disconnect().await;
                tokio::time::sleep(SETTLE).await;
                continue;
            }
        };
        let _ = browse; // browse_share_code resolves the teaser + NIP-65; families come next.

        let families = match browse_peer_listings(&client, &share.pubkey(), &browse_key, RELAY_TIMEOUT)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                last_err = format!("attempt {attempt}: browse_peer_listings: {e}");
                client.disconnect().await;
                tokio::time::sleep(SETTLE).await;
                continue;
            }
        };
        client.disconnect().await;

        // Find the family whose slug matches our seed slug.
        let family = families
            .iter()
            .find(|(root, _, _)| root == SEED_SLUG || root.starts_with(SEED_SLUG))
            .or_else(|| families.first());
        let Some((slug, teaser_rendered, _event_id)) = family else {
            last_err = format!(
                "attempt {attempt}: no listing family found (browse returned {} families)",
                families.len()
            );
            tokio::time::sleep(SETTLE).await;
            continue;
        };

        let truncated = teaser_rendered
            .meta
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let total_items = teaser_rendered
            .meta
            .get("total_items")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let snapshot_fingerprint = teaser_rendered
            .meta
            .get("snapshot_fingerprint")
            .and_then(|v| v.as_str())
            .map(String::from);
        let kept_entry_count = teaser_rendered.entries.len();

        if !truncated {
            last_err = format!(
                "attempt {attempt}: teaser for '{slug}' is NOT truncated (kept {kept_entry_count}, \
                 total {total_items:?}) — the seed collection did not exceed the {LISTING_MAX_BYTES}-byte \
                 truncation budget. Generate more files."
            );
            tokio::time::sleep(SETTLE).await;
            continue;
        }

        return Ok(BrowsedTeaser {
            kept_entry_count,
            total_items,
            snapshot_fingerprint,
        });
    }
    Err(last_err)
}

/// Send the production request-DM (the "Ask owner" wire) to the serve peer. Returns the ask nonce the
/// probe minted (which the serve must echo into the ticket). Records the ask on the probe's store AFTER
/// the send resolves (the production ordering — `request_manifest` does the same).
async fn send_request_dm(probe: &ProbeInput, fingerprint_seen: &Option<String>) -> Result<String, String> {
    use hb_net::RelayClient;

    // Mint the ask nonce HERE (not in the UI), same as `request_manifest`: 128 bits of randomness, hex.
    // It must be the same value that reaches both the wire and the local trace.
    let nonce_bytes: [u8; 16] = rand::random();
    let ask_nonce = hex::encode(nonce_bytes);

    // Build the request-DM body via the PRODUCTION wire builder (the exact JSON the "Ask owner" button
    // produces). build_manifest_request is pub(crate) in commands::chat.
    let content = crate::commands::chat::build_manifest_request(
        SEED_SLUG,
        fingerprint_seen.as_deref().unwrap_or(""),
        None, // teaser_event_id — optional, not needed for the gate
        None, // mascara_pubkey — vestigial, always None (wire_freeze)
        Some(ask_nonce.clone()),
    )?;

    let relays = probe
        .store
        .load_settings()
        .map_err(|e| format!("load settings: {e}"))?
        .map(|s| s.relay_urls)
        .unwrap_or_default();
    if relays.is_empty() {
        return Err("no relays configured on the probe store".to_string());
    }

    let recipient = parse_recipient(&probe.serve_share_code)
        .map_err(|e| format!("parse serve share code for DM recipient: {e}"))?;

    // send_dm_inner is the production NIP-17 gift-wrap send path (pub(crate) in commands::chat). It
    // resolves the recipient's NIP-65 read relays, targets the publish, and returns the wrap.
    let client = RelayClient::connect(&probe.identity, &relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("connect for request-DM: {e}"))?;
    send_dm_inner(
        &client,
        &probe.identity,
        &recipient,
        &content,
        &relays,
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("send_dm_inner (request-DM): {e}"))?;
    client.disconnect().await;

    // Record the ask AFTER the send resolves (production ordering: a failed publish leaves no trace).
    let sent_at = chrono::Utc::now().to_rfc3339();
    probe
        .store
        .record_manifest_ask(
            &probe.serve_npub,
            SEED_SLUG,
            fingerprint_seen.as_deref().unwrap_or(""),
            &sent_at,
            &ask_nonce,
        )
        .map_err(|e| format!("record_manifest_ask: {e}"))?;

    Ok(ask_nonce)
}

/// Poll the probe's DM inbox for a ticket-DM from the serve. The serve's auto-approve loop answers the
/// request-DM with a ticket (serialized as JSON) wrapped in a NIP-17 gift-wrap DM. This leg unwraps
/// incoming DMs (via `decode_dms`, the production NIP-17 unwrap path) and looks for one whose content
/// parses as a `TransportTicket` carrying our ask nonce.
///
/// Retries ×`DM_POLL_RETRIES` with a settle between attempts (DM propagation over a live relay is the
/// slow leg). Every attempt's evidence is dumped to stderr.
async fn poll_for_ticket_dm(probe: &ProbeInput, expected_nonce: &str) -> Result<TransportTicket, String> {
    use crate::commands::chat::decode_dms;
    use hb_net::RelayClient;
    use std::collections::HashSet;

    let relays = probe
        .store
        .load_settings()
        .map_err(|e| format!("load settings: {e}"))?
        .map(|s| s.relay_urls)
        .unwrap_or_default();

    let mut last_err = String::new();
    for attempt in 1..=DM_POLL_RETRIES {
        let client = match RelayClient::connect(&probe.identity, &relays, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("attempt {attempt}: connect: {e}");
                tokio::time::sleep(SETTLE).await;
                continue;
            }
        };
        // Fetch gift-wrap events addressed to us (kind 1059, pubkey = ours). No since-bound: a cold
        // cache should pull everything (the serve's ticket-DM may have landed before this poll started).
        let wraps = match client
            .fetch(
                Filter::new().kind(Kind::GiftWrap).pubkey(probe.identity.public_key()),
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

        eprintln!(
            "   E1/E2 ticket-DM poll attempt {attempt}: {} gift-wrap(s) from relays",
            wraps.len()
        );

        // decode_dms unwraps each gift-wrap, recovers the real sender, and returns sender-attributed
        // messages. Filter to the serve's npub.
        let serve_npub = probe.serve_npub.clone();
        let allow: HashSet<String> = [serve_npub.clone()].into_iter().collect();
        let msgs = decode_dms(&probe.probe_npub, &probe.identity, wraps, Some(&allow)).await;
        eprintln!(
            "   E1/E2 ticket-DM poll attempt {attempt}: {} decoded DM(s) from serve",
            msgs.len()
        );

        for msg in &msgs {
            // Try to parse the DM content as a transport ticket. The serve's auto-approve sends the
            // ticket JSON as the DM body (same as send_full_list).
            let trimmed = msg.content.trim();
            if !trimmed.starts_with('{') {
                continue;
            }
            let ticket: TransportTicket = match serde_json::from_str(trimmed) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ticket.verify_shape().is_err() {
                continue;
            }
            // The ticket must carry OUR ask nonce (the serve echoed it from the request-DM).
            if ticket.ask_nonce.as_deref() != Some(expected_nonce) {
                eprintln!(
                    "   E1/E2 ticket-DM: found a ticket but nonce mismatch (got {:?}, expected {expected_nonce}) — stale DM from a prior ask?",
                    ticket.ask_nonce
                );
                continue;
            }
            return Ok(ticket);
        }

        last_err = format!(
            "attempt {attempt}: no ticket-DM from serve carrying nonce {expected_nonce} ({} DMs decoded)",
            msgs.len()
        );
        tokio::time::sleep(SETTLE).await;
    }
    Err(last_err)
}

/// Redeem a ticket through the full production path: claim the ask → fetch_manifest (with
/// accept_manifest_bytes running INSIDE, same as WAN-M) → spend the ask. Passes `newest_fingerprint`
/// through to the gate (E1 passes the matching fingerprint; E2 passes a stale one to exercise the gate).
async fn redeem_ticket(
    probe: &ProbeInput,
    ticket: &TransportTicket,
    newest_fingerprint: Option<&str>,
) -> Result<ImportedManifest, String> {
    // Claim the ask before the dial (production gate, before any dial). The probe recorded the ask in
    // send_request_dm; the nonce matches.
    claim_for_probe(probe, ticket).await?;

    let endpoint = probe_client_endpoint(probe).await?;
    let store = probe.store.clone();
    let serve_npub = probe.serve_npub.clone();
    let expected_slug = ticket.slug.clone();
    // Capture the nonce before the ticket is moved into the retry loop's async closure.
    let ticket_nonce = ticket.ask_nonce.clone();

    let mut last_err = String::new();
    for attempt in 1..=LIVE_REDEEM_RETRIES {
        let store = store.clone();
        let serve_npub = serve_npub.clone();
        let expected_slug = expected_slug.clone();
        let endpoint = endpoint.clone();
        let ticket = ticket.clone();
        let result = tokio::time::timeout(LIVE_REDEEM_TIMEOUT, async move {
            // THE production redeem path: fetch_manifest runs the gate INSIDE (before the ACK). The
            // closure is accept_manifest_bytes — the real M16 W4 gate, not a transport-flavoured copy.
            let mut imported: Option<ImportedManifest> = None;
            fetch_manifest(&endpoint, &ticket, |payload| {
                let raw = std::str::from_utf8(payload.as_bytes())
                    .map_err(|_| anyhow!("the manifest that arrived was not text"))?;
                imported = Some(
                    accept_manifest_bytes(
                        &serve_npub,
                        Some(&expected_slug),
                        raw,
                        newest_fingerprint,
                        &store,
                        // The cache IS the delivery on this path. A dropped write fails the gate, so no
                        // ACK is sent and the ticket survives.
                        true,
                    )
                    .map_err(|e| anyhow!("{e}"))?,
                );
                Ok(())
            })
            .await?;
            let imported =
                imported.ok_or_else(|| anyhow!("the manifest was acknowledged but never accepted"))?;
            Ok::<_, anyhow::Error>(imported)
        })
        .await;

        match result {
            Ok(Ok(imported)) => {
                eprintln!("   E1/E2 redeem succeeded on attempt {attempt}");
                // Spend the ask now that the redemption landed (production ordering). Uses the
                // pre-captured slug + nonce (the ticket is moved into the async closure above).
                let _ = probe.store.spend_manifest_ask(
                    &probe.serve_npub,
                    SEED_SLUG,
                    ticket_nonce.as_deref().unwrap_or_default(),
                );
                return Ok(imported);
            }
            Ok(Err(e)) => {
                last_err = format!("attempt {attempt}: fetch_manifest failed: {e}");
                eprintln!("   E1/E2 attempt {attempt} failed: {e}");
            }
            Err(_) => {
                last_err =
                    format!("attempt {attempt}: redeem did not complete within {LIVE_REDEEM_TIMEOUT:?}");
                eprintln!("   E1/E2 attempt {attempt} timed out");
            }
        }
        if attempt < LIVE_REDEEM_RETRIES {
            tokio::time::sleep(SETTLE).await;
        }
    }
    Err(last_err)
}

// ---------------------------------------------------------------------------
// Helpers — probe endpoint, claim (mirrors WAN-M, kept local for independence)
// ---------------------------------------------------------------------------

/// Bind the probe's dial-only endpoint via the production path (`ensure_endpoint` with `DialOnly`).
/// Mirrors WAN-M's `probe_client_endpoint`.
async fn probe_client_endpoint(probe: &ProbeInput) -> Result<iroh::Endpoint, String> {
    let app_id = AppIdentity {
        identity: probe.identity.clone(),
        browse_key: probe.browse_key.clone(),
        transport_key: probe.transport_key.clone(),
    };
    let live_npub: SharedIdentity = Arc::new(tokio::sync::RwLock::new(Some(app_id)));
    let source: Arc<dyn ManifestSource> = crate::manifest_source::StoreManifestSource::new(
        probe.store.clone(),
        probe.identity.clone(),
        probe.browse_key.clone(),
    );
    let shared = new_shared_endpoint();
    ensure_endpoint(
        &shared,
        &probe.probe_npub,
        &live_npub,
        &probe.transport_key,
        source,
        Role::DialOnly,
    )
    .await
    .map_err(|e| format!("bind probe endpoint: {e}"))
}

/// Claim the ask for `ticket` on the probe's store (the production gate, before any dial). Mirrors
/// WAN-M's `claim_for_probe`.
async fn claim_for_probe(probe: &ProbeInput, ticket: &TransportTicket) -> Result<(), String> {
    use crate::store::AskClaim;
    let claim = probe
        .store
        .claim_manifest_ask(
            &probe.serve_npub,
            &ticket.slug,
            ticket.ask_nonce.as_deref().unwrap_or_default(),
            &ticket.request_id,
        )
        .map_err(|e| format!("claim_manifest_ask: {e}"))?;
    match claim {
        AskClaim::Granted => Ok(()),
        AskClaim::Spent => Err(
            "the ask is already spent — the probe already redeemed this (harness ordering error)".to_string(),
        ),
        AskClaim::Unsolicited => Err(
            "the ask is unsolicited — the probe never recorded an ask for this (npub, slug)".to_string(),
        ),
        AskClaim::ClaimedByAnother => Err(
            "another request id claimed this ask — the serve minted a ticket with a different request id"
                .to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Build the ProbeInput from the parsed args
// ---------------------------------------------------------------------------

/// Build the `ProbeInput` from the parsed args + probe data dir. Saves serve as a contact (so the
/// acceptance gate can decrypt with the browse-key). The probe's identity + store are seeded from
/// `--data-dir`.
pub async fn build_probe_input(
    app_id: AppIdentity,
    store: DataStore,
    serve_share_code: String,
) -> Result<ProbeInput> {
    // Parse the serve's full share code — we need the browse-key for accept_manifest_bytes and
    // browse_share_code.
    let share = hb_core::ShareCode::parse(&serve_share_code)
        .map_err(|e| anyhow!("invalid serve share code (need the serve's full hbk share-code): {e}"))?;
    let serve_npub = share
        .pubkey()
        .to_bech32()
        .map_err(|e| anyhow!("encode serve npub: {e}"))?;

    // Save serve as a contact with the browse-key — accept_manifest_bytes + browse_share_code read this.
    let contact = crate::store::CachedPeer {
        npub: serve_npub.clone(),
        source: crate::store::ContactSource::Manual,
        browse_key_hex: share.browse_key().map(hex::encode),
        petname: Some("wan-e2e-serve".to_string()),
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
        .save_contact(&crate::store::CachedPeer::pubkey_hash(&serve_npub), &contact)
        .map_err(|e| anyhow!("save serve as contact: {e}"))?;

    Ok(ProbeInput {
        identity: app_id.identity.clone(),
        browse_key: app_id.browse_key.clone(),
        transport_key: app_id.transport_key.clone(),
        probe_npub: app_id.npub(),
        store,
        serve_share_code,
        serve_npub,
    })
}

// ---------------------------------------------------------------------------
// Unit tests — pure parts (no network, no iroh endpoint)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The truncation threshold is a BYTE budget (40 KB), not an entry count. A listing whose JSON
    /// exceeds it truncates; one under it does not. This is the property E1's seed collection relies on:
    /// generate enough small files that the serialized listing exceeds 40 KB.
    #[test]
    fn truncation_threshold_is_a_byte_budget_not_an_entry_count() {
        // A listing just under the budget does NOT truncate.
        let small =
            r#"{"slug":"x","path_alias":"x","item_count":1,"content_types":["other"],"last_updated":"t","entries":[{"name":"a","item_type":"File","tags":[],"children":[]}]}"#;
        let t = hb_net::truncate_listing(small, LISTING_MAX_BYTES).unwrap();
        assert!(!t.truncated, "a listing under the byte budget does not truncate");

        // A listing OVER the budget DOES truncate. Generate ~600 entries to exceed 40 KB.
        let entries: Vec<serde_json::Value> = (0..600)
            .map(|i| {
                serde_json::json!({"name": format!("file-{i:04}.bin"), "item_type": "File", "tags": [], "children": []})
            })
            .collect();
        let big = serde_json::json!({
            "slug": "wan-e2e",
            "path_alias": "WAN-E2E Seed",
            "item_count": entries.len(),
            "content_types": ["other"],
            "last_updated": "2026-08-01T00:00:00Z",
            "entries": entries,
        })
        .to_string();
        assert!(
            big.len() > LISTING_MAX_BYTES,
            "the seed listing ({} bytes) must exceed the {} byte budget to truncate",
            big.len(),
            LISTING_MAX_BYTES
        );
        let t = hb_net::truncate_listing(&big, LISTING_MAX_BYTES).unwrap();
        assert!(t.truncated, "a listing over the byte budget truncates");
        assert!(t.total_items > t.shown_items, "truncation keeps fewer items than the total");
        assert!(t.shown_items > 0, "truncation keeps at least some items (breadth-first)");
    }

    /// The staleness gate is a fingerprint COMPARISON (symmetric mismatch), not a direction-aware
    /// check. `matches_fingerprint` returns true iff the fingerprints are equal; the gate inverts it.
    /// So `stale == true` for ANY mismatch, regardless of which side is "newer".
    #[test]
    fn staleness_gate_is_a_symmetric_fingerprint_comparison() {
        let id = hb_core::Identity::generate();
        let bk: [u8; 32] = [9u8; 32];
        let fp_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let fp_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        // A manifest carrying fp_a, checked against fp_a → not stale (match).
        let plaintext = format!(
            r#"{{"slug":"s","path_alias":"s","item_count":1,"content_types":["other"],"last_updated":"t","snapshot_fingerprint":"{fp_a}","entries":[{{"name":"a","item_type":"File","tags":[],"children":[]}}]}}"#
        );
        let env = hb_core::manifest::build_manifest_envelope(&id, "s", &bk, fp_a, 1, &[plaintext]).unwrap();
        assert!(env.matches_fingerprint(fp_a), "matching fingerprint → match");
        assert!(!env.matches_fingerprint(fp_b), "mismatched fingerprint → no match");

        // The production gate: stale = !matches_fingerprint(newest_fp). So:
        // - manifest(fp_a) checked against newest=fp_a → stale=false (E1's case)
        // - manifest(fp_a) checked against a synthetic stale fp (all zeros) → stale=true (E2's case:
        //   the probe passes a guaranteed-different fingerprint, the gate fires on ANY mismatch)
        let stale_e1 = !env.matches_fingerprint(fp_a);
        let synthetic = "0000000000000000000000000000000000000000000000000000000000000000";
        let stale_e2 = !env.matches_fingerprint(synthetic);
        assert!(!stale_e1, "E1: matching fingerprint → not stale");
        assert!(stale_e2, "E2: synthetic stale fingerprint → stale (the gate fires on ANY mismatch)");
    }

    /// `build_probe_input` parses a full share code into a contact with the browse-key.
    #[tokio::test]
    async fn build_probe_input_saves_serve_contact_with_browse_key() {
        let serve = AppIdentity::generate();
        let serve_share = serve.share_code().unwrap();
        let serve_npub = serve.npub();
        let serve_browse_key = *serve.browse_key.bytes();

        let probe_id = AppIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path()).unwrap();

        let input = build_probe_input(probe_id, store.clone(), serve_share).await.unwrap();

        assert_eq!(input.serve_npub, serve_npub);
        let contact = store
            .load_contact(&crate::store::CachedPeer::pubkey_hash(&serve_npub))
            .unwrap()
            .unwrap();
        assert_eq!(contact.browse_key_hex, Some(hex::encode(serve_browse_key)));
    }
}
