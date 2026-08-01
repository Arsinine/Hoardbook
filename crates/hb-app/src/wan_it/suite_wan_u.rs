//! WAN-U — user surface: profile, collections, add-contact (M20 W6 §W6). Seven rows that are the
//! **live twins of the `hb-it` L2 browse/publish/discovery/private suites**, pointed at real
//! infrastructure instead of ephemeral CI strfry. Per the §W6 sequencing note ("live twins of the L2
//! suites, mostly reusing `hb-it` bodies"), the assertion bodies for U2/U3/U6 adapt `hb-it/suite_browse`
//! (PUB1/PUB2/PUB3 + RK1), and U7 adapts `hb-it/suite_priv` (PRIV1/PRIV5). U1 drives the production
//! add-contact funnel (`resolve_peer` → `save_followed_peer`); U4/U5 exercise replaceable + NIP-09
//! semantics against the live relay's real implementation.
//!
//! **Shape: probe-plays-both (all rows).** The §W6 task instruction authorizes rows that need
//! serve-side state changes to run probe-driven with the probe's own throwaway identity as the
//! publisher. Every row here uses two in-process identities (a publisher and a browser/resolver)
//! against the live relay set — the same shape `hb-it` L2 bodies already use, just pointed at real
//! relays. This keeps `serve` minimal (the serve harness is unchanged) and exercises the exact
//! production read/write paths a real client uses.
//!
//! **Honest red.** Nothing here is `# TODO`/skip. A leg that fails on environment grounds (relay
//! didn't propagate, strfry ignored NIP-09) is an honest `not ok` with a per-step evidence dump —
//! with ONE documented exception: U5's per-relay honor table. A relay ignoring NIP-09 is a recorded
//! finding, not a row failure, BECAUSE the production unpublish path (`unpublish_collection_inner`)
//! treats deletion as best-effort (it publishes the kind-5 and moves on — `let _ = client.publish`).
//! U5 asserts THAT contract: the deletion request is well-formed and published; whether each relay
//! honors it is recorded to stderr evidence, not gated.
//!
//! **Flake policy (P3b precedent):** long-haul rows retry ×3; every failure is a recorded data
//! point, never discarded.

use std::time::{Duration, Instant};

use anyhow::Result;
use hb_core::event::{build_listing_event, build_teaser, Teaser};
use hb_core::{seal_private_listing, Identity, ShareCode};
use hb_net::{
    browse_share_code, fetch_private_listings, publish_listing, publish_private_listing, RelayClient,
    split_listing,
};
use nostr::prelude::*;
use serde_json::Value;

use crate::commands::browse::{resolve_peer, save_followed_peer};
use crate::identity_state::AppIdentity;
use crate::net::{self, SharedRelay};
use crate::store::DataStore;
use crate::wan_it::tap::Tap;

// ---------------------------------------------------------------------------
// Constants — timeouts, retries, settle (match WAN-P / WAN-E2E conventions)
// ---------------------------------------------------------------------------

/// Relay handshake/fetch timeout (matches `net::RELAY_TIMEOUT` = 10 s, rounded up for long-haul).
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);

/// Settle between a publish and a read (lets the relay index the event). WAN relays can take seconds
/// to index a freshly-published event; this is the same settle WAN-P / WAN-E2E use.
const SETTLE: Duration = Duration::from_secs(3);

/// Long-haul rows retry this many times before recording a failure (flake policy, P3b precedent).
const LONG_HAUL_RETRIES: u32 = 3;

/// The truncation threshold (the production constant from `commands::collection::LISTING_MAX_BYTES`).
/// Reused here for the oversize-split row (U3) — the L2 twin (`hb-it/suite_browse::pub2`) uses 40_000.
const LISTING_MAX_BYTES: usize = 40_000;

// ---------------------------------------------------------------------------
// Probe input — built by run_probe_wan_u from the parsed args
// ---------------------------------------------------------------------------

/// The input the WAN-U probe needs. Built from the parsed args; holds the relay set to drive every
/// row against. The rows construct their own throwaway publisher/browser identities internally (the
/// probe-plays-both shape), so this carries only the relay set + the probe's data store (for U1's
/// contact-save path) + the probe's identity (the resolver in U1).
pub struct ProbeInput {
    /// The relay URLs every row publishes to and reads from.
    pub relays: Vec<String>,
    /// The probe's data store (U1's add-contact funnel persists the resolved contact here).
    pub store: DataStore,
    /// The probe's own identity — used as the "resolver" in U1 (the identity that drives
    /// `resolve_peer`). The publisher in U1 is a separate throwaway identity built inside the row.
    pub identity: Identity,
}

