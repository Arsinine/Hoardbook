//! WAN-D — discovery against real relays (M20 W6 §W6). Four rows that are the **live twins of the
//! `hb-it` L2 discovery / big-relay suites**, pointed at real infrastructure instead of ephemeral CI
//! strfry:
//!
//! - **D1** — cross-region visibility: a teaser published via the production path is found by a client
//!   reading a *different* relay of the set. Asserts the production publish contract (the teaser is
//!   written to EVERY relay in the set via `RelayClient::publish`, which fans out across all connected
//!   relays), then each relay individually serves it.
//! - **D2** — NIP-65 resolution end-to-end on the live relays (adapts `hb-it/suite_disc::disc2`).
//! - **D3** — search-eviction measurement (W3's discriminator, VPS strfry only): an all-tags-AND-
//!   matching teaser older than >cap loose matches must still surface via the production
//!   `search_teasers` path. W3 shipped the `.limit()` + ranking (`de3df5d`); this row MEASURES it
//!   against a live capped relay rather than assuming green. Flood-guarded (relay citizenship) to
//!   `--flood-relay` VPS only.
//! - **D4** — BIGRELAY (BIG1/BIG2) cross-region: full family on the big relay only, INV-5 no-leak,
//!   stale-snapshot gate — adapts `hb-it/suite_bigrelay` against real infrastructure.
//!
//! **Shape: probe-plays-both (all rows).** Per the §W6 task instruction, every row uses two in-process
//! identities against the live relay set — the same shape `hb-it` L2 bodies already use. This keeps
//! `serve` minimal and exercises the exact production read/write paths a real client uses.
//!
//! **D1 production-contract finding.** Production publishes a teaser via `publish_listing_capped` →
//! `RelayClient::publish` → nostr-sdk's `send_event`, which fans out to **every relay in the client's
//! connected set** (the full `net::relay_urls(store)` set). A teaser is therefore written to ALL relays
//! the publisher is connected to, not just one. The two VPS strfry instances do NOT federate, so
//! cross-region visibility is a property of the *publish fan-out*, not relay-to-relay forwarding. D1
//! asserts THAT contract: publish via the production path (fan-out to the full set), then assert each
//! relay individually serves the teaser. A single-machine smoke with the set = [SG, JP] proves both
//! relays hold the event because the publish wrote to both.
//!
//! **Honest red.** Nothing here is `# TODO`/skip. A leg that fails on environment grounds is an honest
//! `not ok` with a per-step evidence dump.
//!
//! **Flake policy (P3b precedent):** long-haul rows retry ×3; every failure is a recorded data point.

use std::time::Duration;

use anyhow::Result;
use hb_core::event::{
    build_listing_event, build_teaser, parse_listing_event, KIND_LISTING, KIND_TEASER, Teaser,
};
use hb_core::Identity;
use hb_net::{
    bootstrap_order, build_relay_list, fetch_full_listing_from, fetch_full_listing_if_current,
    parse_relay_list, publish_listing_capped, publish_listing_to, search_teasers, RelayClient,
};
use nostr::prelude::*;
use serde_json::Value;

use crate::wan_it::tap::Tap;

use super::args;

// ---------------------------------------------------------------------------
// Constants — timeouts, retries, settle (match WAN-P / WAN-U conventions)
// ---------------------------------------------------------------------------

/// Relay handshake/fetch timeout (matches WAN-P / WAN-U).
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);

/// Settle between a publish and a read (lets the live relay index the event).
const SETTLE: Duration = Duration::from_secs(3);

/// The truncation threshold (the production constant from `commands::collection::LISTING_MAX_BYTES`).
const LISTING_MAX_BYTES: usize = 40_000;

/// The production search cap (matches `commands::browse::SEARCH_CAP`). D3 publishes N > this loose
/// matches then asserts the strict-AND survivor still surfaces.
const SEARCH_CAP: usize = 100;

// ---------------------------------------------------------------------------
// Probe input — built by run_probe_wan_d from the parsed args
// ---------------------------------------------------------------------------

/// The input the WAN-D probe needs. The rows construct their own throwaway publisher/browser identities
/// internally (the probe-plays-both shape), so this carries only the relay set + the D3 flood context
/// (opt-in, VPS strfry only).
pub struct ProbeInput {
    /// The relay URLs every row publishes to and reads from (the full set, e.g. [SG, JP]).
    pub relays: Vec<String>,
    /// D3 flood context: when `Some`, D3 runs against these explicitly-passed VPS strfry URLs (relay
    /// citizenship — flood-shaped rows never run against public defaults). When `None`, D3 is refused
    /// with a diagnostic.
    pub flood_ctx: Option<FloodCtx>,
}

/// D3 configuration: the flood relays + how many loose-match teasers to publish.
pub struct FloodCtx {
    /// The relay URLs D3 may flood (the `--flood-relay` set, intended: the VPS strfry backbone).
    pub flood_relays: Vec<String>,
    /// The `--relay` set the probe uses for the post-flood read. The flood guard checks every read
    /// relay is a flood relay (relay citizenship).
    pub read_relays: Vec<String>,
    /// How many loose-match teasers to publish (default modest — enough to exceed a LOW relay cap).
    pub flood_count: u32,
}

/// Build the WAN-D probe input from the parsed args.
pub async fn build_probe_input(relays: Vec<String>, flood_ctx: Option<FloodCtx>) -> Result<ProbeInput> {
    Ok(ProbeInput { relays, flood_ctx })
}

