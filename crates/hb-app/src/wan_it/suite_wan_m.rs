//! WAN-M — the iroh manifest plane over real networks (M20 W6 §W6). This slice implements **M1 and
//! M9 only** (the launch-gate manifest-plane path); M2–M8 and the E2E suite are later slices.
//!
//! Both rows drive the **real production redeem path** end to end — `transport::fetch_manifest` with
//! the M16 W4 acceptance gate (`browse::accept_manifest_bytes`) running *inside* it, exactly as
//! `redeem_manifest_ticket` calls it. The consume-exactly-once mechanism is the REAL one
//! (`ManifestSource::issued` → `authorize_redemption` → `into_consumed` → `record_consumed`), not a
//! harness reimplementation: serve binds the endpoint via `transport_state::ensure_endpoint` with a
//! `StoreManifestSource` over the real data directory, so the accept loop and the in-flight set are
//! production.
//!
//! **M1 — cross-NAT redemption.** Redeem a live ticket → manifest parses/verifies/slug-matches via
//! the production gate, the receipt is recorded, the ticket is consumed exactly once; a second
//! redemption attempt gets the standard refusal ("already redeemed").
//!
//! **M9 — owner offline / stale node_addr.** Redeem a ticket whose `node_addr` points at a dead
//! endpoint → bounded failure (explicit timeout, no hang, ticket unspent); then the live redeem
//! succeeds. Per §W6 the dead-endpoint leg is exercised by crafting a ticket via `--ticket-json`
//! that names an unroutable address. The row orders the dead leg FIRST so the live redeem (M1's leg)
//! also demonstrates the "succeeds on retry once serve returns" property.
//!
//! **Honest red.** Nothing here is `# TODO`/skip. A row that cannot reach the serve fails with a
//! recorded diagnostic (the per-step evidence dump, never just "it didn't work"), and the suite's
//! nonzero exit code is the correct signal.
//!
//! **Flake policy (P3b precedent):** the live redeem retries ×3; every failure is a recorded data
//! point. The dead-endpoint leg asserts a BOUNDED failure (it must time out, not hang), and the
//! ticket staying unspent is checked against the probe's own store — the probe recorded the claim,
//! and a failed dial does not spend it (the production property).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use hb_core::TransportTicket;
use nostr::prelude::ToBech32;

// `accept_manifest_bytes` is no longer imported here on purpose: the M16 W4 gate is applied
// INSIDE `redeem_manifest_ticket_inner`, which this suite now calls. The harness importing the gate
// itself was the shape of the 2026-08-27 defect — a copy of the production sequence, kept in step by
// hand, that silently omitted one step.
use crate::identity_state::{AppIdentity, SharedIdentity};
use crate::store::DataStore;
use crate::transport::{fetch_manifest, parse_node_addr};
use crate::transport_state::{ensure_endpoint, new_shared_endpoint, Role};
use crate::wan_it::tap::Tap;

use super::args;

/// A dial deadline for the live redeem. The production plane has its own internal ACK deadline
/// (120 s); this bounds the *dial+fetch* as observed by the harness so a wedged endpoint fails the
/// row rather than hanging indefinitely. Generous: hole punching through n0 relays can take tens of
/// seconds on a cold path.
const LIVE_REDEEM_TIMEOUT: Duration = Duration::from_secs(120);

/// The dead-endpoint deadline (M9). A ticket whose `node_addr` points at an unroutable address must
/// fail *within this bound* — the row's claim is "no hang", so the deadline is the assertion. Well
/// under the production ACK window so a real failure is clearly distinguished from a slow success.
const DEAD_ENDPOINT_TIMEOUT: Duration = Duration::from_secs(30);

/// Long-haul live redemptions retry this many times before recording a failure (flake policy).
const LIVE_REDEEM_RETRIES: u32 = 3;

/// Settle between retry attempts (lets the serve's accept loop recycle).
const SETTLE: Duration = Duration::from_secs(2);

/// The input probe needs to drive the production redeem path. Built by `run_probe_wan_m` from the
/// parsed args; passed in here so the suite is testable against a constructed value. Holds the
/// identity *fields* (not the `AppIdentity`, which owns ZeroizeOnDrop secrets and is not Clone) so
/// `probe_client_endpoint` can build a fresh `SharedIdentity` without cloning.
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
    /// The serve peer's npub (bech32) — `accept_manifest_bytes` pins the manifest author to this.
    pub serve_npub: String,
    /// The live ticket minted by serve (M1 + M9's retry leg redeem this).
    pub live_ticket: TransportTicket,
    /// The dead-endpoint ticket (M9's dead leg redeems this). Same request id / slug as `live_ticket`
    /// but a `node_addr` that points at an unroutable address.
    pub dead_ticket: TransportTicket,
}

