//! GRANT (QURATOR-346) — the v5 auto-grant path, END TO END over the default relays on TWO
//! machines: a real access-request DM answered by the PRODUCTION auto-approve loop, the granted
//! key applied through the PRODUCTION receive path, and the owner's listing decrypted with it.
//!
//! Topology (operator-driven, one process per party, owner first):
//!
//! * **OWNER** — seeds + publishes a collection (`wan-grant`, 24 files ⇒ total_bytes 24), then
//!   answers access requests CONTINUOUSLY through `crate::auto_approve::run_auto_approve_loop`
//!   — the exact task `lib.rs` spawns at startup. It reads the asker's CURRENT teaser
//!   (`hb_net::fetch_peer_teaser`), decides with `hb_core::size_rule::may_read`, and on a pass
//!   mints the grant through the same body the manual click uses.
//! * **ASKER** — publishes a teaser whose `total_bytes` the operator dials with `--asker-files`
//!   (1 byte per seeded file, so 40 files ⇒ total 40 > owner's 24; 10 files ⇒ smaller). Saves the
//!   owner as a MANUAL contact with **no browse key** (the receive allowlist is hand-added
//!   contacts, and the key must arrive via the grant, never the share code), sends the
//!   production access request, then polls the production receive path.
//!
//! ## Per-step production-function map (the anti-"harness re-implements the body" table)
//!
//! | Role | Step | Production function |
//! |------|------|----------------------|
//! | both | seed the collection | `wan_it::seed_collection` → `commands::collection::scan_selective_sized` + `DataStore::save_collection_draft` |
//! | both | publish the collection | `wan_it::publish_teaser_for` → `commands::collection::prepare_listing` + `stamp_for_teaser` + `hb_net::publish_listing_capped` |
//! | both | publish the profile teaser | `commands::profile::teaser_from_profile` + `hb_core::event::build_teaser` + `RelayClient::publish` — the inner body of `publish_profile` |
//! | owner | answer asks (continuously) | `crate::auto_approve::run_auto_approve_loop` (PRODUCTION's loop) → `fetch_peer_teaser` + `size_rule::may_read` + `grant_browse_access_inner` |
//! | asker | ask | `crate::commands::chat::send_access_request_inner` (nonce, throttle, NIP-17 send) |
//! | asker | receive the grant | `crate::commands::private::apply_key_grants_inner` → `hb_net::fetch_key_grants` + the Manual-contact allowlist |
//! | asker | prove the key | `hb_net::fetch_full_listing_from` under the granted key |
//!
//! ## Honest gaps (state them, don't bury them)
//!
//! * `publish_profile` has NO `_inner` shim, so the profile-teaser publish re-composes its inner
//!   body from the production pieces above. Precedent: WAN-U's U1 teaser row documents the same
//!   composition as "the production `publish_profile` path: `build_teaser` + `client.publish`".
//! * `publish_teaser_for` publishes but does not save the store's published marker;
//!   `publish_collection_inner` does. The harness saves `save_published` after it — the marker
//!   is what `total_published_public_bytes` (the loop's `my_total`, and the teaser's
//!   `total_bytes`) counts, so without it the v5 rule sees 0 on BOTH sides and no variant can
//!   ever grant.
//!
//! ## The bounded waits, and why they are sufficient
//!
//! The owner loop polls every 5 s (`AUTO_APPROVE_POLL_INTERVAL`); one pass adds a teaser fetch,
//! a seal, a publish, and relay propagation — the positive path is seconds of work. WG1 allows
//! 90 s (18 owner poll cycles + 18 apply polls) for the key to arrive; WG2 waits 60 s
//! (12 full owner poll cycles) and asserts NO key — long past any plausible slow grant, so the
//! silence is the refusal path the v5 rule takes, not latency.
//!
//! ## Not a CI gate
//!
//! Same status as every `wan_it` suite: manual, OWNER-RUN two-machine harness, never wired into
//! CI. Compile is proven locally; the live run is the owner's.

use std::path::Path;
use std::time::Duration;

use nostr::prelude::FromBech32;

use crate::transport_state::new_shared_endpoint;
use crate::wan_it::suite_wan_carry::CarryInput;
use crate::wan_it::tap::Tap;

/// The slug both sides publish. Distinct from every other suite's so a shared relay set cannot
/// cross-contaminate one row with another's listing.
const GRANT_SLUG: &str = "wan-grant";

/// The owner's fixed tree: 24 files × 1 byte ⇒ `total_bytes` 24 (the seed files are one byte
/// each, so the size dial IS the file count). The asker's `--asker-files` picks either side.
const OWNER_FILES: usize = 24;

/// Apply-poll cadence — matches the owner loop's own 5 s poll, so neither side is the bottleneck.
const APPLY_POLL: Duration = Duration::from_secs(5);