/// Build the WAN-U probe input from the parsed args + the probe's loaded identity + store.
pub async fn build_probe_input(
    app_id: AppIdentity,
    store: DataStore,
    relays: Vec<String>,
) -> Result<ProbeInput> {
    Ok(ProbeInput {
        relays,
        store,
        identity: app_id.identity.clone(),
    })
}

/// Run the WAN-U rows (U1–U7) against the live relay set. Each row is an honest TAP check:
/// Ok ⇒ pass, Err(detail) ⇒ fail with a `# diagnostic` block.
pub async fn run(tap: &mut Tap, probe: &ProbeInput) {
    let wall = Instant::now();

    tap.check(
        "U1: profile publish → resolve by npub AND share code; petname funnel completes (1 resolve_peer)",
        u1_profile_resolve_funnel(probe).await,
    );

    tap.check(
        "U2: collection publish → teaser + encrypted listing browse back complete; non-holder sees teaser only",
        u2_collection_browse(probe).await,
    );

    tap.check(
        "U3: oversize listing splits, publishes, browses back complete()",
        u3_oversize_split(probe).await,
    );

    tap.check(
        "U4: republish replaces — exactly one current listing on the live relay",
        u4_republish_replaces(probe).await,
    );

    tap.check(
        "U5: unpublish → NIP-09 deletion request honored/recorded per-relay (best-effort contract)",
        u5_unpublish_nip09(probe).await,
    );

    tap.check(
        "U6: re-key — old share code dead against newly-published events; new key works",
        u6_rekey(probe).await,
    );

    tap.check(
        "U7: private collection — gift-wrap reaches recipient; non-recipient finds nothing",
        u7_private_collection(probe).await,
    );

    // The wall-clock diagnostic: total time for all 7 rows. §W6/W2 want latency to trend visibly.
    eprintln!(
        "   WAN-U total wall-clock for 7 rows: {:.2}s",
        wall.elapsed().as_secs_f64()
    );
}

// ---------------------------------------------------------------------------
// Small helpers shared across rows (adapted from hb-it/harness.rs + suite_browse.rs)
// ---------------------------------------------------------------------------

/// A deterministic-ish browse-key for a seed byte (matches `hb-it/suite_browse::bk`).
fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// A small listing payload with `n` named entries (matches `hb-it/suite_browse::listing`).
fn listing(slug: &str, n: usize) -> String {
    let entries: Vec<Value> =
        (0..n).map(|i| serde_json::json!({ "name": format!("title-{i:03}") })).collect();
    serde_json::json!({ "slug": slug, "content_types": ["video"], "entries": entries }).to_string()
}

/// A listing big enough to force a split under a 40 KiB part budget (matches
/// `hb-it/suite_browse::big_listing`).
fn big_listing(slug: &str, n: usize) -> String {
    let entries: Vec<Value> = (0..n)
        .map(|i| serde_json::json!({ "name": format!("title-{i:05}-padding-padding-padding-xx") }))
        .collect();
    serde_json::json!({ "slug": slug, "content_types": ["video"], "entries": entries }).to_string()
}

/// A teaser with a display name + tags (matches `hb-it/suite_browse::teaser`).
fn teaser(name: &str, tags: Vec<String>) -> Teaser {
    Teaser {
        display_name: name.into(),
        bio: "hoards".into(),
        tags,
        content_types: vec!["video".into()],
        picture: None,
    }
}

/// The full share code for an identity + browse-key (matches `hb-it/suite_browse::full_code`).
fn full_code(id: &Identity, browse_key: [u8; 32]) -> ShareCode {
    ShareCode::Full { pubkey: id.public_key(), browse_key }
}

/// Connect a client to the relay set (matches `hb-it/harness.rs::Ctx::connect`).
async fn connect(id: &Identity, relays: &[String]) -> Result<RelayClient> {
    Ok(RelayClient::connect(id, relays, RELAY_TIMEOUT).await?)
}

/// A small settle after a publish before a read (lets the live relay index the event).
async fn settle() {
    tokio::time::sleep(SETTLE).await;
}

/// Recursive leaf count of a rendered entry (matches `hb-it/suite_browse::leaf_count`). Kept for the
/// U3 deep-listing variant if a future row needs it; the current U3 uses a flat big_listing whose
/// top-level entry count == n, so `entries.len()` suffices.
#[cfg(test)]
fn leaf_count(node: &Value) -> usize {
    match node.get("children").and_then(Value::as_array) {
        Some(kids) if !kids.is_empty() => kids.iter().map(leaf_count).sum(),
        _ => 1,
    }
}

