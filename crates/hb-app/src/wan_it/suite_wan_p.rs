//! WAN-P — presence between real clients (M20 W6 §W6, build FIRST). Five rows drive the **real
//! production presence path** against a live `hb-wan-it serve` peer: the per-contact pill's
//! `refresh_count` → `hb_net::fetch_presence_for_authors` author-filtered read → `fresh` map (P1),
//! the per-relay accept/reject audit (P2), the cap-displacement discriminator against VPS strfry
//! (P3), the publish-retry regression (P4), and the same-NAT author-filtered resolution body folded
//! in from `hb-it/same_nat.rs` (P5).
//!
//! **Honest red, no skips.** P1 and P4 were `not ok` pre-W1 (the global-aggregate scavenge with no
//! `.authors()`; the absent retry in `run_presence_loop`). W1 (2026-08-02) fixed both — P1 now drives
//! the author-filtered read (`fetch_presence_for_authors`), P4 asserts the shipped `next_delay`
//! retry selector — so the suite is green post-W1. The philosophy is unchanged: every row exercises
//! real shipped code with diagnostics on failure, never `# TODO`/skip.
//!
//! **Flake policy (P3b precedent):** long-haul rows retry ×3; every failure is a recorded data
//! point (a `# diagnostic` line in the TAP block), never discarded. A failing row dumps per-relay
//! evidence (which relay, accept/reject, counts), never just "it didn't work".

use std::time::Duration;

use hb_core::{build_binding, verify_binding, Identity};
use hb_net::{fetch_presence_for_authors, select_newest_by_created_at, RelayClient};
use nostr::prelude::*;

use super::tap::Tap;

/// Online freshness window — matches `commands::online::ONLINE_WINDOW_SECS` (600 s).
const ONLINE_WINDOW_SECS: u64 = 600;
/// A presence beacon TTL comfortably inside the freshness window.
const PRESENCE_TTL_SECS: u64 = 30 * 60;
/// Relay handshake/fetch timeout.
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);
/// Settle delay after a publish before a read (lets the relay index the event).
const SETTLE: Duration = Duration::from_secs(3);
/// Long-haul rows retry this many times before recording a failure.
const LONG_HAUL_RETRIES: u32 = 3;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run the WAN-P suite against a live `serve` peer. `peer_npub` is the served identity's public key
/// (parsed from its printed npub/share-code); `relays` is the relay set both sides use. P3 is
/// opt-in: it runs only when `flood_ctx` is `Some` (the probe was passed `--flood-relay` URLs that
/// satisfy the flood guard), and is skipped (with an explicit diagnostic, NOT a green skip) when the
/// relay-citizenship guard is not satisfied.
pub async fn run(
    tap: &mut Tap,
    peer_npub: &PublicKey,
    relays: &[String],
    flood_ctx: Option<&FloodCtx>,
) {
    // P1 — the W1 fix row: author-filtered read resolves the served peer regardless of the global cap.
    tap.check(
        "P1: resolve served peer via the production presence path (fetch_presence_for_authors) within 600s",
        p1_resolve_via_production_path(peer_npub, relays).await,
    );

    // P2 — per-relay acceptance audit.
    tap.check(
        "P2: beacon accepted by at least one relay (per-relay evidence recorded)",
        p2_per_relay_acceptance(relays).await,
    );

    // P3 — cap-displacement (VPS strfry only). Refuses to run unless the flood guard is satisfied.
    tap.check(
        "P3: cap-displacement — contact presence still resolves after N>cap foreign beacons (VPS strfry only)",
        p3_cap_displacement(peer_npub, relays, flood_ctx).await,
    );

    // P4 — retry regression. W1 shipped the retry: a failed cycle backs off inside the 600s window.
    tap.check(
        "P4: a failed publish cycle retries inside the 600s window (not the full 300s cadence)",
        p4_retry_within_window(relays).await,
    );

    // P5 — same-NAT author-filtered resolution (SAMENAT1 body folded in, pointed at the live relay set).
    tap.check(
        "P5: same-NAT — two identities from one IP each resolve via the author-filtered read",
        p5_same_nat_plus_two(relays).await,
    );
}

// ---------------------------------------------------------------------------
// P1 — production presence path (the W1 regression row)
// ---------------------------------------------------------------------------