/// WG1's bound for the key to arrive (see the header's wait rationale).
const GRANT_DEADLINE: Duration = Duration::from_secs(90);

/// WG2's bound of silence before the absence is called a refusal (see the header's rationale).
const REFUSE_WAIT: Duration = Duration::from_secs(60);

/// Settle after the asker publishes its teaser, so the owner's first poll sees it.
const TEASER_SETTLE: Duration = Duration::from_secs(3);

pub async fn run(tap: &mut Tap, role: &str, input: &CarryInput) {
    match role {
        "owner" => {
            tap.check(
                "WAN-G owner: seeded + published (total 24) + answering access requests through the PRODUCTION loop",
                run_owner(input).await,
            );
        }
        "asker" => {
            let expect = input.flag("--expect").unwrap_or("grant").to_string();
            if expect == "refuse" {
                tap.check(
                    "WG2 (refuse): a SMALLER asker gets NO key within the bounded wait",
                    run_asker(input, false).await,
                );
            } else {
                tap.check(
                    "WG1 (grant): a BIGGER asker is auto-granted and decrypts the owner's listing",
                    run_asker(input, true).await,
                );
            }
        }
        other => {
            tap.check(
                format!("GRANT: unknown --role '{other}' (expected owner|asker)"),
                Err("unknown role".to_string()),
            );
        }
    }
}

/// OWNER: seed, publish, print the identity + total facts, then serve asks forever through the
/// production loop. `--seed-dir <dir>` required. Start this role FIRST.
async fn run_owner(input: &CarryInput) -> Result<(), String> {
    let seed_dir = input
        .flag("--seed-dir")
        .ok_or_else(|| "role owner requires --seed-dir <dir>".to_string())?
        .to_string();

    super::generate_seed_tree(Path::new(&seed_dir), OWNER_FILES, 0)
        .map_err(|e| format!("generate seed tree: {e:#}"))?;
    super::seed_collection(&input.store, &input.app_id.identity, &input.app_id.browse_key, &seed_dir, GRANT_SLUG)
        .map_err(|e| format!("seed collection: {e:#}"))?;
    let published = super::publish_teaser_for(&input.store, &input.app_id.identity, &input.app_id.browse_key, GRANT_SLUG)
        .await
        .map_err(|e| format!("publish teaser: {e:#}"))?;
    // The published marker `publish_collection_inner` saves after publishing — the marker is
    // what `total_published_public_bytes` (the loop's `my_total`) counts (see header gap 2).
    input
        .store
        .save_published(GRANT_SLUG, &format!("{{\"parts\":{}}}", published.parts))
        .map_err(|e| format!("save published marker: {e}"))?;
    publish_own_teaser(input).await?;

    let my_total = crate::commands::profile::total_published_public_bytes(&input.store);
    eprintln!(
        "[wan-g] owner: collection '{GRANT_SLUG}' published ({} part(s)), total_bytes={my_total} — answering access requests via the PRODUCTION loop; kill this process when the asker reports done",
        published.parts
    );

    let shared_relay = crate::net::new_shared();
    let endpoint = new_shared_endpoint();
    crate::auto_approve::run_auto_approve_loop(input.store.clone(), input.live_identity(), shared_relay, endpoint)
        .await;
    Ok(())
}