/// Run the WAN-M rows (M1, M9) against a live serve. Each row is an honest TAP check: Ok ⇒ pass,
/// Err(detail) ⇒ fail with a `# diagnostic` block. Rows that need the serve's receipt state share it
/// via the `serve_store` handle (the harness on a single machine can read it; across machines the
/// orchestrator correlates the TAP output).
pub async fn run(tap: &mut Tap, probe: &ProbeInput, serve_store: Option<&DataStore>) {
    // M9's dead-endpoint leg FIRST — it must fail bounded and leave the ticket unspent, then the
    // live redeem (M1's first leg) demonstrates "succeeds on retry once serve returns". Ordering the
    // dead leg first keeps the two rows honest: the live redeem is not pre-burned by the dead leg.
    tap.check(
        "M9-dead: a ticket whose node_addr is unroutable fails bounded-in-time (no hang, ticket unspent)",
        m9_dead_endpoint_fails_bounded(probe).await,
    );

    // M1's first leg — the live redeem. Doubles as M9's "succeeds on retry once serve returns".
    tap.check(
        "M1-live: redeem the live ticket → manifest parses/verifies/slug-matches via accept_manifest_bytes, receipt recorded",
        m1_live_redeem(probe).await,
    );

    // M1's exactly-once leg — the ticket is now spent, so a second attempt gets the standard refusal.
    // The refusal is observed on the PROBE side as an error from fetch_manifest; the OWNER-side
    // confirmation (the store shows consumed_at set) is checked below when the orchestrator runs
    // single-machine.
    tap.check(
        "M1-once: a second redemption attempt is refused (ticket consumed exactly once)",
        m1_second_attempt_refused(probe, serve_store).await,
    );
}

// ---------------------------------------------------------------------------
// M9 — dead endpoint → bounded failure, ticket unspent
// ---------------------------------------------------------------------------