/// Resolve the served peer through the **same production path the contact-row pill uses**:
/// `commands::online::refresh_count` → `hb_net::fetch_presence_for_authors` (the author-filtered
/// read) → the `fresh` map. The harness drives real shipped code — it does not reimplement the
/// query. Asserts the peer's npub appears in the fresh map within the 600 s window.
///
/// **W1 (2026-08-02):** the read is now author-filtered (`.authors([peer_npub])` +
/// `.limit(1)`), so the relay's global response cap cannot displace the served beacon — the peer
/// resolves regardless of ambient network volume. Pre-W1 this row drove the global aggregate and was
/// red on busy public relays; the fix retargets the read, and this row exercises that fixed path.
///
/// Retries ×3 with a settle between attempts (flake policy); a persistent failure is an honest
/// `not ok` with the observed counts as the diagnostic.
async fn p1_resolve_via_production_path(peer_npub: &PublicKey, relays: &[String]) -> Result<(), String> {
    let observer = Identity::generate();
    let mut last_diag = String::new();
    for attempt in 1..=LONG_HAUL_RETRIES {
        let client = match RelayClient::connect(&observer, relays, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                last_diag = format!("attempt {attempt}: connect failed: {e}");
                continue;
            }
        };
        // THE production read the contact pill uses (hb-net::fetch_presence_for_authors →
        // hb-net::count::presence_authors_filter: author-filtered kind-11111, W1 fix).
        let (fresh, now) = match fetch_presence_for_authors(&client, &[*peer_npub], ONLINE_WINDOW_SECS, RELAY_TIMEOUT).await {
            Ok(v) => v,
            Err(e) => {
                last_diag = format!("attempt {attempt}: fetch_presence_for_authors failed: {e}");
                client.disconnect().await;
                continue;
            }
        };
        client.disconnect().await;

        // Does the served peer appear fresh? The fresh map is PublicKey → newest created_at.
        let observed = fresh.get(peer_npub).copied();
        let total_online = fresh.len();
        match observed {
            Some(ts) => {
                let age = now.saturating_sub(ts);
                if age <= ONLINE_WINDOW_SECS {
                    return Ok(());
                }
                last_diag = format!(
                    "attempt {attempt}: peer seen but stale (created_at={ts}, age={age}s > {ONLINE_WINDOW_SECS}s window); {total_online} npubs in the author-filtered map"
                );
            }
            None => {
                last_diag = format!(
                    "attempt {attempt}: peer NOT in the author-filtered fresh map ({total_online} npubs returned — the served beacon did not resolve)"
                );
            }
        }
        tokio::time::sleep(SETTLE).await;
    }
    Err(last_diag)
}

// ---------------------------------------------------------------------------
// P2 — per-relay acceptance audit
// ---------------------------------------------------------------------------