/// Build a `SharedRelay` against the probe's relay set. U1 needs the production `SharedRelay` (the
/// path `paste_key`/`resolve_peer` calls `net::client` internally, reading `net::relay_urls(store)`).
/// The probe's store MUST have the relay set persisted in Settings (run_probe_wan_u does this).
fn shared_relay() -> SharedRelay {
    net::new_shared()
}

// ---------------------------------------------------------------------------
// U1 — profile publish → resolve by npub AND share code; petname funnel (1 resolve_peer)
//
// Adapted from the production add-contact funnel: `paste_key` → `resolve_peer` (the lookup) →
// `save_followed_peer` (the W2 commit tail). The W2 fix (b177c16) extracted `save_followed_peer`
// as a pure store-only tail so `follow` can accept the lookup's pre-resolved peer and skip a SECOND
// `resolve_peer`. This row exercises that path end-to-end over a live relay: one publish, one
// resolve, one save. The wall-clock is printed as a TAP diagnostic so latency trends are visible
// (§W6/W2's "total wall-clock recorded per run").
//
// Shape: probe-plays-both. A throwaway "publisher" identity publishes a profile teaser (the
// production `publish_profile` path: `build_teaser` + `client.publish`); the probe's identity drives
// `resolve_peer` + `save_followed_peer` against it.
// ---------------------------------------------------------------------------