/// ASKER: seed + publish (dialling its total with `--asker-files`), add the owner as a keyless
/// Manual contact, send the production access request, then poll the production receive path.
/// Flags: `--seed-dir <dir> --owner-npub <npub> --asker-files <n> --expect grant|refuse`.
async fn run_asker(input: &CarryInput, expect_grant: bool) -> Result<(), String> {
    let seed_dir = input
        .flag("--seed-dir")
        .ok_or_else(|| "role asker requires --seed-dir <dir>".to_string())?
        .to_string();
    let owner_npub = input
        .flag("--owner-npub")
        .ok_or_else(|| "role asker requires --owner-npub <owner npub>".to_string())?
        .to_string();
    let asker_files: usize = input
        .flag("--asker-files")
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);

    // The owner is a hand-added contact with NO browse key — the receive allowlist is Manual
    // contacts, and the key must arrive via the GRANT, never a share code.
    save_owner_contact_without_key(input, &owner_npub)?;

    super::generate_seed_tree(Path::new(&seed_dir), asker_files, 0)
        .map_err(|e| format!("generate seed tree: {e:#}"))?;
    super::seed_collection(&input.store, &input.app_id.identity, &input.app_id.browse_key, &seed_dir, GRANT_SLUG)
        .map_err(|e| format!("seed collection: {e:#}"))?;
    let published = super::publish_teaser_for(&input.store, &input.app_id.identity, &input.app_id.browse_key, GRANT_SLUG)
        .await
        .map_err(|e| format!("publish teaser: {e:#}"))?;
    input
        .store
        .save_published(GRANT_SLUG, &format!("{{\"parts\":{}}}", published.parts))
        .map_err(|e| format!("save published marker: {e}"))?;
    publish_own_teaser(input).await?;

    let me_total = crate::commands::profile::total_published_public_bytes(&input.store);
    let owner_pk: nostr::PublicKey = nostr::PublicKey::from_bech32(&owner_npub)
        .map_err(|e| format!("parse --owner-npub: {e}"))?;

    // Operator pre-check, through the SAME production pieces the answerer will read: if the file
    // counts did not produce the intended side of the rule, fail LOUDLY here with guidance
    // instead of spending a 60-90 s wait on a mis-sized fixture. The ask is sent regardless —
    // the answerer's decision is the thing under test.
    let shared_relay = crate::net::new_shared();
    let client = crate::net::client(&input.app_id.identity, &input.store, &shared_relay)
        .await
        .map_err(|e| format!("relay client: {e:#}"))?;
    let owner_total = hb_net::fetch_peer_teaser(&client, &owner_pk, crate::net::RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("fetch the owner's teaser (is the owner up?): {e}"))?
        .map(|t| t.total_bytes)
        .unwrap_or(0);
    let rule_says_grant = hb_core::size_rule::may_read(me_total, owner_total);
    eprintln!(
        "[wan-g] asker: total={me_total} ({asker_files} files) vs owner total={owner_total} — the rule says {}",
        if rule_says_grant { "GRANT" } else { "REFUSE" }
    );
    if rule_says_grant != expect_grant {
        return Err(format!(
            "fixture mismatch: --expect {} but the rule says {} (asker {me_total} vs owner {owner_total}). \
             Raise --asker-files for the grant run or lower it for the refuse run",
            if expect_grant { "grant" } else { "refuse" },
            if rule_says_grant { "grant" } else { "refuse" },
        ));
    }

    tokio::time::sleep(TEASER_SETTLE).await;
    // THE ASK — the production body: nonce, throttle, NIP-17 send (the manual command's body,
    // which is also what the automatic ask path drives).
    crate::commands::chat::send_access_request_inner(&owner_npub, &input.app_id.identity, &input.store, &shared_relay)
        .await?;

    let contact_hash = crate::store::CachedPeer::pubkey_hash(&owner_npub);
    if expect_grant {
        // WG1: poll the PRODUCTION receive path until the owner's key is applied.
        let deadline = tokio::time::Instant::now() + GRANT_DEADLINE;
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            let applied = apply_key_grants(input).await?;
            if applied.iter().any(|npub| npub == &owner_npub) {
                eprintln!("[wan-g] asker: key granted by the owner after {attempts} apply poll(s)");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "no key grant arrived from the owner within {:?} ({attempts} apply polls at {APPLY_POLL:?}) — \
                     the auto-approve loop should have granted a bigger asker within a few 5 s cycles",
                    GRANT_DEADLINE
                ));
            }
            tokio::time::sleep(APPLY_POLL).await;
        }

        // The granted key decrypts the owner's published listing.
        let key = granted_key(input, &contact_hash, &owner_npub)?;
        let rendered = hb_net::fetch_full_listing_from(
            &client,
            &owner_pk,
            GRANT_SLUG,
            &key,
            &input.relays,
            crate::net::RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("the owner's listing did NOT decrypt under the granted key: {e}"))?;
        if !rendered.complete() || rendered.entries.is_empty() {
            return Err(format!(
                "the owner's listing decrypted but rendered {} entries (complete={}) — the grant did not deliver a usable read",
                rendered.entries.len(),
                rendered.complete()
            ));
        }
        eprintln!(
            "[wan-g] asker: owner's listing decrypted under the granted key ({} entries)",
            rendered.entries.len()
        );
        Ok(())
    } else {
        // WG2: a bounded WAIT — 60 s is 12 full owner poll cycles (and 12 apply polls), far past
        // any plausible slow grant (the positive path lands within a few 5 s cycles), so silence
        // here is the v5 refusal, not latency.
        let deadline = tokio::time::Instant::now() + REFUSE_WAIT;
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            let applied = apply_key_grants(input).await?;
            if applied.iter().any(|npub| npub == &owner_npub) {
                return Err(format!(
                    "a SMALLER asker was GRANTED a key after {attempts} apply poll(s) — the v5 size rule failed to gate the auto-approve loop"
                ));
            }
            let contact = input
                .store
                .load_contact(&contact_hash)
                .map_err(|e| format!("load contact: {e}"))?
                .ok_or_else(|| "the owner contact vanished".to_string())?;
            if contact.browse_key_hex.is_some() {
                return Err(
                    "a SMALLER asker holds a browse key for the owner — the grant gate leaked".to_string()
                );
            }
            if tokio::time::Instant::now() >= deadline {
                eprintln!(
                    "[wan-g] asker: no key after {attempts} apply polls across {REFUSE_WAIT:?} (12 owner poll cycles) — the refusal held"
                );
                return Ok(());
            }
            tokio::time::sleep(APPLY_POLL).await;
        }
    }
}