/// Publish the beacon and record EACH relay's accept/reject individually. `RelayClient::publish`
/// returns a `PublishOutcome { accepted, rejected }` — the per-relay evidence. Emits one diagnostic
/// line per relay to stderr (the evidence dump a silent "Ok" would otherwise hide). The row FAILS
/// only if ZERO relays accepted; partial acceptance is recorded evidence, not a pass.
async fn p2_per_relay_acceptance(relays: &[String]) -> Result<(), String> {
    let author = Identity::generate();
    let beacon = build_binding(&author, unix_now(), PRESENCE_TTL_SECS)
        .map_err(|e| format!("build beacon: {e}"))?;
    let client = RelayClient::connect(&author, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    // publish() errors only if NO relay accepted; the outcome still carries the per-relay split for
    // the all-reject diagnostic. Drive the real production publish path (presence.rs::publish_presence
    // calls client.publish(&event) the same way).
    let outcome = match client.publish(&beacon).await {
        Ok(o) => o,
        Err(e) => {
            // All relays rejected or dropped — record the net error, then probe per-relay status so
            // the diagnostic names which relay refused (never just "it didn't work").
            client.disconnect().await;
            return Err(format!(
                "publish rejected by all relays: {e}\n# relay set: {}",
                relays.join(", ")
            ));
        }
    };
    client.disconnect().await;

    // The evidence dump (per relay) — to stderr so it appears as a TAP `# diagnostic` context block.
    eprintln!("   P2 per-relay outcome:");
    for url in &outcome.accepted {
        eprintln!("   P2   {url}: ACCEPTED");
    }
    for (url, reason) in &outcome.rejected {
        eprintln!("   P2   {url}: REJECTED ({reason})");
    }

    if outcome.accepted.is_empty() {
        return Err(format!(
            "zero relays accepted the beacon (rejected: {:?})",
            outcome.rejected
        ));
    }
    // Partial acceptance is recorded evidence — the row passes when at least one relay accepted.
    Ok(())
}

// ---------------------------------------------------------------------------
// P3 — cap-displacement (VPS strfry ONLY)
// ---------------------------------------------------------------------------

/// Configuration for the P3 cap-displacement row, parsed from `--flood-relay`/`--flood-count` on the
/// probe. `None` on `FloodCtx` means the row was not armed; the row refuses with a diagnostic.
pub struct FloodCtx {
    /// The relay URLs P3 may flood (the `--flood-relay` set, intended: the VPS strfry backbone).
    pub flood_relays: Vec<String>,
    /// The `--relay` set the probe uses for reads, which the flood guard checks against
    /// `flood_relays` (every read relay must be a flood relay, or the row refuses).
    pub read_relays: Vec<String>,
    /// How many foreign kind-11111 events to publish (default ~600, > a low `maxFilterLimit`).
    pub flood_count: u32,
}

/// Publish N > cap foreign kind-11111 events from throwaway keys, then assert the served contact's
/// presence still resolves via the **author-filtered** production read — the W1 red→green
/// discriminator. The author-filtered read (`fetch_presence_for_authors`) survives the flood
/// (cap-immune); the pre-W1 global read did not.
///
/// **Relay citizenship guard (standing, M16):** flood-shaped rows NEVER run against public relays.
/// `flood_guard_violations` returns any `read_relay` not in the flood allowlist; if non-empty, the
/// row refuses to run and names the offending URLs — it does NOT silently flood a public relay.
async fn p3_cap_displacement(
    peer_npub: &PublicKey,
    relays: &[String],
    flood_ctx: Option<&FloodCtx>,
) -> Result<(), String> {
    let Some(ctx) = flood_ctx else {
        return Err(
            "P3 SKIPPED (not armed): pass --flood-relay <url>... (VPS strfry only) to run the cap-displacement row. \
             It is skipped here rather than run against a public relay — relay citizenship forbids flood-shaped rows on the public defaults."
                .to_string(),
        );
    };

    // The guard: every read relay must be a flood relay (so the operator explicitly named each target).
    let violations = super::args::flood_guard_violations(&ctx.read_relays, &ctx.flood_relays);
    if !violations.is_empty() {
        return Err(format!(
            "P3 REFUSED (relay citizenship): these --relay URLs are not in the --flood-relay allowlist: {}. \
             Flood-shaped rows run only against explicitly-passed VPS strfry (ws://198.51.100.1:7777, ws://198.51.100.2:7777).",
            violations.join(", ")
        ));
    }

    eprintln!("   P3 flooding {} foreign kind-11111 events across {} relay(s)", ctx.flood_count, ctx.flood_relays.len());

    // Publish N throwaway beacons (foreign identities) to displace the served beacon in a capped
    // aggregate. Best-effort: a publish error is recorded but does not abort the flood (the point is
    // to saturate the relay's window).
    for i in 0..ctx.flood_count {
        let foreign = Identity::generate();
        let beacon = match build_binding(&foreign, unix_now(), PRESENCE_TTL_SECS) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("   P3   flood event {i}: build failed: {e}");
                continue;
            }
        };
        let client = match RelayClient::connect(&foreign, &ctx.flood_relays, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("   P3   flood event {i}: connect failed: {e}");
                continue;
            }
        };
        if let Err(e) = client.publish(&beacon).await {
            eprintln!("   P3   flood event {i}: publish rejected: {e}");
        }
        client.disconnect().await;
    }

    tokio::time::sleep(SETTLE).await;

    // Now read the served peer through the author-filtered production path (same as P1, W1 fix). The
    // author-filtered read survives the flood (cap-immune) — the real red→green discriminator.
    let observer = Identity::generate();
    let client = RelayClient::connect(&observer, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("post-flood connect: {e}"))?;
    let (fresh, now) = fetch_presence_for_authors(&client, &[*peer_npub], ONLINE_WINDOW_SECS, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("post-flood fetch_presence_for_authors: {e}"))?;
    client.disconnect().await;

    let observed = fresh.get(peer_npub).copied();
    let total = fresh.len();
    match observed {
        Some(ts) => {
            let age = now.saturating_sub(ts);
            if age <= ONLINE_WINDOW_SECS {
                Ok(())
            } else {
                Err(format!(
                    "peer seen but stale (age={age}s); {total} npubs in the author-filtered map after the flood"
                ))
            }
        }
        None => Err(format!(
            "peer NOT in the author-filtered fresh map after the flood ({total} npubs returned) — the author-filtered read should have survived the flood (cap-immune)"
        )),
    }
}