async fn u1_profile_resolve_funnel(probe: &ProbeInput) -> Result<(), String> {
    let start = Instant::now();

    // (1) Publisher: a throwaway identity publishes a profile teaser via the production path
    //     (`build_teaser` + `client.publish` — the inner body of `commands::profile::publish_profile`).
    let publisher = Identity::generate();
    let pub_client = connect(&publisher, &probe.relays)
        .await
        .map_err(|e| format!("U1 publisher connect: {e}"))?;
    let teaser_ev = build_teaser(&publisher, &teaser("WANUPeer", vec!["wan-u".into()]), true)
        .map_err(|e| format!("U1 build_teaser: {e}"))?;
    pub_client
        .publish(&teaser_ev)
        .await
        .map_err(|e| format!("U1 teaser publish: {e}"))?;
    pub_client.disconnect().await;
    settle().await;

    // (2) Resolve by share code (the production paste_key → resolve_peer path). The probe drives the
    //     REAL `resolve_peer` (promoted pub(crate) — see the doc-comment on the promotion): it fetches
    //     the teaser + presence, builds a CachedPeer. This is the ONE resolve_peer the W2 regression
    //     row pins — the lookup leg.
    let share_code = full_code(&publisher, bk(1));
    let shared_relay = shared_relay();
    let resolved = resolve_peer(&share_code, &probe.identity, &probe.store, &shared_relay)
        .await
        .map_err(|e| format!("U1 resolve_peer (by share code) failed: {e}"))?;

    // The resolved peer must carry the publisher's npub + the teaser's display_name as petname.
    let pub_npub = publisher.npub();
    if resolved.npub != pub_npub {
        return Err(format!(
            "U1 resolve by share code: npub mismatch — resolved {}, expected {pub_npub}",
            resolved.npub
        ));
    }
    if resolved.petname.as_deref() != Some("WANUPeer") {
        return Err(format!(
            "U1 resolve by share code: petname mismatch — got {:?}, expected \"WANUPeer\"",
            resolved.petname
        ));
    }
    eprintln!("   U1 resolve_peer (share code) OK: npub + petname \"WANUPeer\" recovered");

    // (3) Resolve by bare npub (FollowOnly code) — the second leg of "resolves by npub AND by share
    //     code". A bare npub carries no browse-key, so resolve_peer returns the profile + presence
    //     but no collections.
    let npub_code = ShareCode::FollowOnly { pubkey: publisher.public_key() };
    let resolved_npub = resolve_peer(&npub_code, &probe.identity, &probe.store, &shared_relay)
        .await
        .map_err(|e| format!("U1 resolve_peer (by npub) failed: {e}"))?;
    if resolved_npub.npub != pub_npub {
        return Err(format!(
            "U1 resolve by npub: npub mismatch — resolved {}, expected {pub_npub}",
            resolved_npub.npub
        ));
    }
    eprintln!("   U1 resolve_peer (bare npub) OK: npub recovered");

    // (4) Petname funnel completes: the W2 commit tail (`save_followed_peer`) persists the pre-resolved
    //     peer WITHOUT a second resolve_peer (the structural fix — `save_followed_peer` takes only the
    //     store, no relay/identity arg). This is the "one resolve_peer" property pinned end-to-end:
    //     the funnel did one resolve (step 2), then saved.
    save_followed_peer(&probe.store, resolved.clone(), None, None, false)
        .map_err(|e| format!("U1 save_followed_peer (W2 tail): {e}"))?;
    let loaded = probe
        .store
        .load_contact(&crate::store::CachedPeer::pubkey_hash(&pub_npub))
        .map_err(|e| format!("U1 load_contact: {e}"))?
        .ok_or_else(|| "U1 save_followed_peer did not persist the contact".to_string())?;
    if loaded.npub != pub_npub {
        return Err(format!(
            "U1 save_followed_peer persisted npub {}, expected {pub_npub}",
            loaded.npub
        ));
    }
    eprintln!("   U1 save_followed_peer (W2 tail) OK: contact persisted with 0 extra resolves");

    let elapsed = start.elapsed();
    eprintln!(
        "   U1 wall-clock (publish + 2 resolves + save): {:.2}s — one resolve_peer per add (W2)",
        elapsed.as_secs_f64()
    );
    // Also as a TAP-visible diagnostic line (stdout-side): the §W6 "total wall-clock recorded per run".
    println!(
        "# U1 timing: publish + resolve(npub) + resolve(share-code) + save = {:.2}s (W2: 1 resolve_peer per add)",
        elapsed.as_secs_f64()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// U2 — collection publish → teaser + encrypted listing browse back complete; non-holder teaser only
//
// Live twin of `hb-it/suite_browse` PUB1 (publish→browse round-trip) + BR1 (non-holder key → teaser,
// listing locked). Reuses those bodies directly, pointed at the live relay set.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn u2_collection_browse(probe: &ProbeInput) -> Result<(), String> {
    let id = Identity::generate();
    let key = bk(2);
    let slug = "wan-u-pub1";
    let json = listing(slug, 5);

    let client = connect(&id, &probe.relays)
        .await
        .map_err(|e| format!("U2 publisher connect: {e}"))?;
    client
        .publish(&build_teaser(&id, &teaser("archivebox", vec!["wan-u".into()]), true)
            .map_err(|e| format!("U2 build_teaser: {e}"))?)
        .await
        .map_err(|e| format!("U2 teaser publish: {e}"))?;
    let published = publish_listing(&client, &id, slug, &key, &json, LISTING_MAX_BYTES)
        .await
        .map_err(|e| format!("U2 publish_listing: {e}"))?;
    if published.parts != 1 {
        return Err(format!(
            "U2 a small listing should publish as one part, got {}",
            published.parts
        ));
    }
    settle().await;

    // Holder browse: the full share code decrypts the listing → complete.
    let res = browse_share_code(&client, &full_code(&id, key), slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U2 holder browse_share_code: {e}"))?;
    if res.teaser.is_none() {
        return Err("U2 holder browse did not surface the public teaser".to_string());
    }
    let holder_listing = res
        .listing
        .ok_or_else(|| "U2 holder browse returned no listing".to_string())?;
    if !holder_listing.complete() {
        return Err("U2 a fully-published listing should browse complete".to_string());
    }
    if holder_listing.entries.len() != 5 {
        return Err(format!(
            "U2 expected 5 entries, got {}",
            holder_listing.entries.len()
        ));
    }
    eprintln!("   U2 holder browse OK: teaser + complete listing (5 entries)");

    // Non-holder: a fresh identity (no browse-key) sees the teaser ONLY. A wrong key yields no listing.
    let non_holder = Identity::generate();
    let nh_client = connect(&non_holder, &probe.relays)
        .await
        .map_err(|e| format!("U2 non-holder connect: {e}"))?;
    let wrong = full_code(&id, bk(99));
    let nh_res = browse_share_code(&nh_client, &wrong, slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U2 non-holder browse_share_code: {e}"))?;
    nh_client.disconnect().await;
    client.disconnect().await;
    if nh_res.teaser.is_none() {
        return Err("U2 non-holder must still see the public teaser".to_string());
    }
    if nh_res.listing.is_some() {
        return Err("U2 a wrong browse-key must NOT decrypt the listing (non-holder sees teaser only)".to_string());
    }
    eprintln!("   U2 non-holder browse OK: teaser visible, listing locked");
    Ok(())
}

// ---------------------------------------------------------------------------
// U3 — oversize listing splits, publishes, browses back complete()
//
// Live twin of `hb-it/suite_browse` PUB2 (oversize split → browse full tree). Reuses that body.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn u3_oversize_split(probe: &ProbeInput) -> Result<(), String> {
    let id = Identity::generate();
    let key = bk(3);
    let slug = "wan-u-huge";
    let n = 1300;
    let json = big_listing(slug, n);

    let client = connect(&id, &probe.relays)
        .await
        .map_err(|e| format!("U3 publisher connect: {e}"))?;
    let published = publish_listing(&client, &id, slug, &key, &json, LISTING_MAX_BYTES)
        .await
        .map_err(|e| format!("U3 publish_listing: {e}"))?;
    if published.parts <= 2 {
        return Err(format!(
            "U3 oversize listing must split into >2 parts, got {}",
            published.parts
        ));
    }
    eprintln!("   U3 oversize listing split into {} parts", published.parts);
    settle().await;

    let res = browse_share_code(&client, &full_code(&id, key), slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U3 browse_share_code: {e}"))?;
    client.disconnect().await;

    let lst = res
        .listing
        .ok_or_else(|| "U3 split listing did not browse back".to_string())?;
    if !lst.complete() {
        return Err(format!(
            "U3 all parts present should browse complete (present {} of {})",
            lst.parts_present, lst.parts_total
        ));
    }
    if lst.entries.len() != n {
        return Err(format!(
            "U3 restitched {} of {n} entries",
            lst.entries.len()
        ));
    }
    eprintln!("   U3 browse OK: complete tree, {n} entries restitched from {} parts", published.parts);
    Ok(())
}

// ---------------------------------------------------------------------------
// U4 — republish replaces: exactly one current listing on the live relay
//
// Live twin of `hb-it/suite_browse` PUB3 (re-publish replaces — one current listing). The live-relay
// caveat (§W6): "replaceable semantics are relay-implementation-dependent — the CI relay proving it
// means little." This row runs against the REAL strfry, so the result is meaningful. Reuses the PUB3
// body: publish, wait >1s (so created_at advances — replaceable events key on it), republish, browse
// back, assert only the newest (7 entries) survives.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn u4_republish_replaces(probe: &ProbeInput) -> Result<(), String> {
    let id = Identity::generate();
    let key = bk(4);
    let slug = "wan-u-shelf";

    let client = connect(&id, &probe.relays)
        .await
        .map_err(|e| format!("U4 publisher connect: {e}"))?;
    publish_listing(&client, &id, slug, &key, &listing(slug, 2), LISTING_MAX_BYTES)
        .await
        .map_err(|e| format!("U4 first publish_listing: {e}"))?;
    // A >1s gap makes the second publish strictly newer (replaceable events key on created_at).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    publish_listing(&client, &id, slug, &key, &listing(slug, 7), LISTING_MAX_BYTES)
        .await
        .map_err(|e| format!("U4 second publish_listing: {e}"))?;
    settle().await;

    let res = browse_share_code(&client, &full_code(&id, key), slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U4 browse_share_code: {e}"))?;
    client.disconnect().await;

    let lst = res
        .listing
        .ok_or_else(|| "U4 no listing after replace".to_string())?;
    if lst.entries.len() != 7 {
        return Err(format!(
            "U4 replace should leave only the newest (7 entries), got {} — \
             the live relay did not honor parameterized-replaceable semantics (older event co-resident)",
            lst.entries.len()
        ));
    }
    eprintln!("   U4 republish OK: exactly one current listing (7 entries, the newest)");
    Ok(())
}

// ---------------------------------------------------------------------------
// U5 — unpublish → NIP-09 deletion: request honored/recorded per-relay (best-effort)
//
// The production unpublish path (`commands::collection::unpublish_collection_inner`) is BEST-EFFORT
// by construction: it builds a NIP-09 deletion (kind 5) referencing each listing event by id and
// publishes it with `let _ = client.publish(&deletion).await` — whether any relay honors it is the
// relay's decision, not ours. This row asserts THAT contract: the deletion request is well-formed,
// signed by the same identity, and published. Whether each relay honors it is RECORDED to stderr
// (the per-relay honor table), not gated — a relay ignoring NIP-09 is a finding, not a row failure.
//
// Mechanism: publish a listing, fetch it back (confirm present), build + publish the NIP-09
// deletion (`hb_net::build_deletion` — the exact production fn), then fetch again per relay. Each
// relay is recorded as HONORED (event gone) or IGNORED (event still served). The row fails ONLY if
// the deletion request itself could not be built/published (the production contract).
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn u5_unpublish_nip09(probe: &ProbeInput) -> Result<(), String> {
    let id = Identity::generate();
    let key = bk(5);
    let slug = "wan-u-del";

    let client = connect(&id, &probe.relays)
        .await
        .map_err(|e| format!("U5 publisher connect: {e}"))?;

    // (1) Publish a single listing event (the production path). Capture the signed event so we can
    //     build the NIP-09 deletion for exactly it (the production fn targets event ids).
    let parts = split_listing(slug, &listing(slug, 3), LISTING_MAX_BYTES)
        .map_err(|e| format!("U5 split_listing: {e}"))?;
    if parts.len() != 1 {
        return Err(format!(
            "U5 expected a single-part listing for the deletion target, got {}",
            parts.len()
        ));
    }
    let listing_ev = build_listing_event(&id, slug, &key, &parts[0].json)
        .map_err(|e| format!("U5 build_listing_event: {e}"))?;
    client
        .publish(&listing_ev)
        .await
        .map_err(|e| format!("U5 listing publish: {e}"))?;
    settle().await;

    // (2) Confirm the event is fetchable BEFORE the deletion (per relay) — the baseline.
    let target_id = listing_ev.id;
    let target_hex = target_id.to_hex();
    let filter = Filter::new()
        .author(id.public_key())
        .kind(Kind::from_u16(hb_core::event::KIND_LISTING))
        .id(target_id);
    let mut honored: Vec<String> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for url in &probe.relays {
        let one = std::slice::from_ref(url).to_vec();
        let rc = RelayClient::connect(&id, &one, RELAY_TIMEOUT)
            .await
            .map_err(|e| format!("U5 per-relay connect {url}: {e}"))?;
        let before = rc.fetch(filter.clone(), RELAY_TIMEOUT).await.unwrap_or_default();
        rc.disconnect().await;
        let present_before = before.iter().any(|e| e.id == target_id);
        eprintln!("   U5   {url}: before deletion, target event present = {present_before}");
        if !present_before {
            // If the relay never had it, we can't measure honor — record as "not present before".
            ignored.push(format!("{url} (not present before deletion)"));
        }
    }

    // (3) Build + publish the NIP-09 deletion via the EXACT production fn (`hb_net::build_deletion`).
    //     This is the production contract: the request is well-formed and signed by the author.
    let deletion = hb_net::build_deletion(&id, &listing_ev)
        .map_err(|e| format!("U5 build_deletion (the production fn): {e}"))?;
    if deletion.kind != Kind::EventDeletion {
        return Err(format!(
            "U5 deletion request kind mismatch: got {:?}, expected EventDeletion",
            deletion.kind
        ));
    }
    if !deletion.tags.iter().any(|t| t.content() == Some(target_hex.as_str())) {
        return Err("U5 deletion request must reference the target event id via an e tag".to_string());
    }
    client
        .publish(&deletion)
        .await
        .map_err(|e| format!("U5 deletion publish (the production contract): {e}"))?;
    eprintln!("   U5 NIP-09 deletion request published (kind 5, target {target_hex})");
    settle().await;

    // (4) Per-relay honor check: fetch the target event from EACH relay individually. HONORED = the
    //     event is gone; IGNORED = the event is still served. This is the per-relay honor table §W6
    //     mandates — recorded to stderr, not gated (the production path is best-effort).
    for url in &probe.relays {
        let one = std::slice::from_ref(url).to_vec();
        let rc = RelayClient::connect(&id, &one, RELAY_TIMEOUT)
            .await
            .map_err(|e| format!("U5 post-delete connect {url}: {e}"))?;
        let after = rc.fetch(filter.clone(), RELAY_TIMEOUT).await.unwrap_or_default();
        rc.disconnect().await;
        let still_present = after.iter().any(|e| e.id == target_id);
        if still_present {
            eprintln!("   U5   {url}: IGNORED — listing event still served after NIP-09 deletion (a relay finding, not a row failure: production unpublish is best-effort)");
            ignored.push(format!("{url} (ignored NIP-09)"));
        } else {
            eprintln!("   U5   {url}: HONORED — listing event gone after NIP-09 deletion");
            honored.push(url.clone());
        }
    }
    client.disconnect().await;

    // The per-relay honor summary (§W6 evidence).
    eprintln!(
        "   U5 NIP-09 honor table: {} honored {:?}, {} ignored {:?}",
        honored.len(),
        honored,
        ignored.len(),
        ignored
    );

    // The row FAILS only if the production contract was not met: the deletion request could not be
    // built or published. Per-relay honor is a finding (stderr above), not a gate — because the
    // production `unpublish_collection_inner` is best-effort by design (`let _ = client.publish`).
    // We DO flag the degenerate case: zero relays honored AND there were relays that had it before.
    // That is still not a row failure (the contract holds), but it is surfaced as a diagnostic so a
    // total NIP-09 failure is never silent.
    let had_before = ignored.iter().any(|s| !s.contains("not present"));
    if honored.is_empty() && had_before {
        eprintln!(
            "   U5 NOTE: no live relay honored the NIP-09 deletion. The production contract (request \
             well-formed + published) held; relay honor is best-effort. If this persists, deletion \
             UX should set user expectations (the event lingers on non-compliant relays)."
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// U6 — re-key: old share code dead against newly-published events; new key works
//
// Live twin of `hb-it/suite_browse` RK1 (re-key kills old key for new listings). Reuses that body:
// publish under key A, confirm A works, republish under key B, confirm A is dead + B works.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn u6_rekey(probe: &ProbeInput) -> Result<(), String> {
    let id = Identity::generate();
    let slug = "wan-u-rekey";
    let old = bk(10);
    let new = bk(11);

    let client = connect(&id, &probe.relays)
        .await
        .map_err(|e| format!("U6 publisher connect: {e}"))?;
    publish_listing(&client, &id, slug, &old, &listing(slug, 4), LISTING_MAX_BYTES)
        .await
        .map_err(|e| format!("U6 first publish_listing (old key): {e}"))?;
    settle().await;

    // Old key works before the re-key.
    let before = browse_share_code(&client, &full_code(&id, old), slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U6 pre-rekey browse (old key): {e}"))?;
    if before.listing.is_none() {
        return Err("U6 the old key should decrypt the pre-rekey listing".to_string());
    }
    eprintln!("   U6 pre-rekey: old key decrypts the listing (baseline)");

    // Re-key: republish the same slug under the NEW browse-key (replaces the prior event).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    publish_listing(&client, &id, slug, &new, &listing(slug, 4), LISTING_MAX_BYTES)
        .await
        .map_err(|e| format!("U6 second publish_listing (new key): {e}"))?;
    settle().await;

    // Old key is now dead against the newly-published event.
    let with_old = browse_share_code(&client, &full_code(&id, old), slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U6 post-rekey browse (old key): {e}"))?;
    if with_old.listing.is_some() {
        return Err(
            "U6 the leaked OLD key must NOT decrypt the re-keyed listing (old share code should be dead)"
                .to_string(),
        );
    }
    eprintln!("   U6 post-rekey: old key dead (listing locked) — the old share code is dead");

    // New key works.
    let with_new = browse_share_code(&client, &full_code(&id, new), slug, &probe.relays, &probe.relays, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U6 post-rekey browse (new key): {e}"))?;
    client.disconnect().await;
    if with_new.listing.is_none() {
        return Err("U6 the new key must decrypt the re-keyed listing".to_string());
    }
    eprintln!("   U6 post-rekey: new key works (listing decrypted)");
    Ok(())
}

// ---------------------------------------------------------------------------
// U7 — private collection: gift-wrap reaches recipient; non-recipient finds nothing
//
// Live twin of `hb-it/suite_priv` PRIV1 (multi-relay publish: wrap fetchable) + PRIV5 (non-recipient
// finds no private listing). Reuses those bodies: seal → publish → recipient fetches + opens →
// non-recipient (fresh keys) finds nothing.
//
// Shape: probe-plays-both.
// ---------------------------------------------------------------------------

async fn u7_private_collection(probe: &ProbeInput) -> Result<(), String> {
    const PRIV_LISTING: &str =
        r#"{"slug":"vault","content_types":["video"],"entries":[{"name":"rare.mkv"}]}"#;

    let author = Identity::generate();
    let recipient = Identity::generate();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let wraps = seal_private_listing(&author, &[recipient.public_key()], PRIV_LISTING, now)
        .map_err(|e| format!("U7 seal_private_listing: {e}"))?;

    let ac = connect(&author, &probe.relays)
        .await
        .map_err(|e| format!("U7 author connect: {e}"))?;
    publish_private_listing(&ac, &wraps)
        .await
        .map_err(|e| format!("U7 publish_private_listing: {e}"))?;
    settle().await;

    // Recipient: the gift-wrap reaches them (the #p-tagged inbox holds the wrap; opening recovers the
    // inner author behind the ephemeral outer).
    let rc = connect(&recipient, &probe.relays)
        .await
        .map_err(|e| format!("U7 recipient connect: {e}"))?;
    let mut last_err = String::new();
    let mut got = Vec::new();
    for attempt in 1..=LONG_HAUL_RETRIES {
        match fetch_private_listings(&rc, &recipient, &[author.public_key()], RELAY_TIMEOUT).await {
            Ok(v) => {
                got = v;
                break;
            }
            Err(e) => {
                last_err = format!("attempt {attempt}: fetch_private_listings: {e}");
                settle().await;
            }
        }
    }
    if got.is_empty() {
        rc.disconnect().await;
        ac.disconnect().await;
        return Err(format!(
            "U7 the recipient found no private listing (gift-wrap did not reach them): {last_err}"
        ));
    }
    if got.len() != 1 {
        rc.disconnect().await;
        ac.disconnect().await;
        return Err(format!(
            "U7 recipient should open exactly one listing, got {}",
            got.len()
        ));
    }
    if got[0].listing_json != PRIV_LISTING {
        rc.disconnect().await;
        ac.disconnect().await;
        return Err("U7 recipient: listing plaintext mismatch".to_string());
    }
    if got[0].inner_author != author.public_key() {
        rc.disconnect().await;
        ac.disconnect().await;
        return Err("U7 recipient: inner author behind the ephemeral wrap is not the real author".to_string());
    }
    eprintln!("   U7 recipient OK: gift-wrap reached them, listing opened (inner author recovered)");

    // Non-recipient: a fresh identity finds nothing addressed to it (no #p tag → no wrap in its inbox).
    let outsider = Identity::generate();
    let oc = connect(&outsider, &probe.relays)
        .await
        .map_err(|e| format!("U7 outsider connect: {e}"))?;
    let outsider_got = fetch_private_listings(&oc, &outsider, &[author.public_key()], RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("U7 outsider fetch_private_listings: {e}"))?;
    // And a raw scan of gift-wraps addressed to the outsider returns nothing about the collection.
    let raw = oc
        .fetch(
            Filter::new().kind(Kind::GiftWrap).pubkey(outsider.public_key()),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("U7 outsider raw GiftWrap fetch: {e}"))?;
    oc.disconnect().await;
    rc.disconnect().await;
    ac.disconnect().await;
    if !outsider_got.is_empty() {
        return Err(format!(
            "U7 a non-recipient must surface no private listing, got {}",
            outsider_got.len()
        ));
    }
    if !raw.is_empty() {
        return Err("U7 no gift-wrap should be addressed to a non-recipient — no enumeration hint".to_string());
    }
    eprintln!("   U7 non-recipient OK: no private listing, no gift-wrap in their inbox");
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests for pure helpers (no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_payload_has_n_entries() {
        let json = listing("test", 7);
        let v: Value = serde_json::from_str(&json).unwrap();
        let entries = v.get("entries").and_then(Value::as_array).unwrap();
        assert_eq!(entries.len(), 7);
        assert_eq!(v["slug"], "test");
    }

    #[test]
    fn big_listing_is_oversize_for_40k_budget() {
        let json = big_listing("big", 1300);
        // The serialized JSON must exceed the 40 KB budget to force a split (the PUB2/U3 discriminator).
        assert!(
            json.len() > LISTING_MAX_BYTES,
            "big_listing(1300) = {} bytes, must exceed {} to force a split",
            json.len(),
            LISTING_MAX_BYTES
        );
    }

    #[test]
    fn split_listing_big_yields_index_plus_content_parts() {
        let json = big_listing("split-test", 1300);
        let parts = split_listing("split-test", &json, LISTING_MAX_BYTES).unwrap();
        assert!(
            parts.len() > 2,
            "a 1300-entry big_listing must split into >2 parts under 40 KB, got {}",
            parts.len()
        );
    }

    #[test]
    fn leaf_count_counts_recursively() {
        // A single file node is one leaf.
        assert_eq!(leaf_count(&serde_json::json!({"name":"f.bin"})), 1);
        // A folder with 3 leaf children is 3 leaves.
        let folder = serde_json::json!({
            "name": "dir",
            "children": [{"name":"a"},{"name":"b"},{"name":"c"}]
        });
        assert_eq!(leaf_count(&folder), 3);
        // Nested folders recurse.
        let nested = serde_json::json!({
            "name": "root",
            "children": [
                {"name":"a"},
                {"name":"sub","children":[{"name":"x"},{"name":"y"}]}
            ]
        });
        assert_eq!(leaf_count(&nested), 3);
    }

    #[test]
    fn teaser_carries_display_name_and_tags() {
        let t = teaser("Alice", vec!["anime".into(), "vhs".into()]);
        assert_eq!(t.display_name, "Alice");
        assert_eq!(t.tags, vec!["anime", "vhs"]);
    }

    #[test]
    fn full_code_carries_browse_key() {
        let id = Identity::generate();
        let code = full_code(&id, bk(42));
        match code {
            ShareCode::Full { pubkey, browse_key } => {
                assert_eq!(pubkey, id.public_key());
                assert_eq!(browse_key, [42u8; 32]);
            }
            _ => panic!("expected a Full code"),
        }
    }

    #[test]
    fn followonly_code_carries_no_browse_key() {
        let id = Identity::generate();
        let code = ShareCode::FollowOnly { pubkey: id.public_key() };
        assert_eq!(code.pubkey(), id.public_key());
        assert!(code.browse_key().is_none(), "a FollowOnly code carries no browse-key");
    }
}