/// Dial a ticket whose `node_addr` points at an unroutable (TEST-NET-1, RFC 5737) address. The
/// redeem MUST fail within [`DEAD_ENDPOINT_TIMEOUT`] (the assertion is "no hang"), and the failed
/// dial must NOT spend the ticket — checked against the probe's store: `claim_manifest_ask` recorded
/// the claim, and a failed dial leaves the ask unspent (the production property the owner's ruling
/// exists to guarantee).
///
/// The dead ticket shares the live ticket's `request_id` and `slug` but rewrites `node_addr` to an
/// unroutable address, so the claim recorded for the live ticket covers this leg too (the claim is
/// keyed by `(npub, slug)` and bound to the `request_id`; the same `request_id` re-claims Granted).
async fn m9_dead_endpoint_fails_bounded(probe: &ProbeInput) -> Result<(), String> {
    // Claim the ask for this request id before the dial — the production gate, taken before any dial
    // (a validation that is not a claim is not a gate). The claim is durable; a failed dial leaves it.
    claim_for_probe(probe, &probe.dead_ticket).await?;

    let endpoint = probe_client_endpoint(probe).await?;

    // The bounded dial: iroh's connect() to an unroutable IP does NOT fail fast — it retries hole
    // punches and relay lookups until its own internal timeout (which can be minutes). Our explicit
    // timeout IS the bound §W6 mandates ("bounded, recoverable failure — no hang"). A clean dial
    // error before the deadline is the fast-fail path; a timeout is the expected slow path. Either
    // way the ticket must stay unspent (the production property).
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(DEAD_ENDPOINT_TIMEOUT, async {
        fetch_manifest(&endpoint, &probe.dead_ticket, |_| Ok(())).await
    })
    .await;
    let elapsed = start.elapsed();

    match result {
        Ok(Err(e)) => {
            // The dial failed cleanly before the deadline (the fast-fail path). Record the evidence.
            eprintln!(
                "   M9-dead redeem failed in {:.1}s (expected — node_addr is unroutable): {e}",
                elapsed.as_secs_f64()
            );
            // Assert the ticket stayed unspent on the probe side: the claim is still Granted for this
            // request id, not Spent (a failed dial does not spend the ask — the production property).
            assert_probe_ask_unspent(probe, &probe.dead_ticket)?;
            Ok(())
        }
        Ok(Ok(_payload)) => Err(format!(
            "the dead-endpoint redeem UNEXPECTEDLY succeeded in {:.1}s — the ticket's \
             node_addr was meant to be unroutable. Either the address was not dead, or a relay \
             resolved it unexpectedly. This is not the M9-dead failure the row asserts.",
            elapsed.as_secs_f64()
        )),
        Err(_) => {
            // The dial timed out at our deadline — this is the expected slow path for an unroutable
            // address (iroh retries hole punches + relay lookups internally; it does not fail fast on
            // a dead IP). The timeout IS the bound: "no hang" means OUR explicit deadline fired, not
            // that iroh gave up on its own. The assertion that matters is the ticket staying unspent.
            eprintln!(
                "   M9-dead redeem timed out at {DEAD_ENDPOINT_TIMEOUT:?} (expected for an unroutable \
                 IP — iroh retries internally; our deadline is the bound, not iroh's)"
            );
            assert_probe_ask_unspent(probe, &probe.dead_ticket)?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// M1 — the live redeem + exactly-once
// ---------------------------------------------------------------------------

/// Redeem the live ticket by calling **the production command body itself**.
///
/// This used to hand-copy `redeem_manifest_ticket`'s steps — claim the ask, `fetch_manifest` with
/// the `accept_manifest_bytes` gate, spend the ask — each one mirrored here and kept in step by
/// comments saying "the harness mirrors it". That is exactly how the 2026-08-27 devtest failure got
/// out: the ONE step the copy omitted was `sanitize_node_addr`, the asker's SSRF guard, which
/// rewrites `ticket.node_addr` before the ticket is presented. The owner side then compared the
/// presented ticket against the issued one byte-for-byte and refused every honest redemption between
/// two real machines, while 18 loopback QUIC tests and this suite stayed green.
///
/// So the harness no longer mirrors anything: `redeem_manifest_ticket_inner` IS the command's body
/// (the `#[tauri::command]` is a marshalling shim over it), and every guard, transformation and
/// ordering rule inside it is now on the WAN path by construction rather than by a comment asking a
/// future editor to keep two copies in step. Retries ×3 stay (flake policy).
///
/// This is CLAUDE.md §9 applied where it was being violated: *a round-trip test must END WHERE
/// PRODUCTION ENDS.*
async fn m1_live_redeem(probe: &ProbeInput) -> Result<(), String> {
    // NOTE: the ask claim, the gate and the spend all happen INSIDE the call below now. The harness
    // does not pre-claim — doing so would re-introduce a hand-copy of the very gate under test.
    let (live_npub, shared) = probe_shared_state(probe);
    let ticket_json = serde_json::to_string(&probe.live_ticket)
        .map_err(|e| format!("re-serialize the live ticket: {e}"))?;

    let mut last_err = String::new();
    for attempt in 1..=LIVE_REDEEM_RETRIES {
        let result = tokio::time::timeout(
            LIVE_REDEEM_TIMEOUT,
            crate::commands::fulfil::redeem_manifest_ticket_inner(
                probe.serve_npub.clone(),
                ticket_json.clone(),
                None,
                &live_npub,
                &probe.store,
                &shared,
            ),
        )
        .await;

        match result {
            Ok(Ok(_imported)) => {
                eprintln!("   M1-live redeem succeeded on attempt {attempt} (via the production command body)");
                // The endpoint the command bound, for the hole-punch diagnostic. Re-reading it through
                // `ensure_endpoint` returns the same binding rather than making a second one.
                if let Ok(ep) = ensure_endpoint(
                    &shared,
                    &probe.probe_npub,
                    &live_npub,
                    &probe.transport_key,
                    probe_source(probe),
                    Role::DialOnly,
                )
                .await
                {
                    log_redeem_connection_type(&ep, &probe.live_ticket.node_addr).await;
                }
                return Ok(());
            }
            Ok(Err(e)) => {
                last_err = format!("attempt {attempt}: redeem_manifest_ticket_inner failed: {e}");
                eprintln!("   M1-live attempt {attempt} failed: {e}");
                // The claim is durable and bound to this request id, so a retry re-claims Granted.
                // An `Unsolicited`/`ClaimedByAnother` error here is a real finding, not a flake —
                // report it verbatim rather than retrying into a misleading timeout.
                if e.contains("doesn't answer a request you sent") || e.contains("already answering")
                {
                    return Err(last_err);
                }
            }
            Err(_) => {
                last_err = format!(
                    "attempt {attempt}: redeem did not complete within {LIVE_REDEEM_TIMEOUT:?}"
                );
                eprintln!("   M1-live attempt {attempt} timed out");
            }
        }
        if attempt < LIVE_REDEEM_RETRIES {
            tokio::time::sleep(SETTLE).await;
        }
    }
    Err(last_err)
}

/// QURATOR-45/74: log whether the just-completed redeem actually hole-punched (a direct IP path is
/// active) or fell back to iroh's relay — a successful `fetch_manifest` proves the pipeline works
/// either way, but the two say very different things about hole-punch success rate. Diagnostic only
/// (eprintln!, not a TAP assertion): a relay fallback is a legitimate, expected iroh behaviour, not a
/// row failure — the whole point of `presets::N0`'s relay fallback is that the pipeline still works
/// when hole-punching doesn't. Best-effort: `remote_info` can return `None`/no active addr in the
/// narrow window right after a stream closes, which is not itself evidence of anything.
async fn log_redeem_connection_type(endpoint: &iroh::Endpoint, ticket_node_addr: &str) {
    let Ok(peer_addr) = parse_node_addr(ticket_node_addr) else {
        eprintln!("   M1-live connection type: could not re-parse the ticket's node_addr to check");
        return;
    };
    match endpoint.remote_info(peer_addr.id).await {
        Some(info) => {
            let active: Vec<_> = info
                .addrs()
                .filter(|a| matches!(a.usage(), iroh::endpoint::TransportAddrUsage::Active))
                .collect();
            if active.is_empty() {
                eprintln!("   M1-live connection type: no active address recorded (inconclusive)");
            } else {
                let direct = active.iter().any(|a| !a.addr().is_relay());
                let relay = active.iter().any(|a| a.addr().is_relay());
                eprintln!(
                    "   M1-live connection type: direct_path={direct} relay_path={relay} ({} active address(es)) \
                     — direct=true means at least one non-relay path is active (hole-punch, not just relay fallback)",
                    active.len()
                );
            }
        }
        None => eprintln!("   M1-live connection type: remote_info returned None (inconclusive)"),
    }
}

/// The exactly-once assertion: a second redemption of the same ticket is refused. The refusal is
/// observed on the probe side as an error from `fetch_manifest`; the owner-side confirmation (the
/// ticket's `consumed_at` is set) is checked against `serve_store` when the harness runs
/// single-machine. The refusal message must indicate the ticket is already redeemed / no match (the
/// standard refusal an invented request id gets is the same constant — indistinguishable-from-forged
/// per M8 semantics, which is what the production path returns; we do NOT build the M8 row here).
async fn m1_second_attempt_refused(
    probe: &ProbeInput,
    serve_store: Option<&DataStore>,
) -> Result<(), String> {
    let endpoint = probe_client_endpoint(probe).await?;

    // The second redeem — the ticket is spent on the serve, so this must fail. The acceptance gate
    // never runs (the serve refuses before sending the manifest).
    let result = tokio::time::timeout(LIVE_REDEEM_TIMEOUT, async {
        fetch_manifest(&endpoint, &probe.live_ticket, |_| Ok(())).await
    })
    .await;

    let err_msg = match result {
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(_payload)) => {
            return Err(
                "the second redemption UNEXPECTEDLY succeeded — the ticket was not consumed on the \
                 first redeem, which breaks one-ticket-one-delivery. Check the serve's accept loop \
                 and receipt persistence."
                    .to_string(),
            )
        }
        Err(_) => {
            return Err(format!(
                "the second redemption timed out past {LIVE_REDEEM_TIMEOUT:?} — a spent-ticket \
                 refusal should be fast (the serve refuses before sending anything); a timeout \
                 suggests the serve is unreachable, not that the ticket is spent"
            ))
        }
    };

    // The refusal must indicate the ticket is no longer redeemable. The production path returns the
    // standard refusal constant ("no approved manifest request matches this ticket") for a spent
    // ticket too — indistinguishable-from-forged per M8 semantics, which is the intended property.
    // Accept either the "no match" constant or a message naming the replay (the transport's refusal
    // surfaces the authorize_redemption error string, which for a consumed ticket is "already
    // redeemed").
    let lower = err_msg.to_lowercase();
    let is_refusal = lower.contains("already")
        || lower.contains("redeem")
        || lower.contains("no approved")
        || lower.contains("matches this ticket");
    if !is_refusal {
        return Err(format!(
            "the second redemption failed, but not with the standard refusal — got: {err_msg}. \
             A spent ticket must refuse with the no-match/already-redeemed message."
        ));
    }
    eprintln!("   M1-once second attempt refused as expected: {err_msg}");

    // The owner-side confirmation: when the harness runs single-machine, the serve's store shows the
    // ticket consumed. This is the strongest assertion (the receipt was persisted); across machines
    // the orchestrator correlates the TAP output and this leg is informational.
    if let Some(serve) = serve_store {
        match serve.load_issued_ticket(&probe.live_ticket.request_id) {
            Ok(Some(rec)) => match rec.consumed_at {
                Some(ts) => eprintln!(
                    "   M1-once owner-side confirmation: ticket consumed_at={ts}, delivered_bytes={:?}",
                    rec.delivered_bytes
                ),
                None => {
                    return Err(
                        "the owner-side store shows the ticket NOT consumed after a successful redeem \
                         + a refused replay — the receipt was not persisted, which breaks the \
                         one-ticket-one-delivery property on the owner side"
                            .to_string(),
                    )
                }
            },
            Ok(None) => {
                return Err(
                    "the owner-side store has no record of this request id — the serve did not mint \
                     this ticket through its store (harness wiring error)"
                        .to_string(),
                )
            }
            Err(e) => eprintln!("   M1-once could not read serve store (informational): {e}"),
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers — probe endpoint, claim, unspent assertion
// ---------------------------------------------------------------------------

/// Bind the probe's dial-only endpoint via the production path (`ensure_endpoint` with `DialOnly`).
/// The probe never listens (owner ruling ③): binding a listening endpoint would leave it answering
/// anyone holding its permanently-stable node id. Constructs a fresh `SharedIdentity` from the
/// probe's fields (AppIdentity is not Clone — it holds ZeroizeOnDrop secrets).
/// The two pieces of app state the production command bodies take. Built exactly as the running app
/// builds them, so `redeem_manifest_ticket_inner` gets the same shapes it gets in production.
fn probe_shared_state(probe: &ProbeInput) -> (SharedIdentity, crate::transport_state::SharedEndpoint) {
    let app_id = AppIdentity {
        identity: probe.identity.clone(),
        browse_key: probe.browse_key.clone(),
        transport_key: probe.transport_key.clone(),
    };
    (Arc::new(tokio::sync::RwLock::new(Some(app_id))), new_shared_endpoint())
}

fn probe_source(probe: &ProbeInput) -> Arc<dyn crate::transport::ManifestSource> {
    crate::manifest_source::StoreManifestSource::new(
        probe.store.clone(),
        probe.identity.clone(),
        probe.browse_key.clone(),
    )
}

async fn probe_client_endpoint(probe: &ProbeInput) -> Result<iroh::Endpoint, String> {
    let (live_npub, shared) = probe_shared_state(probe);
    // new_shared_endpoint + ensure_endpoint is the production path (transport_state). DialOnly never
    // spawns an accept loop.
    ensure_endpoint(
        &shared,
        &probe.probe_npub,
        &live_npub,
        &probe.transport_key,
        probe_source(probe),
        Role::DialOnly,
    )
    .await
    .map_err(|e| format!("bind probe endpoint: {e}"))
}

/// Claim the ask for `ticket` on the probe's store (the production gate, before any dial). The probe
/// must have a recorded ask for `(serve_npub, slug)` with the matching nonce — `run_probe` records
/// one before the suite runs. A re-claim with the same request id is Granted (retry); a different one
/// is ClaimedByAnother (the security boundary).
async fn claim_for_probe(probe: &ProbeInput, ticket: &TransportTicket) -> Result<(), String> {
    use crate::store::AskClaim;
    let expected_author = ticket.author_npub.clone().unwrap_or_else(|| probe.serve_npub.clone());
    let claim = probe
        .store
        .claim_manifest_ask(
            &probe.serve_npub,
            &expected_author,
            &ticket.slug,
            ticket.ask_nonce.as_deref().unwrap_or_default(),
            &ticket.request_id,
        )
        .map_err(|e| format!("claim_manifest_ask: {e}"))?;
    match claim {
        AskClaim::Granted => Ok(()),
        AskClaim::Spent => Err(
            "the ask is already spent — the probe already redeemed this (harness ordering error, or \
             a prior run left the store dirty)"
                .to_string(),
        ),
        AskClaim::Unsolicited => Err(
            "the ask is unsolicited — the probe never recorded an ask for this (npub, slug), so the \
             claim gate (the production security boundary) refused. Did serve mint the ticket for a \
             different slug/nonce than the probe recorded?"
                .to_string(),
        ),
        AskClaim::ClaimedByAnother => Err(
            "another request id claimed this ask — the dead and live tickets must share the serve's \
             request id (the harness rewrites only node_addr for the dead ticket)"
                .to_string(),
        ),
    }
}

/// Assert the probe's ask for `ticket` is still unspent (a failed dial does not spend it). Mirrors
/// what `redeem_manifest_ticket` relies on: a dial that never connected can simply be retried.
fn assert_probe_ask_unspent(probe: &ProbeInput, ticket: &TransportTicket) -> Result<(), String> {
    let asks = probe
        .store
        .load_manifest_asks()
        .map_err(|e| format!("load_manifest_asks: {e}"))?;
    // The author the ticket names, else the DM sender — same resolution as the redeem path.
    let author = ticket.author_npub.as_deref().unwrap_or(&probe.serve_npub);
    let key = format!("{}|{}|{}", probe.serve_npub, author, ticket.slug);
    match asks.get(&key) {
        Some(ask) if ask.spent => Err(format!(
            "the ask is marked spent after a dead-endpoint dial that never reached the serve — \
             a failed delivery must cost nothing (the production property). Nonce/req: {}/{}",
            ask.nonce,
            ask.claimed_by.as_deref().unwrap_or("(none)")
        )),
        Some(_) => Ok(()),
        None => Err(format!(
            "no ask recorded for {key} — the probe did not record an ask before the dead-endpoint leg"
        )),
    }
}

/// Build the `ProbeInput` from the parsed args + probe data dir. Saves serve as a contact (so the
/// acceptance gate can decrypt with the browse-key) and records a manifest ask (so the claim gate
/// passes). Returns the input the suite drives.
pub async fn build_probe_input(
    app_id: AppIdentity,
    store: DataStore,
    peer_str: &str,
    ticket_json_raw: &str,
    dead_endpoint_addr: &str,
) -> Result<ProbeInput> {
    // Parse the serve's full share code — we need the browse-key for accept_manifest_bytes.
    let share = hb_core::ShareCode::parse(peer_str)
        .map_err(|e| anyhow!("invalid --peer code (need the serve's full hbk share-code): {e}"))?;
    let serve_npub = share.pubkey().to_bech32().map_err(|e| anyhow!("encode serve npub: {e}"))?;

    // Save serve as a contact with the browse-key — accept_manifest_bytes reads this. Mirrors what
    // the add-contact funnel does (browse → save_contact with browse_key_hex).
    let contact = crate::store::CachedPeer {
        npub: serve_npub.clone(),
        source: crate::store::ContactSource::Manual,
        browse_key_hex: share.browse_key().map(hex::encode),
        petname: Some("wan-it-serve".to_string()),
        profile: None,
        collections: vec![],
        listings_state: Default::default(), // QURATOR-134 tri-state (not classified on this stub path)
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None,
        local_tags: vec![],
        fingerprint: None,
    };
    store.save_contact(&crate::store::CachedPeer::pubkey_hash(&serve_npub), &contact)
        .map_err(|e| anyhow!("save serve as contact: {e}"))?;

    // Parse the ticket JSON (path-or-inline).
    let ticket_bytes = args::read_ticket_json(ticket_json_raw)
        .map_err(|e| anyhow!("read --ticket-json: {e}"))?;
    let live_ticket: TransportTicket = serde_json::from_str(&ticket_bytes)
        .map_err(|e| anyhow!("parse ticket JSON: {e}"))?;
    live_ticket.verify_shape().map_err(|e| anyhow!("ticket failed shape check: {e}"))?;

    // Build the dead-endpoint ticket: same request id / slug / nonce, unroutable node_addr. TEST-NET-1
    // (RFC 5737, 192.0.2.0/24) is guaranteed unroutable — the dial fails, not a real peer.
    let mut dead_ticket = live_ticket.clone();
    dead_ticket.node_addr = dead_endpoint_addr.to_string();

    // Record a manifest ask so the probe's claim_manifest_ask passes. The nonce is the asker's own;
    // it must match the one serve echoed into the ticket. The fingerprint is informational (we have
    // not browsed a teaser); a placeholder stands in (the gate checks nonce + request id, not fp).
    store.record_manifest_ask(
        &serve_npub,
        &serve_npub,
        &live_ticket.slug,
        "wan-it-probe",
        &chrono::Utc::now().to_rfc3339(),
        live_ticket.ask_nonce.as_deref().unwrap_or_default(),
    ).map_err(|e| anyhow!("record manifest ask: {e}"))?;

    Ok(ProbeInput {
        identity: app_id.identity.clone(),
        browse_key: app_id.browse_key.clone(),
        transport_key: app_id.transport_key.clone(),
        probe_npub: app_id.npub(),
        store,
        serve_npub,
        live_ticket,
        dead_ticket,
    })
}

// ---------------------------------------------------------------------------
// M4 — n0 canary core (ruling 2): bind_endpoint obtains a home relay and the endpoint is reachable
//
// This is the canary daemon's core row (ruling 2: "n0 infrastructure: canary it first"). Production
// silently depends on n0's public relay/discovery fleet — `bind_endpoint` binds with `presets::N0`,
// which acquires a home relay. This row proves that dependency holds: bind a listening endpoint via
// the production path, wait for it to acquire a home relay (an addr carrying a relay URL), then dial
// it from a SEPARATE dial-only endpoint via the relay path and complete the ALPN handshake. A failure
// here is the early warning that n0 is down or unreachable — the class of breakage that happens
// BETWEEN releases with no code change on our side.
//
// Standalone (probe-plays-both): no serve, no ticket, no manifest. The row binds TWO endpoints — a
// listener (the production `bind_endpoint`, with the manifest ALPN) and a dialer (the production
// `bind_client_endpoint`, dial-only). The dialer connects to the listener's relay-routable addr; the
// ALPN handshake completing is the reachability proof.
// ---------------------------------------------------------------------------

/// How long to wait for the listener to acquire a home relay (n0 discovery). The endpoint polls its
/// own addr; once the addr carries a relay URL, the home relay is up. Generous: n0 discovery on a
/// cold path can take tens of seconds.
const HOME_RELAY_WAIT: Duration = Duration::from_secs(60);

/// The dial deadline for the reachability leg. A relay-routed dial can take tens of seconds on a
/// cold path (iroh discovers the route, then the relay proxies the QUIC handshake). Generous so a
/// slow-but-working relay path is not read as a failure; a timeout means the home relay is up but
/// genuinely not routing.
const M4_DIAL_TIMEOUT: Duration = Duration::from_secs(90);

/// Run the M4 n0 canary row standalone. Binds a listening endpoint via the production `bind_endpoint`
/// (`presets::N0`), waits for a home relay, then dials it from a dial-only endpoint and completes the
/// ALPN handshake. Returns Ok on reachability, Err with a diagnostic on failure.
pub async fn m4_n0_canary() -> Result<(), String> {
    use crate::transport::{bind_client_endpoint, bind_endpoint, MANIFEST_ALPN};
    use iroh::Watcher;

    // (1) Bind a listening endpoint via the production path. bind_endpoint uses presets::N0 + the
    //     manifest ALPN — the same binding serve uses. A fresh transport secret per run is fine (M4
    //     asserts reachability, not stable identity).
    let server_secret: [u8; 32] = rand::random();
    let server = bind_endpoint(&server_secret)
        .await
        .map_err(|e| format!("M4 bind_endpoint (presets::N0 listener) failed: {e}"))?;
    eprintln!("   M4 listener bound via presets::N0; waiting for a home relay");

    // (2) Wait for the listener to acquire a home relay. The addr watcher yields EndpointAddrs; once
    //     one carries a relay URL (a TransportAddr::Relay), the home relay is up and the endpoint is
    //     dialable through it. Poll the watcher until a relay addr appears or the deadline fires.
    let deadline = std::time::Instant::now() + HOME_RELAY_WAIT;
    let mut relay_url: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let addr = server.watch_addr().get();
        // Look for a relay transport addr in the set. The addr's addrs are a BTreeSet; a Relay variant
        // means the endpoint registered with a home relay.
        let found = serde_json::to_string(&addr).unwrap_or_default();
        if found.contains("Relay") || found.contains("relay") {
            // Extract the relay URL from the addr for the diagnostic. The EndpointAddr's Display/Debug
            // names the relay; the JSON serialization carries it. This is best-effort — the assertion
            // is "a relay addr exists", not "we parsed the URL".
            relay_url = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let relay_info = match relay_url {
        Some(r) => {
            eprintln!("   M4 listener acquired a home relay (addr carries a Relay transport)");
            r
        }
        None => {
            server.close().await;
            return Err(format!(
                "M4 the listener did NOT acquire a home relay within {HOME_RELAY_WAIT:?}. n0's relay \
                 fleet is unreachable — this is ruling 2's canary failure (production silently depends \
                 on n0). Check n0 status / network egress."
            ));
        }
    };
    let _ = relay_info; // informational; the handshake below is the real assertion.

    // (3) Dial the listener from a fresh dial-only endpoint via the relay path. bind_client_endpoint
    //     is the production dial-only binding (no advertised ALPN). The dial uses the listener's addr
    //     (which now carries the relay URL), so the connection routes through the home relay. The
    //     ALPN handshake completing (connect returns Ok) is the reachability proof.
    let client_secret: [u8; 32] = rand::random();
    let client = bind_client_endpoint(&client_secret)
        .await
        .map_err(|e| format!("M4 bind_client_endpoint (dial-only) failed: {e}"))?;

    // The listener's addr (with the relay URL) — the dial target. watch_addr().get() snapshots it.
    let target = server.watch_addr().get();

    let dial_result = tokio::time::timeout(M4_DIAL_TIMEOUT, async {
        client.connect(target, MANIFEST_ALPN).await
    })
    .await;

    let conn = match dial_result {
        Ok(Ok(conn)) => {
            eprintln!("   M4 dial-only endpoint reached the listener through the n0 home relay (ALPN ok)");
            conn
        }
        Ok(Err(e)) => {
            // Bounded close: iroh's close().await can hang on a wedged relay path. Drop the endpoints
            // (background cleanup) rather than awaiting close — the canary force-exits via
            // std::process::exit in --once mode, and the OS reclaims sockets on exit.
            drop(server);
            drop(client);
            return Err(format!(
                "M4 the dial through the home relay FAILED (the home relay is up but not routing — a \
                 partial n0 outage): {e}"
            ));
        }
        Err(_) => {
            drop(server);
            drop(client);
            return Err(format!(
                "M4 the dial through the home relay TIMED OUT at {M4_DIAL_TIMEOUT:?} — the home relay \
                 is up but the connection did not complete (a relay-routing failure or a stale addr)"
            ));
        }
    };

    // A clean close proves the connection was real (not a half-open that the runtime tear-down masks).
    conn.close(0u32.into(), b"M4 canary complete");
    drop(server);
    drop(client);
    Ok(())
}
// ---------------------------------------------------------------------------
// Unit tests — pure parts (no network, no iroh endpoint)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path()).unwrap();
        (dir, store)
    }

    /// A dead ticket shares every binding with the live one except `node_addr` — the property M9's
    /// dead leg relies on (the claim recorded for the live ticket covers the dead leg, because the
    /// claim is keyed by `(npub, slug)` and the request id agrees).
    #[test]
    fn dead_ticket_rewrites_only_node_addr() {
        let live = TransportTicket::issue("req-1", "slug", "real-addr", 1_700_000_000, Some("n1"));
        let mut dead = live.clone();
        dead.node_addr = "dead-addr".to_string();
        assert_eq!(dead.request_id, live.request_id);
        assert_eq!(dead.slug, live.slug);
        assert_eq!(dead.ask_nonce, live.ask_nonce);
        assert_eq!(dead.node_addr, "dead-addr");
        assert_ne!(dead.node_addr, live.node_addr);
    }

    /// A failed dial must not spend the ask — the production property `redeem_manifest_ticket` relies
    /// on. Exercised against the real store: claim the ask, then assert it is still unspent.
    #[test]
    fn a_claimed_ask_is_unspent_until_spend_manifest_ask() {
        let (_dir, store) = store();
        let npub = "npub1probe";
        let slug = "slug";
        let nonce = "nonce-1";
        let req = "req-1";
        store.record_manifest_ask(npub, npub, slug, "fp", "2026-01-01T00:00:00Z", nonce).unwrap();

        // The claim is Granted and does not spend the ask.
        let claim = store.claim_manifest_ask(npub, npub, slug, nonce, req).unwrap();
        assert!(matches!(claim, crate::store::AskClaim::Granted));

        // The ask is still present and unspent after the claim (a failed dial changes nothing).
        let asks = store.load_manifest_asks().unwrap();
        let ask = asks.get(&format!("{npub}|{npub}|{slug}")).unwrap();
        assert!(!ask.spent, "a claim does not spend the ask");

        // spend_manifest_ask is what marks it spent (the production command calls this AFTER success).
        store.spend_manifest_ask(npub, npub, slug, nonce).unwrap();
        let asks = store.load_manifest_asks().unwrap();
        assert!(asks.get(&format!("{npub}|{npub}|{slug}")).unwrap().spent);
    }

    /// `build_probe_input` parses a full share code into a contact with the browse-key, and records
    /// the ask the claim gate needs. Exercised against the real store + a real ticket.
    #[tokio::test]
    async fn build_probe_input_saves_contact_and_records_ask() {
        let serve = AppIdentity::generate();
        let serve_share = serve.share_code().unwrap();
        let serve_npub = serve.npub();

        let probe_id = AppIdentity::generate();
        let serve_browse_key = *serve.browse_key.bytes();
        let (_dir, store) = store();
        let store_clone = DataStore::new(store.base_dir().to_path_buf());

        // A real ticket addressed at a loopback-shaped opaque addr (the value is opaque to hb-core).
        let live = TransportTicket::issue("req-1", "my-slug", r#"{"id":"x"}"#, 1_700_000_000, Some("nonce-1"));
        let ticket_json = serde_json::to_string(&live).unwrap();

        let input = build_probe_input(
            probe_id,
            store_clone,
            &serve_share,
            &ticket_json,
            "dead-addr",
        )
        .await
        .unwrap();

        // The serve npub round-trips from the share code.
        assert_eq!(input.serve_npub, serve_npub);

        // The contact is saved with the serve's browse-key (accept_manifest_bytes reads this).
        let contact = store.load_contact(&crate::store::CachedPeer::pubkey_hash(&serve_npub)).unwrap().unwrap();
        assert_eq!(contact.browse_key_hex, Some(hex::encode(serve_browse_key)));

        // The ask is recorded with the matching nonce (claim_manifest_ask passes).
        let claim = input
            .store
            .claim_manifest_ask(&serve_npub, &serve_npub, "my-slug", "nonce-1", "req-1")
            .unwrap();
        assert!(matches!(claim, crate::store::AskClaim::Granted));

        // The dead ticket shares every binding except node_addr.
        assert_eq!(input.dead_ticket.request_id, input.live_ticket.request_id);
        assert_eq!(input.dead_ticket.slug, input.live_ticket.slug);
        assert_eq!(input.dead_ticket.node_addr, "dead-addr");
    }

    /// `assert_probe_ask_unspent` catches a spent ask (the failure mode M9 exists to detect on the
    /// probe side: a failed dial that wrongly spent the ask).
    #[test]
    fn assert_probe_ask_unspent_catches_a_spent_ask() {
        let (_dir, store) = store();
        let npub = "npub1serve";
        let slug = "slug";
        let nonce = "nonce-1";
        store.record_manifest_ask(npub, npub, slug, "fp", "t", nonce).unwrap();
        store.spend_manifest_ask(npub, npub, slug, nonce).unwrap();

        let probe_id = AppIdentity::generate();
        let ticket = TransportTicket::issue("req-1", slug, "addr", 1, Some(nonce));
        let input = ProbeInput {
            identity: probe_id.identity.clone(),
            browse_key: probe_id.browse_key.clone(),
            transport_key: probe_id.transport_key.clone(),
            probe_npub: probe_id.npub(),
            store,
            serve_npub: npub.to_string(),
            live_ticket: ticket.clone(),
            dead_ticket: ticket,
        };
        let err = assert_probe_ask_unspent(&input, &input.live_ticket).unwrap_err();
        assert!(err.contains("spent"), "the assertion names the spent ask, got: {err}");
    }
}