// ---------------------------------------------------------------------------
// P4 — retry regression
// ---------------------------------------------------------------------------

/// Assert that a failed publish cycle does not yield a permanent offline (a retry lands inside the
/// 600 s window rather than waiting the full 300 s cadence).
///
/// **W1 (2026-08-02) shipped the retry:** `presence.rs::next_delay` selects a fast backoff
/// (15/30/60/120 s) on failure instead of the full 300 s cadence. Legs (1)-(3) collect the live
/// publish evidence (a failed cycle against an unreachable relay, then a good-leg publish that
/// succeeds and verifies); leg (4) asserts the shipped `next_delay` selector directly — the pure,
/// deterministic proxy the loop uses (driving the real >300 s loop is impractical).
async fn p4_retry_within_window(relays: &[String]) -> Result<(), String> {
    use crate::presence::{fetch_peer_presence, publish_presence};

    let author = Identity::generate();

    // (1) Induce a failed publish cycle: point the beacon at an unreachable relay. The real
    //     publish path is presence.rs::publish_presence (client.publish under the hood). Connect to a
    //     TEST-NET-1 (RFC 5737) address — guaranteed unroutable — so the handshake fails: that failed
    //     cycle IS the thing P4 asserts the production loop does NOT fast-retry. `.ok()` flattens the
    //     expected Err into None (the leg we wanted to induce); the Some branch below guards against
    //     the improbable case a connection object came back despite the dead host.
    let dead = vec!["ws://192.0.2.1:7777".to_string()];
    let dead_client: Option<RelayClient> = RelayClient::connect(&author, &dead, Duration::from_secs(3))
        .await
        .ok();
    if let Some(c) = dead_client {
        // If a connection object came back despite the dead host, publishing through it must fail.
        let _ = publish_presence(&c, &author).await;
        c.disconnect().await;
    }
    eprintln!("   P4 induced a failed publish cycle against an unreachable relay (the failed leg)");

    // (2) The production path DOES succeed against a good relay — publish_presence is fine.
    let good_client = RelayClient::connect(&author, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("good-leg connect failed: {e}"))?;
    publish_presence(&good_client, &author)
        .await
        .map_err(|e| format!("good-leg publish_presence failed: {e}"))?;
    good_client.disconnect().await;
    eprintln!("   P4 good-leg publish succeeded (presence.rs::publish_presence is functional)");

    // (3) Fetch the peer's own presence back via the production read to confirm the beacon landed.
    let reader = RelayClient::connect(&author, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("verify-leg connect failed: {e}"))?;
    let presence = fetch_peer_presence(&reader, &author.public_key(), RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("verify-leg fetch_peer_presence: {e}"))?;
    reader.disconnect().await;
    let Some(ev) = presence else {
        return Err("verify-leg: no presence returned".to_string());
    };
    if verify_binding(&ev, &author.public_key(), unix_now()).is_err() {
        return Err("verify-leg: binding did not verify".to_string());
    }

    // (4) W1 shipped the retry: a failed cycle now backs off fast (inside the 600s window) instead
    //     of waiting the full 300s cadence. Assert the production selector directly (driving the real
    //     >300s loop is impractical; next_delay is the pure, deterministic proxy the loop uses).
    use crate::presence::{next_delay, PRESENCE_REFRESH_SECS};
    let first_retry = next_delay(false, 0);
    if first_retry >= Duration::from_secs(PRESENCE_REFRESH_SECS) {
        return Err(format!(
            "no fast retry: a failed cycle's delay is {first_retry:?} >= the 300s cadence"
        ));
    }
    if first_retry >= Duration::from_secs(ONLINE_WINDOW_SECS) {
        return Err(format!(
            "retry does not land inside the 600s window: {first_retry:?}"
        ));
    }
    eprintln!("   P4 retry lands in {first_retry:?} (< 300s cadence, inside the 600s window)");
    Ok(())
}

// ---------------------------------------------------------------------------
// P5 — same-NAT +2 (SAMENAT1 body folded in, pointed at the live relay set)
// ---------------------------------------------------------------------------