/// Run the WAN-D rows (D1–D4) against the live relay set. Each row is an honest TAP check:
/// Ok ⇒ pass, Err(detail) ⇒ fail with a `# diagnostic` block.
pub async fn run(tap: &mut Tap, probe: &ProbeInput) {
    tap.check(
        "D1: teaser published via the production fan-out is served by EACH relay of the set individually",
        d1_cross_region_visibility(probe).await,
    );

    tap.check(
        "D2: NIP-65 resolution end-to-end — peer's advertised relays lead the bootstrap order, teaser found there",
        d2_nip65_resolution(probe).await,
    );

    tap.check(
        "D3: search-eviction — all-tags-AND match survives >cap loose matches (W3 discriminator, VPS strfry only)",
        d3_search_eviction(probe).await,
    );

    tap.check(
        "D4: BIGRELAY — full family on the big relay only, INV-5 no public leak, stale snapshot gated",
        d4_bigrelay_cross_region(probe).await,
    );
}

// ---------------------------------------------------------------------------
// Small helpers shared across rows (adapted from WAN-U / hb-it/harness.rs)
// ---------------------------------------------------------------------------

/// A deterministic-ish browse-key for a seed byte (matches `hb-it/suite_browse::bk`).
fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// A teaser with a display name + tags (matches `hb-it/suite_browse::teaser`).
fn teaser(name: &str, tags: Vec<String>, cts: Vec<String>) -> Teaser {
    Teaser { display_name: name.into(), bio: "hoards".into(), tags, content_types: cts, picture: None }
}

/// Connect a client to the relay set (matches `hb-it/harness.rs::Ctx::connect`).
async fn connect(id: &Identity, relays: &[String]) -> Result<RelayClient> {
    Ok(RelayClient::connect(id, relays, RELAY_TIMEOUT).await?)
}

/// A small settle after a publish before a read (lets the live relay index the event).
async fn settle() {
    tokio::time::sleep(SETTLE).await;
}

// ---------------------------------------------------------------------------
// D1 — cross-region visibility (the production publish-contract row)
//
// Production publishes a teaser via `publish_listing_capped` → `RelayClient::publish` → nostr-sdk
// `send_event`, which fans out to EVERY relay in the client's connected set. The two VPS strfry
// instances do NOT federate, so cross-region visibility is a property of the publish fan-out, not
// relay forwarding. This row asserts THAT contract: publish a teaser via the production publish path
// (the same `client.publish` `publish_listing_capped` calls internally), then read it back from EACH
// relay INDIVIDUALLY — proving each relay holds the event because the publish wrote to it.
// ---------------------------------------------------------------------------