/// One poll of the PRODUCTION receive path (`apply_key_grants_inner`): fetch the `#p` inbox
/// against the Manual-contact allowlist and apply whatever grants verify.
async fn apply_key_grants(input: &CarryInput) -> Result<Vec<String>, String> {
    let shared_relay = crate::net::new_shared();
    crate::commands::private::apply_key_grants_inner(&input.app_id.identity, &input.store, &shared_relay)
        .await
}

/// Read the granted key back out of the contact record the production apply path wrote.
fn granted_key(
    input: &CarryInput,
    contact_hash: &str,
    owner_npub: &str,
) -> Result<[u8; 32], String> {
    let contact = input
        .store
        .load_contact(contact_hash)
        .map_err(|e| format!("load contact: {e}"))?
        .ok_or_else(|| format!("no contact record for {owner_npub}"))?;
    let hex_key = contact
        .browse_key_hex
        .ok_or_else(|| "the grant was applied but the contact holds no browse key".to_string())?;
    let bytes = hex::decode(&hex_key).map_err(|e| format!("contact browse key is not hex: {e}"))?;
    bytes.try_into().map_err(|_| "the contact browse key is not 32 bytes".to_string())
}

/// Save the owner as a keyless Manual contact — the `save_asker_contact` shape (mod.rs), pointed
/// the other way: the ASKER holds the OWNER, with `browse_key_hex: None` because the whole point
/// of the row is that the key may only arrive via the grant.
fn save_owner_contact_without_key(input: &CarryInput, owner_npub: &str) -> Result<(), String> {
    let contact = crate::store::CachedPeer {
        npub: owner_npub.to_string(),
        source: crate::store::ContactSource::Manual,
        browse_key_hex: None,
        petname: Some("wan-g-owner".to_string()),
        profile: None,
        collections: vec![],
        listings_state: Default::default(), // QURATOR-134 tri-state (not classified on this path)
        online: false,
        last_fetched: chrono::Utc::now(),
        last_presence: None,
        local_tags: vec![],
        fingerprint: None,
    };
    input
        .store
        .save_contact(&crate::store::CachedPeer::pubkey_hash(owner_npub), &contact)
        .map_err(|e| format!("save contact: {e}"))
}

/// Publish this node's profile teaser through the production pieces — `teaser_from_profile`
/// computes `total_bytes` from the published public collections (the v5 size-rule input), and
/// `build_teaser` + `RelayClient::publish` is the inner body of `commands::profile::publish_profile`
/// (no `_inner` shim exists; see the header's honest-gaps note and the WAN-U U1 precedent).
async fn publish_own_teaser(input: &CarryInput) -> Result<(), String> {
    use hb_core::event::build_teaser;
    use hb_core::types::Profile;

    let profile = Profile {
        display_name: "wan-g node".to_string(),
        bio: None,
        tags: vec![],
        since: None,
        est_size: None,
        languages: vec![],
        contact_hint: None,
        email: None,
        location: None,
        social_links: vec![],
        willing_to: vec![],
        content_types: vec![],
        picture: None,
        hide_in_rosters: false,
        // Never set on the user's OWN profile — computed at publish (teaser_from_profile).
        total_bytes: 0,
        updated: chrono::Utc::now(),
    };
    input.store.save_profile_draft(&profile).map_err(|e| format!("save profile draft: {e}"))?;
    let teaser = crate::commands::profile::teaser_from_profile(&input.store, &profile);
    let event = build_teaser(&input.app_id.identity, &teaser, false)
        .map_err(|e| format!("build_teaser: {e}"))?;
    let shared_relay = crate::net::new_shared();
    let client = crate::net::client(&input.app_id.identity, &input.store, &shared_relay)
        .await
        .map_err(|e| format!("relay client: {e:#}"))?;
    client.publish(&event).await.map_err(|e| format!("publish teaser: {e}"))?;
    // The marker `publish_profile` saves after publishing (NIP-09 unpublish + refresh eligibility).
    let event_json = serde_json::to_string(&event).map_err(|e| format!("serialize event: {e}"))?;
    input.store.save_published("profile", &event_json).map_err(|e| format!("save published marker: {e}"))?;
    Ok(())
}