/// Two identities, both connected from **this process** (one source IP), each publish a presence
/// beacon; the observer must resolve BOTH via the **author-filtered** read (W1), regardless of the
/// global relay cap. This is the `SAMENAT1` body from `hb-it/src/same_nat.rs` adapted into the WAN
/// harness and pointed at the live relay set instead of ephemeral CI strfry. Every per-relay
/// `PublishOutcome` is dumped to stderr — the evidence a same-NAT reject would otherwise leave
/// invisible.
async fn p5_same_nat_plus_two(relays: &[String]) -> Result<(), String> {
    let observer = Identity::generate();
    let oc = RelayClient::connect(&observer, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("observer connect: {e}"))?;

    let alice = Identity::generate();
    let bob = Identity::generate();
    let ac = RelayClient::connect(&alice, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("alice connect: {e}"))?;
    let bc = RelayClient::connect(&bob, relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("bob connect: {e}"))?;

    let a_outcome = ac
        .publish(&build_binding(&alice, unix_now(), PRESENCE_TTL_SECS).map_err(|e| format!("alice beacon: {e}"))?)
        .await
        .map_err(|e| format!("alice publish: {e}"))?;
    let b_outcome = bc
        .publish(&build_binding(&bob, unix_now(), PRESENCE_TTL_SECS).map_err(|e| format!("bob beacon: {e}"))?)
        .await
        .map_err(|e| format!("bob publish: {e}"))?;
    eprintln!(
        "   P5 alice publish: accepted={:?} rejected={:?}",
        a_outcome.accepted, a_outcome.rejected
    );
    eprintln!(
        "   P5 bob   publish: accepted={:?} rejected={:?}",
        b_outcome.accepted, b_outcome.rejected
    );
    tokio::time::sleep(SETTLE).await;

    verify_peer_online(&oc, &alice).await?;
    verify_peer_online(&oc, &bob).await?;

    // W1: assert both same-NAT identities resolve via the AUTHOR-FILTERED read (cap-immune), the
    // property the fix delivers — not the global count's +2, which stays subject to the relay cap.
    let (fresh, now) = fetch_presence_for_authors(
        &oc,
        &[alice.public_key(), bob.public_key()],
        ONLINE_WINDOW_SECS,
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("author-filtered read: {e}"))?;

    ac.disconnect().await;
    bc.disconnect().await;
    oc.disconnect().await;

    let a_fresh = fresh.get(&alice.public_key()).map(|ts| now.saturating_sub(*ts) <= ONLINE_WINDOW_SECS).unwrap_or(false);
    let b_fresh = fresh.get(&bob.public_key()).map(|ts| now.saturating_sub(*ts) <= ONLINE_WINDOW_SECS).unwrap_or(false);
    if a_fresh && b_fresh {
        Ok(())
    } else {
        Err(format!(
            "same-NAT author-filtered shortfall: alice_fresh={a_fresh} bob_fresh={b_fresh} \
             (both should resolve via the author-filtered read regardless of global cap — \
             a shortfall names a per-IP relay reject in the P5 publish diagnostics above)"
        ))
    }
}

/// Fetch `peer`'s newest presence via `client` and assert it verifies as a fresh online beacon.
/// (Mirrors `hb-it/same_nat.rs::verify_peer_online` — small enough that duplicating beats a premature
/// shared helper across two suites that evolve independently, per the project rule.)
async fn verify_peer_online(client: &RelayClient, peer: &Identity) -> Result<(), String> {
    let events = client
        .fetch(
            Filter::new()
                .author(peer.public_key())
                .kind(Kind::from_u16(hb_core::binding::KIND_PRESENCE)),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("verify_peer_online fetch: {e}"))?;
    let newest = select_newest_by_created_at(events).ok_or_else(|| "no presence for peer".to_string())?;
    let created = newest.created_at.as_secs();
    if unix_now().saturating_sub(created) > ONLINE_WINDOW_SECS {
        return Err(format!("peer's presence is not within the online window (age={}s)", unix_now().saturating_sub(created)));
    }
    verify_binding(&newest, &peer.public_key(), unix_now())
        .map_err(|e| format!("verify_binding failed: {e}"))?;
    Ok(())
}

/// The canary's P2 reuse point: publish a beacon and record EACH relay's accept/reject. This is the
/// standalone form of the P2 row body (the canary does not run the full WAN-P suite — it runs this one
/// row). Returns Ok when at least one relay accepted, Err with per-relay evidence otherwise.
pub async fn canary_beacon_acceptance(relays: &[String]) -> Result<(), String> {
    p2_per_relay_acceptance(relays).await
}