async fn d1_cross_region_visibility(probe: &ProbeInput) -> Result<(), String> {
    if probe.relays.len() < 2 {
        return Err(format!(
            "D1 needs >=2 relays to assert cross-region visibility (got {}); pass both VPS strfry URLs",
            probe.relays.len()
        ));
    }
    let author = Identity::generate();
    let slug = format!("wan-d-d1-{}", token());
    // A small listing that publishes whole (not truncated) — the focus is the fan-out, not truncation.
    let listing_json = serde_json::json!({
        "slug": slug, "content_types": ["video"], "entries": [{"name": "d1-file"}],
    })
    .to_string();

    // Publish via the production path. publish_listing_capped calls client.publish(&event), which
    // fans out to EVERY connected relay (the full set). This IS the production contract. Build the
    // event via the same production helper (build_listing_event) and publish it directly so the
    // per-relay accept/reject split is captured as evidence — publish_listing_capped would discard it.
    let client = connect(&author, &probe.relays)
        .await
        .map_err(|e| format!("D1 publisher connect: {e}"))?;
    let event = build_listing_event(&author, &slug, &bk(7), &listing_json)
        .map_err(|e| format!("D1 build_listing_event: {e}"))?;
    let outcome = client.publish(&event).await.map_err(|e| format!("D1 publish (fan-out): {e}"))?;
    client.disconnect().await;
    eprintln!(
        "   D1 fan-out publish accepted by: {} (rejected: {:?})",
        outcome.accepted.join(", "),
        outcome.rejected
    );
    settle().await;

    // Read back from EACH relay INDIVIDUALLY. A single-relay fetch per relay proves that relay holds
    // the event — the cross-region visibility claim, asserted per-relay (not via a multi-relay fan-in
    // that could mask one relay missing it).
    let mut missing: Vec<String> = Vec::new();
    for relay_url in &probe.relays {
        let one = std::slice::from_ref(relay_url);
        let reader = connect(&Identity::generate(), one)
            .await
            .map_err(|e| format!("D1 read connect to {relay_url}: {e}"))?;
        let events = reader
            .fetch(
                Filter::new()
                    .author(author.public_key())
                    .kind(Kind::from_u16(KIND_LISTING)),
                RELAY_TIMEOUT,
            )
            .await
            .map_err(|e| format!("D1 read fetch from {relay_url}: {e}"))?;
        reader.disconnect().await;
        let found = events.iter().any(|e| e.tags.identifier() == Some(slug.as_str()));
        eprintln!("   D1 relay {relay_url}: {}", if found { "HOLDS the teaser" } else { "MISSING the teaser" });
        if !found {
            missing.push(relay_url.clone());
        }
    }

    if !missing.is_empty() {
        return Err(format!(
            "D1 cross-region visibility FAILED: these relays do NOT hold the teaser after a production \
             fan-out publish: {}. The publish path wrote to {} relay(s); the missing relays either \
             rejected the write or did not index it. Production contract: publish_listing_capped → \
             client.publish fans out to every connected relay.",
            missing.join(", "),
            probe.relays.len()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// D2 — NIP-65 resolution end-to-end (adapts hb-it/suite_disc::disc2)
//
// Publish a kind-10002 relay list advertising one relay, plus a teaser, then resolve the peer's
// NIP-65 from a DIFFERENT configured relay and fetch the teaser from the advertised set. The
// bootstrap order leads with the peer's advertised outbox.
// ---------------------------------------------------------------------------

async fn d2_nip65_resolution(probe: &ProbeInput) -> Result<(), String> {
    let peer = Identity::generate();
    // Advertise the first relay of the set as the peer's outbox.
    let advertised = vec![probe.relays[0].clone()];
    let relay_list = build_relay_list(&peer, &advertised, &advertised)
        .map_err(|e| format!("D2 build_relay_list: {e}"))?;
    let tag = format!("wan-d-d2-{}", token());
    let tea = build_teaser(&peer, &teaser("d2-peer", vec![tag.clone()], vec!["video".into()]), true)
        .map_err(|e| format!("D2 build_teaser: {e}"))?;

    // Publish both events to the full set.
    let pubc = connect(&peer, &probe.relays)
        .await
        .map_err(|e| format!("D2 publisher connect: {e}"))?;
    pubc.publish(&relay_list).await.map_err(|e| format!("D2 relay-list publish: {e}"))?;
    pubc.publish(&tea).await.map_err(|e| format!("D2 teaser publish: {e}"))?;
    pubc.disconnect().await;
    settle().await;

    // (1) Resolve the peer's NIP-65 from a configured relay. Use the LAST relay of the set as the
    //     resolver (so when the set is [SG, JP], the resolver is JP while the advertised outbox is
    //     SG — a genuine cross-region resolve when both are passed).
    let resolver_url = probe.relays.last().unwrap().clone();
    let resolver = connect(&Identity::generate(), std::slice::from_ref(&resolver_url))
        .await
        .map_err(|e| format!("D2 resolver connect to {resolver_url}: {e}"))?;
    let lists = resolver
        .fetch(Filter::new().author(peer.public_key()).kind(Kind::RelayList), RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("D2 NIP-65 fetch from {resolver_url}: {e}"))?;
    resolver.disconnect().await;
    if lists.len() != 1 {
        return Err(format!("D2 expected 1 NIP-65 event from {resolver_url}, got {}", lists.len()));
    }
    let list = parse_relay_list(&lists[0]).map_err(|e| format!("D2 parse_relay_list: {e}"))?;
    if list.write.is_empty() {
        return Err("D2 NIP-65 advertised no write relays".to_string());
    }
    eprintln!("   D2 NIP-65 resolved from {resolver_url}: {} write relay(s)", list.write.len());

    // (2) The bootstrap order leads with the advertised outbox. Fetch the teaser from that order.
    let order = bootstrap_order(&probe.relays, &[], Some(&list));
    eprintln!("   D2 bootstrap order: {}", order.join(", "));
    let from_advertised = connect(&Identity::generate(), &order)
        .await
        .map_err(|e| format!("D2 advertised-set connect: {e}"))?;
    let got = from_advertised
        .fetch(
            Filter::new().author(peer.public_key()).kind(Kind::from_u16(KIND_TEASER)),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("D2 teaser fetch from advertised set: {e}"))?;
    from_advertised.disconnect().await;
    if got.len() != 1 {
        return Err(format!(
            "D2 teaser not found on the peer's advertised relays (got {} events, expected 1)",
            got.len()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// D3 — search-eviction measurement (W3's discriminator, VPS strfry ONLY)
//
// W3 shipped (`de3df5d`: `TEASER_SEARCH_FETCH_LIMIT` 1000 + `rank_hits`). This row MEASURES it against
// a live capped relay: publish N loose-match teasers (single-tag, matching the OR-union but NOT the
// AND) from throwaway keys, then publish ONE strict-AND-match teaser OLDER than the flood, and assert
// the strict match still surfaces via the production `search_teasers` path.
//
// Relay citizenship: flood-shaped rows NEVER run against public relays. The flood guard requires every
// read relay to be in the --flood-relay allowlist.
// ---------------------------------------------------------------------------

async fn d3_search_eviction(probe: &ProbeInput) -> Result<(), String> {
    let Some(ctx) = &probe.flood_ctx else {
        return Err(
            "D3 SKIPPED (not armed): pass --flood-relay <url>... (VPS strfry only) to run the \
             search-eviction measurement. It is skipped here rather than run against a public relay — \
             relay citizenship forbids flood-shaped rows on the public defaults."
                .to_string(),
        );
    };

    // The guard: every read relay must be a flood relay.
    let violations = args::flood_guard_violations(&ctx.read_relays, &ctx.flood_relays);
    if !violations.is_empty() {
        return Err(format!(
            "D3 REFUSED (relay citizenship): these --relay URLs are not in the --flood-relay allowlist: {}. \
             Flood-shaped rows run only against explicitly-passed VPS strfry (ws://198.51.100.1:7777, \
             ws://198.51.100.2:7777).",
            violations.join(", ")
        ));
    }

    // The strict-AND-match tag and the loose-match (single-tag) tag. A teaser tagged [want, want2]
    // matches the AND of [want, want2]; a teaser tagged [want] only matches the OR-union (the relay
    // returns it for the #want query, but `teaser_matches` discards it client-side for lacking want2).
    let want = format!("wan-d-d3-want-{}", token());
    let want2 = format!("wan-d-d3-want2-{}", token());

    // (1) Publish the strict-AND-match teaser FIRST (so it is OLDER than the flood — the eviction
    //     symptom is an older strict match displaced by newer loose matches). A single throwaway key.
    let strict_author = Identity::generate();
    let strict_teaser = build_teaser(
        &strict_author,
        &teaser("d3-strict", vec![want.clone(), want2.clone()], vec!["video".into()]),
        true,
    )
    .map_err(|e| format!("D3 build strict teaser: {e}"))?;
    let sc = connect(&strict_author, &ctx.flood_relays)
        .await
        .map_err(|e| format!("D3 strict connect: {e}"))?;
    sc.publish(&strict_teaser).await.map_err(|e| format!("D3 strict publish: {e}"))?;
    sc.disconnect().await;
    eprintln!("   D3 strict-AND teaser published (older than the flood)");

    // (2) Flood N loose-match teasers (tagged [want] only — the OR-union returns them, but they fail
    //     the AND). Each from a throwaway key so they are distinct npubs (dedup keeps one per npub).
    eprintln!(
        "   D3 flooding {} loose-match teasers across {} relay(s)",
        ctx.flood_count,
        ctx.flood_relays.len()
    );
    for i in 0..ctx.flood_count {
        let foreign = Identity::generate();
        let teaser_ev = match build_teaser(
            &foreign,
            &teaser(&format!("d3-loose-{i}"), vec![want.clone()], vec!["video".into()]),
            true,
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("   D3   flood teaser {i}: build failed: {e}");
                continue;
            }
        };
        let client = match RelayClient::connect(&foreign, &ctx.flood_relays, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("   D3   flood teaser {i}: connect failed: {e}");
                continue;
            }
        };
        if let Err(e) = client.publish(&teaser_ev).await {
            eprintln!("   D3   flood teaser {i}: publish rejected: {e}");
        }
        client.disconnect().await;
    }
    settle().await;

    // (3) The production search path: `search_teasers` (the function `search_peers` wraps). It builds
    //     `teaser_search_filter` (with the W3 `.limit(1000)`), fetches, then ingests (AND-filter +
    //     dedup + rank). The strict-AND teaser MUST surface — if W3's `.limit()` were absent, the
    //     flood would evict it before the client filter saw it.
    let reader = connect(&Identity::generate(), &ctx.read_relays)
        .await
        .map_err(|e| format!("D3 post-flood connect: {e}"))?;
    let hits = search_teasers(
        &reader,
        &[want.clone(), want2.clone()],
        &[],
        SEARCH_CAP,
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("D3 post-flood search_teasers: {e}"))?;
    reader.disconnect().await;

    let strict_npub = strict_author.public_key().to_bech32().unwrap_or_default();
    let found_strict = hits.iter().any(|h| h.npub == strict_npub);
    eprintln!(
        "   D3 search returned {} strict-AND hit(s) out of {} total; strict author surfaced: {}",
        hits.iter().filter(|h| h.npub == strict_npub).count(),
        hits.len(),
        found_strict
    );
    if !found_strict {
        return Err(format!(
            "D3 search-eviction FAILED: the strict-AND teaser (author {strict_npub}) did NOT surface \
             after {} loose-match teasers. search_teasers returned {} hits. This is the W3 symptom — \
             the relay's cap evicted the older strict match before the client AND-filter saw it. W3's \
             TEASER_SEARCH_FETCH_LIMIT=1000 + rank_hits should prevent this; if it recurs, the relay's \
             own cap is below the declared fetch budget.",
            ctx.flood_count,
            hits.len()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// D4 — BIGRELAY cross-region (adapts hb-it/suite_bigrelay BIG1/BIG2)
//
// Use one VPS relay as the "big relay" and the other as the normal set. The full listing family
// publishes to the big relay ONLY (publish_listing_to → publish_to, targeted), the truncated teaser
// to the normal set, INV-5 no-leak (the normal relay holds ONLY the teaser), and a stale big-relay
// snapshot does not supersede the teaser.
// ---------------------------------------------------------------------------

async fn d4_bigrelay_cross_region(probe: &ProbeInput) -> Result<(), String> {
    if probe.relays.len() < 2 {
        return Err(format!(
            "D4 needs >=2 relays (a big relay + a normal set); got {}",
            probe.relays.len()
        ));
    }
    // relay[0] = the normal/public set; relay[1] = the big relay.
    let normal = &probe.relays[0];
    let big = &probe.relays[1];

    let fp = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string();
    let fp_old = "1".repeat(64);

    // BIG1 — full family on the big relay only, teaser on the normal relay, INV-5 no-leak.
    let owner = Identity::generate();
    let key = bk(31);
    let slug = format!("wan-d-d4-{}", token());
    // QURATOR-169: publish the family through the PRODUCTION stamp, exactly as the big-relay arm of
    // the publish path does (`commands/collection.rs`, gated on `will_truncate`). The staleness gate
    // (`full_supersedes`) compares `teaser_fingerprint` — audit #25: the only value a teaser and a
    // full carrier can share — and that field exists ONLY where the stamp put it. A bare
    // `full_listing(...)` publish carried no such field, `family_teaser_fingerprint` read `None`,
    // and the gate correctly refused the family: a harness omission surfacing as a phantom product
    // defect (the third of its shape here, after `sanitize_node_addr` and `approve_request`).
    // `stamp_teaser_fingerprint` is derived, not hand-built: it runs the same `truncate_listing`
    // the teaser publish runs, on these exact bytes, and reads the digest back — the two cannot
    // drift the way a hand-stamped fixture (`hb-it`'s `full_listing(slug, n, fp, tf)` would be)
    // silently can. The teaser publish below re-truncates the same stamped bytes, so both arms see
    // identical meta and the digest is by construction the one the teaser carries.
    let full = crate::commands::collection::stamp_teaser_fingerprint(&full_listing(&slug, 1300, &fp))
        .map_err(|e| format!("D4 stamp_teaser_fingerprint: {e}"))?;

    // The value a real browser gates on is the TEASER's digest, NOT the full-tree `fp`. A truncated
    // teaser's `snapshot_fingerprint` meta IS its teaser digest (`truncate_listing` re-stamps it),
    // and a browser reads it off the teaser it just fetched. This row used to pass `fp`, which
    // `full_supersedes` then compared against the family's `teaser_fingerprint` — two different
    // digests, so the gate refused a family that was genuinely current. Read the expected value back
    // out of the stamped family rather than recomputing it, so it cannot drift from what the stamp
    // derived (QURATOR-169).
    let teaser_fp = serde_json::from_str::<Value>(&full)
        .ok()
        .and_then(|v| v.get("teaser_fingerprint").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| {
            "D4 the stamped family carries no teaser_fingerprint — the gate compares that field, so \
             a browser would have nothing to gate on. Entries must deserialize as DirectoryItem."
                .to_string()
        })?;

    // Full family → the big relay ONLY (publish_listing_to → publish_to, targeted).
    let cbig = connect(&owner, std::slice::from_ref(big))
        .await
        .map_err(|e| format!("D4 big-relay connect: {e}"))?;
    let published = publish_listing_to(
        &cbig,
        &owner,
        &slug,
        &key,
        &full,
        LISTING_MAX_BYTES,
        std::slice::from_ref(big),
    )
    .await
    .map_err(|e| format!("D4 publish_listing_to big relay: {e}"))?;
    if published.parts <= 2 {
        return Err(format!(
            "D4 the full family must split into >2 parts, got {} (the listing may be too small)",
            published.parts
        ));
    }
    cbig.disconnect().await;
    eprintln!("   D4 full family: {} parts → big relay ({}) ONLY", published.parts, big);

    // Truncated teaser → the normal relay ONLY.
    let cnormal = connect(&owner, std::slice::from_ref(normal))
        .await
        .map_err(|e| format!("D4 normal-relay connect: {e}"))?;
    let teaser_pub = publish_listing_capped(
        &cnormal,
        &owner,
        &slug,
        &key,
        &full,
        LISTING_MAX_BYTES,
    )
    .await
    .map_err(|e| format!("D4 publish_listing_capped normal relay: {e}"))?;
    cnormal.disconnect().await;
    if !teaser_pub.truncated {
        return Err("D4 the same listing must truncate to a single teaser event".to_string());
    }
    eprintln!("   D4 truncated teaser → normal relay ({}) ONLY", normal);
    settle().await;

    // QURATOR-169 evidence: `fetch_full_listing_if_current` collapsing to `None` here conflates four
    // materially different causes, and the old failure message asserted "fingerprint matched" for a
    // branch this row never evaluated. Before the gated fetch, independently read back what the big
    // relay actually holds for this author/slug and compare it against what the gated fetch needs.
    //   (a) parts never landed — publish reported success but the relay dropped/rate-limited them
    //       (publish counts events SENT, not events INDEXED). 0 part events readable → (a).
    //   (b) landed but not fetched — subscription/filter/timing (settle too short, restricted relay
    //       list not applied, relay slow to index). Parts readable here but `fetch_err`/`render_err`
    //       below → (b). If this looks like the big relay throttling a multi-part publish/read, the
    //       fix is a HARNESS correction (back off / retry), never a recorded product defect.
    //   (c) fetched but the fingerprints did not agree, so the staleness gate refused the family —
    //       see the `fingerprints_seen` vs `fp` comparison below. NOTE the real gate
    //       (`full_supersedes`) compares `teaser_fingerprint` against `fp`, and the fixture
    //       `full_listing` stamps NO `teaser_fingerprint` — a legitimate `(c)` way for this row to
    //       fail that is a FIXTURE gap, not a product fault (hb-it's suite_bigrelay stamps it).
    //   (d) all parts arrived but restitching failed — `fetch_err` carries the render error.
    let diag = d4_big_relay_state(big, &owner, &slug, &key, &teaser_fp).await;
    eprintln!("   D4 pre-fetch big-relay read: {}", diag.summary());

    // A holder browses: fetch the full family from the big relay, gated on the teaser's fingerprint.
    let browser = connect(&Identity::generate(), &probe.relays)
        .await
        .map_err(|e| format!("D4 browser connect: {e}"))?;
    let current = fetch_full_listing_if_current(
        &browser,
        &owner.public_key(),
        &slug,
        &key,
        std::slice::from_ref(big),
        &teaser_fp,
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("D4 fetch_full_listing_if_current: {e} (diag: {})", diag.summary()))?;
    let full_tree = current.ok_or_else(|| {
        format!(
            "D4 a current big-relay family must supersede the teaser — the gated fetch returned \
             None. Observed: {}",
            diag.summary()
        )
    })?;
    if !full_tree.complete() {
        return Err("D4 the full family did not render as a complete tree".to_string());
    }
    eprintln!("   D4 full family restitched complete: {} entries", full_tree.entries.len());

    // A wrong fingerprint must NOT supersede (the stale-guard, over the wire).
    let mismatched = fetch_full_listing_if_current(
        &browser,
        &owner.public_key(),
        &slug,
        &key,
        std::slice::from_ref(big),
        "deadbeef",
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("D4 mismatched-fp fetch: {e}"))?;
    if mismatched.is_some() {
        return Err("D4 a fingerprint mismatch must keep the teaser (the stale-guard failed)".to_string());
    }

    // INV-5 no-leak: the normal relay holds ONLY the truncated teaser — none of the #part family.
    let normal_read = connect(&Identity::generate(), std::slice::from_ref(normal))
        .await
        .map_err(|e| format!("D4 no-leak read connect: {e}"))?;
    let normal_events = normal_read
        .fetch(
            Filter::new().author(owner.public_key()).kind(Kind::from_u16(KIND_LISTING)),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("D4 no-leak fetch: {e}"))?;
    normal_read.disconnect().await;
    browser.disconnect().await;
    let leaked_parts = normal_events
        .iter()
        .any(|e| e.tags.identifier().is_some_and(|d| d.contains("#part")));
    if leaked_parts {
        return Err(format!(
            "D4 INV-5 LEAK: the normal relay ({normal}) holds a #part family member — the big-relay \
             family leaked to the public pool. publish_listing_to must target the big relay ONLY."
        ));
    }
    let holds_teaser = normal_events
        .iter()
        .any(|e| e.tags.identifier() == Some(slug.as_str()));
    if !holds_teaser {
        return Err(format!(
            "D4 the normal relay ({normal}) must hold the truncated teaser (slug {slug}), but it is absent"
        ));
    }
    eprintln!("   D4 INV-5 no-leak: normal relay ({normal}) holds ONLY the truncated teaser");

    // BIG2 — a stale big-relay snapshot (older fingerprint than the teaser's) must NOT supersede.
    let owner2 = Identity::generate();
    let key2 = bk(32);
    let slug2 = format!("wan-d-d4-stale-{}", token());
    let cbig2 = connect(&owner2, std::slice::from_ref(big))
        .await
        .map_err(|e| format!("D4 stale big-relay connect: {e}"))?;
    publish_listing_to(
        &cbig2,
        &owner2,
        &slug2,
        &key2,
        // Genuinely older CONTENT (1200 entries, not 1300). The teaser digest is derived from the
        // entries + elided count, so a copy that differs only in its `snapshot_fingerprint` meta
        // hashes IDENTICALLY and would legitimately supersede — changing the meta does not make a
        // copy stale. Stamped, so it carries a real teaser digest that genuinely differs from the
        // current one: before this the family was unstamped AND gated against `fp`, so the refusal
        // below could not fail and proved nothing (QURATOR-169).
        &crate::commands::collection::stamp_teaser_fingerprint(&full_listing(&slug2, 1200, &fp_old))
            .map_err(|e| format!("D4 stale stamp_teaser_fingerprint: {e}"))?,
        LISTING_MAX_BYTES,
        std::slice::from_ref(big),
    )
    .await
    .map_err(|e| format!("D4 stale publish_listing_to: {e}"))?;
    cbig2.disconnect().await;
    settle().await;

    let browser2 = connect(&Identity::generate(), &probe.relays)
        .await
        .map_err(|e| format!("D4 stale browser connect: {e}"))?;
    let stale = fetch_full_listing_if_current(
        &browser2,
        &owner2.public_key(),
        &slug2,
        &key2,
        std::slice::from_ref(big),
        &teaser_fp,
        RELAY_TIMEOUT,
    )
    .await
    .map_err(|e| format!("D4 stale fetch: {e}"))?;
    browser2.disconnect().await;
    if stale.is_some() {
        return Err("D4 BIG2: a stale big-relay snapshot must NOT supersede the teaser".to_string());
    }
    eprintln!("   D4 BIG2 stale big-relay snapshot correctly gated (teaser kept)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A full listing of `n` padded entries carrying `snapshot_fingerprint = fp` (matches
/// `hb-it/suite_bigrelay::full_listing`). Big enough to split into an index + several content parts
/// under the 40 KiB per-part budget. The returned JSON is UNSTAMPED: callers on the gated path must
/// pass it through the production stamp (`stamp_teaser_fingerprint`) before publishing — the gate
/// `fetch_full_listing_if_current` reads `teaser_fingerprint`, which only the stamp can derive
/// (over the visible entries + elided count, QURATOR-169).
fn full_listing(slug: &str, n: usize, fp: &str) -> String {
    let entries: Vec<Value> = (0..n)
        // `item_type` is REQUIRED by `DirectoryItem` (no serde default). Without it these entries do
        // not deserialize as a tree, `truncate_listing` cannot re-derive the teaser digest, and
        // `stamp_teaser_fingerprint` takes its documented "underivable" branch and returns the
        // listing UNCHANGED — silently. That is exactly how D4 kept failing with `teaser_fp=<none>`
        // after the stamp call was added (QURATOR-169, second pass).
        .map(|i| serde_json::json!({
            "name": format!("title-{i:05}-padding-padding-padding-xx"),
            "item_type": "File",
        }))
        .collect();
    serde_json::json!({
        "slug": slug, "content_types": ["video"], "snapshot_fingerprint": fp, "entries": entries,
    })
    .to_string()
}

/// A short hex token to namespace this run's tags/slugs (so counts stay correct on a relay holding
/// earlier runs' events).
fn token() -> String {
    let bytes: [u8; 4] = rand::random();
    hex::encode(bytes)
}

// ---------------------------------------------------------------------------
// D4 diagnostics (QURATOR-169) — what the big relay ACTUALLY holds, independently of the gated
// fetch under test. `fetch_full_listing_if_current` collapsing to `None` conflates four causes
// (parts-never-landed / landed-but-not-fetched / fingerprint-mismatch / restitch-failed); this
// read separates them so a WAN failure names WHICH it hit.
// ---------------------------------------------------------------------------

/// What an independent raw read of the big relay found for the D4 author/slug.
struct BigRelayState {
    /// How the raw read itself went (the diagnostic can fail too — never fold that into the row's
    /// verdict about the fetch path).
    read_err: Option<String>,
    /// Decrypted payload count for this slug's family (index + parts).
    parts_seen: usize,
    /// Distinct `d`-tags those payloads carried (index = the bare slug; parts = `slug#partN`).
    d_tags_seen: Vec<String>,
    /// Per-payload fingerprint pairs found in decrypted JSON, as `(d_tag, snapshot_fp, teaser_fp)`.
    /// `None` fingerprint = the payload carried no such field.
    fingerprints_seen: Vec<(String, Option<String>, Option<String>)>,
    /// Whether ANY decrypted payload carries the row's expected `snapshot_fingerprint` (`fp`).
    /// NOTE: the real gate compares `teaser_fingerprint` — see the (c) note at the call site.
    snapshot_fp_matches: bool,
    /// Whether ANY decrypted payload carries `teaser_fingerprint == fp` (what the gate reads).
    teaser_fp_matches: bool,
    /// What `fetch_full_listing_from` (ungated) did over the same relay: `Err(msg)` = fetch or
    /// render failed (cases b/d), `Ok(n)` = rendered fine with n entries, so a `None` from the
    /// gated call is the staleness gate refusing (case c).
    ungated: Result<usize, String>,
}

impl BigRelayState {
    fn summary(&self) -> String {
        let read = match &self.read_err {
            Some(e) => format!("raw read FAILED ({e})"),
            None => format!(
                "raw read: {} payload(s) across d-tags {:?}",
                self.parts_seen, self.d_tags_seen
            ),
        };
        let fps = self
            .fingerprints_seen
            .iter()
            .map(|(d, s, t)| {
                format!(
                    "{d}: snapshot_fp={} teaser_fp={}",
                    s.as_deref().unwrap_or("<none>"),
                    t.as_deref().unwrap_or("<none>")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let ungated = match &self.ungated {
            Ok(n) => format!("ungated fetch rendered a complete tree ({n} entries)"),
            Err(e) => format!("ungated fetch FAILED: {e}"),
        };
        format!(
            "{read}; fingerprints [{fps}]; snapshot_fp match: {}, teaser_fp (what the gate \
             compares) match: {}; {ungated}",
            self.snapshot_fp_matches, self.teaser_fp_matches
        )
    }
}

/// Independently read back what the big relay holds for `owner`/`slug` (QURATOR-169 evidence).
/// One `KIND_LISTING` fetch by author (the widest honest scope the family shape allows), filtered
/// client-side to this slug's family, then decrypted and inspected for both fingerprint fields.
/// Never fails the row: every error is carried into the printed summary.
async fn d4_big_relay_state(
    big: &str,
    owner: &Identity,
    slug: &str,
    key: &[u8; 32],
    fp: &str,
) -> BigRelayState {
    let part_prefix = format!("{slug}#part");
    let mut state = BigRelayState {
        read_err: None,
        parts_seen: 0,
        d_tags_seen: Vec::new(),
        fingerprints_seen: Vec::new(),
        snapshot_fp_matches: false,
        teaser_fp_matches: false,
        ungated: Err("not attempted".to_string()),
    };

    let reader = match connect(&Identity::generate(), std::slice::from_ref(&big.to_string())).await {
        Ok(c) => c,
        Err(e) => {
            state.read_err = Some(format!("connect: {e}"));
            return state;
        }
    };
    let events = match reader
        .fetch(Filter::new().author(owner.public_key()).kind(Kind::from_u16(KIND_LISTING)), RELAY_TIMEOUT)
        .await
    {
        Ok(evs) => evs,
        Err(e) => {
            state.read_err = Some(format!("author fetch: {e}"));
            let _ = reader.disconnect().await;
            return state;
        }
    };
    let _ = reader.disconnect().await;

    // Group by d-tag, newest per d — the same newest-wins discipline `render_slug_family` applies.
    let mut by_d: std::collections::HashMap<String, Vec<Event>> = std::collections::HashMap::new();
    for ev in events {
        if ev.created_at > now_secs() + 60 {
            continue; // a future-dated stray; newest-wins below must not be poisoned by it
        }
        if let Some(d) = ev.tags.identifier() {
            if d == slug || d.starts_with(&part_prefix) {
                by_d.entry(d.to_string()).or_default().push(ev);
            }
        }
    }
    let mut d_tags: Vec<(String, Event)> = by_d
        .into_iter()
        .map(|(d, mut group)| {
            group.sort_by_key(|e| e.created_at.as_u64());
            (d, group.pop().unwrap())
        })
        .collect();
    d_tags.sort();

    for (d, ev) in &d_tags {
        state.d_tags_seen.push(d.clone());
        state.parts_seen += 1;
        // Decrypt with the same production parser the fetch path uses (re-verifies the signature).
        match parse_listing_event(ev, key) {
            Ok((_slug, json)) => {
                let v: Value = match serde_json::from_str(&json) {
                    Ok(v) => v,
                    Err(_) => {
                        state.fingerprints_seen.push((d.clone(), None, None));
                        continue;
                    }
                };
                let sfp = v.get("snapshot_fingerprint").and_then(Value::as_str).map(str::to_string);
                let tfp = v.get("teaser_fingerprint").and_then(Value::as_str).map(str::to_string);
                if sfp.as_deref() == Some(fp) {
                    state.snapshot_fp_matches = true;
                }
                if tfp.as_deref() == Some(fp) {
                    state.teaser_fp_matches = true;
                }
                state.fingerprints_seen.push((d.clone(), sfp, tfp));
            }
            Err(e) => {
                // Undecryptable = an event that is not part of this family's keying (or a corrupt
                // one) — record it as a payload with no fingerprints rather than aborting the read.
                let _ = e;
                state.fingerprints_seen.push((d.clone(), None, None));
            }
        }
    }
    if state.d_tags_seen.is_empty() {
        state.read_err = Some("author fetch returned no events for this slug's family".to_string());
    }

    // The ungated counterpart of the call under test, over the SAME restricted relay list: if this
    // renders while the gated call returned None, the gate (fingerprint) refused the family — (c).
    // If this fails too, the failure is upstream of the gate — (a)/(b)/(d) — with the error naming
    // which (a fetch error = (b); a Split/render error = (d); "no listing found" = (a)).
    let browser = connect(&Identity::generate(), std::slice::from_ref(&big.to_string())).await;
    state.ungated = match browser {
        Ok(b) => {
            let r = fetch_full_listing_from(
                &b,
                &owner.public_key(),
                slug,
                key,
                std::slice::from_ref(&big.to_string()),
                RELAY_TIMEOUT,
            )
            .await;
            let _ = b.disconnect().await;
            match r {
                Ok(rendered) => Ok(rendered.entries.len()),
                Err(e) => Err(e.to_string()),
            }
        }
        Err(e) => Err(format!("connect: {e}")),
    };
    state
}

/// Wall-clock seconds (for the future-dated-stray guard above).
fn now_secs() -> Timestamp {
    Timestamp::now()
}

// ---------------------------------------------------------------------------
// Unit tests for the pure helpers (no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_listing_carries_the_fingerprint_and_entries() {
        let s = full_listing("slug", 3, "deadbeef");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["snapshot_fingerprint"], "deadbeef");
        assert_eq!(v["entries"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn teaser_helper_builds_the_expected_tags() {
        let t = teaser("name", vec!["a".into()], vec!["video".into()]);
        assert_eq!(t.display_name, "name");
        assert_eq!(t.tags, vec!["a".to_string()]);
        assert_eq!(t.content_types, vec!["video".to_string()]);
    }

    /// D4 must publish its full family through the PRODUCTION teaser stamp
    /// (`commands::collection::stamp_teaser_fingerprint`) — the same step the big-relay arm of the
    /// real publish path performs. The staleness gate (`full_supersedes`, hb-net browse.rs) compares
    /// `teaser_fingerprint` — audit #25 / QURATOR-123: "the only value a teaser and a full carrier
    /// can still share" — and that field exists only where the stamp put it. D4 used to hand-build
    /// its `listing_json` and call `publish_listing_to` directly, omitting the stamp: the family
    /// genuinely lacked the field, `family_teaser_fingerprint` read `None`, and the gate correctly
    /// refused it. The third instance here of the WAN harness re-implementing a production path and
    /// omitting a step (`sanitize_node_addr` 2026-08-27; `approve_request` 2026-09-01) — without a
    /// guard the omission comes back with the next fixture edit. A drift guard, not integration
    /// coverage — the `suite_cap.rs` precedent: it fails loudly on re-divergence.
    ///
    /// MUTATION (P-10, resolve by containing function): in `d4_bigrelay_cross_region`, revert the
    /// stamped publish to the bare fixture — replace the `crate::commands::collection::stamp_teaser_fingerprint(&full_listing(...))`
    /// expression with plain `full_listing(&slug, 1300, &fp)` (delete the surrounding `?`-mapped
    /// call, keep the `String` binding) — and this test reds: the sliced body no longer contains
    /// the call. The mutated file still COMPILES (both forms bind a `String`), so the red is a
    /// result, not a build failure. Comments are stripped first, so documenting this rule cannot
    /// satisfy it.
    #[test]
    fn d4_publishes_through_the_production_teaser_stamp() {
        let src = include_str!("suite_wan_d.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let sig = "async fn d4_bigrelay_cross_region(";
        let at = code.find(sig).expect("d4_bigrelay_cross_region not found");
        let end = code[at..].find("\n}").expect("d4_bigrelay_cross_region must terminate") + at;
        let body = &code[at..end];

        assert!(
            body.contains("stamp_teaser_fingerprint("),
            "d4_bigrelay_cross_region must stamp its full family via the production \
             stamp_teaser_fingerprint before publishing — the gate reads teaser_fingerprint, \
             which only the stamp derives. Publishing a bare hand-built fixture is how D4 failed \
             as a phantom product defect (QURATOR-169)."
        );
    }

    /// The half the call-site guard above CANNOT see: that the stamp actually PRODUCES a digest.
    ///
    /// `stamp_teaser_fingerprint` returns the listing UNCHANGED, with no error, when
    /// `truncate_listing`'s kept entries do not deserialize as a `DirectoryItem` tree — the digest
    /// is underivable and there is nothing to stamp. `DirectoryItem` requires `item_type`, so a
    /// fixture emitting bare `{"name": …}` objects silently produces no `teaser_fingerprint` at all.
    ///
    /// That is not hypothetical: it is what the first fix for QURATOR-169 did. The call-site guard
    /// passed — the call WAS there — while the published family still carried `teaser_fp=<none>` and
    /// D4 still failed. A guard that pins a call pins nothing about its effect.
    ///
    /// MUTATION (P-10, resolve by containing function): in `full_listing`, drop the `"item_type"`
    /// line from the entry map. It still compiles, and this test reds because the stamp goes back to
    /// returning its input unchanged.
    #[test]
    fn the_stamp_actually_derives_a_teaser_digest_for_this_fixture() {
        let raw = full_listing("stamp-effect", 1300, "deadbeef");
        let stamped = crate::commands::collection::stamp_teaser_fingerprint(&raw)
            .expect("the stamp must not error on this fixture");
        let v: Value = serde_json::from_str(&stamped).expect("stamped listing must be JSON");

        // Precondition, loud: the fixture must actually exceed the budget, or the stamp is a
        // legitimate no-op and the assertion below would be vacuous.
        assert!(
            hb_net::truncate_listing(&raw, LISTING_MAX_BYTES).unwrap().truncated,
            "precondition: the fixture must truncate, or there is nothing for the stamp to derive"
        );

        let tf = v.get("teaser_fingerprint").and_then(Value::as_str);
        assert!(
            tf.is_some_and(|t| !t.is_empty()),
            "the stamp produced NO teaser_fingerprint — the gate compares that field, so the \
             published family would be refused. Entries must deserialize as DirectoryItem \
             (item_type is required) or the digest is underivable and the stamp silently no-ops."
        );
    }
}
