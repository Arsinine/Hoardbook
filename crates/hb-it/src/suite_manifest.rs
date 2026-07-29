//! Suite MAN — M16 Layer 2: the `.hbmanifest` **file** carrier, round-tripped against a real relay.
//!
//! Layer 3 (Suite BIGRELAY) proves the full listing can reach a browser over a second relay. Layer 2
//! is the other carrier for the *same* envelope: the hoarder exports a file, hands it over out of
//! band, and the recipient imports it to lift a truncated paywall teaser. Suite BIGRELAY covers the
//! relay carrier; nothing covered the file one.
//!
//! **Why this needs an integration suite at all**, given `hb-core::manifest` has 17 unit tests: those
//! tests treat the envelope body as *opaque*, and their multi-part fixture is hand-written
//! (`{"part":0,"entries":[]}`). Real production parts come from `hb_net::split_listing` and are read
//! back by `hb_net::render_listing` — a cross-crate seam that hb-core cannot see (it does not depend
//! on hb-net) and hb-app's unit tests exercise only with a store. `hb-it` is the only crate that
//! depends on both. That is the same class of blind spot the W7.2 packer defect landed in: a
//! transform tested exclusively against hand-written fixtures, where the production serializer emits
//! a different shape. **At least one fixture has to come from the actual serializer.**
//!
//! The relay is not decoration here. The manifest's staleness gate compares its fingerprint against
//! *the teaser the browser is currently showing*, so the honest input is a fingerprint read back off
//! a real published teaser, not a constant restated on both sides of the assertion.
//!
//!   MAN1 — a manifest exported from a listing that truncates on the relay round-trips through a real
//!          file and restores the WHOLE tree, at a fingerprint matching the teaser browsed back.
//!   MAN2 — a manifest built over an older snapshot is detected as stale against the relay's teaser,
//!          and still opens (stale is surfaced, never blocking — an older list beats no list).
//!   MAN3 — a validly-signed manifest carrying only the split INDEX renders incomplete, so a partial
//!          family cannot masquerade as the full tree; and it will not verify under another author.

use anyhow::{ensure, Result};
use hb_core::manifest::{build_manifest_envelope, ManifestEnvelope};
use hb_core::{Identity, ShareCode};
use hb_net::{browse_share_code, publish_listing_capped, render_listing, split_listing};
use serde_json::Value;

use crate::harness::{now, result, settle, Ctx, FETCH_TIMEOUT};
use crate::suite_cap::{ensure_budget_matches_hb_app, LISTING_MAX_BYTES};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("MAN1 .hbmanifest file round trip lifts a truncated teaser to the full tree", man1(ctx).await),
        result("MAN2 manifest older than the relay's teaser reads stale, still opens", man2(ctx).await),
        result("MAN3 index-only manifest renders incomplete and fails a foreign author", man3(ctx).await),
    ]
}

/// The snapshot the teaser and the manifest share; `FP_OLD` is an earlier snapshot of the same tree.
const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const FP_OLD: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// Entry count chosen to exceed the publish budget comfortably, so the teaser truncates and the
/// manifest splits into an index plus several content parts — the case the file carrier exists for.
const ENTRIES: usize = 1300;

fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// A full listing carrying `snapshot_fingerprint = fp`, shaped like the canonical listing JSON both
/// carriers are handed (`collection_to_listing_json`'s output, which hb-it cannot call).
fn full_listing(slug: &str, n: usize, fp: &str) -> String {
    let entries: Vec<Value> = (0..n)
        .map(|i| serde_json::json!({ "name": format!("title-{i:05}-padding-padding-padding-xx") }))
        .collect();
    serde_json::json!({
        "slug": slug,
        "path_alias": "Vault",
        "content_types": ["video"],
        "snapshot_fingerprint": fp,
        "entries": entries,
    })
    .to_string()
}

/// The export half of `hb-app::build_slug_manifest`, minus the store and the visibility check it
/// wraps around this: split the canonical listing at the production budget, then seal + sign the
/// ordered parts. Deliberately routed through the real `split_listing` — hand-written parts are the
/// blind spot this suite exists to close.
fn export_envelope(
    owner: &Identity,
    slug: &str,
    key: &[u8; 32],
    listing: &str,
    fp: &str,
) -> Result<ManifestEnvelope> {
    let parts: Vec<String> = split_listing(slug, listing, LISTING_MAX_BYTES)?
        .into_iter()
        .map(|p| p.json)
        .collect();
    Ok(build_manifest_envelope(owner, slug, key, fp, now(), &parts)?)
}

/// Publish the truncated teaser and browse it straight back with the share code, returning what a
/// paywalled recipient actually sees: the entries rendered, and the fingerprint their teaser carries.
async fn publish_teaser_and_browse(
    ctx: &Ctx,
    owner: &Identity,
    slug: &str,
    key: [u8; 32],
    listing: &str,
) -> Result<(usize, String)> {
    let client = ctx.connect(owner).await?;
    let teaser = publish_listing_capped(&client, owner, slug, &key, listing, LISTING_MAX_BYTES).await?;
    client.disconnect().await;
    ensure!(teaser.truncated, "the fixture must truncate — a manifest only has a job behind a paywall");
    settle().await;

    let browser = ctx.connect(&Identity::generate()).await?;
    let code = ShareCode::Full { pubkey: owner.public_key(), browse_key: key };
    let res = browse_share_code(&browser, &code, slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    browser.disconnect().await;

    let rendered = res
        .listing
        .ok_or_else(|| anyhow::anyhow!("the teaser did not come back — the relay rejected the event"))?;
    // The staleness gate's real input: the fingerprint the browser reads off the teaser it is
    // showing, not a constant restated on both sides of the comparison.
    let fp = rendered
        .meta
        .get("snapshot_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the browsed teaser carries no snapshot_fingerprint to gate on"))?
        .to_string();
    Ok((rendered.entries.len(), fp))
}

/// Write the envelope to a real file and read it back — the export/import artifact, not just its
/// in-memory struct. Named per run so concurrent or repeated runs cannot collide.
fn write_then_read(ctx: &Ctx, slug: &str, env: &ManifestEnvelope) -> Result<ManifestEnvelope> {
    let path = std::env::temp_dir().join(format!("hbit-{}-{slug}.hbmanifest", ctx.run_id));
    std::fs::write(&path, env.to_json()?.as_bytes())?;
    let raw = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(ManifestEnvelope::from_json(&raw)?)
}

/// MAN1 — the whole Layer-2 loop: truncate on the relay, export to a file, import, get the tree back.
async fn man1(ctx: &Ctx) -> Result<()> {
    ensure_budget_matches_hb_app()?;
    let owner = Identity::generate();
    let key = bk(41);
    let slug = ctx.tag("man1");
    let listing = full_listing(&slug, ENTRIES, FP);

    // What a recipient holding the share code can see today: a cropped tree.
    let (shown, teaser_fp) = publish_teaser_and_browse(ctx, &owner, &slug, key, &listing).await?;
    ensure!(
        shown > 0 && shown < ENTRIES,
        "the teaser should be a partial view of {ENTRIES} entries, got {shown}"
    );

    // Export → file → import.
    let env = export_envelope(&owner, &slug, &key, &listing, FP)?;
    ensure!(
        env.ciphertexts.len() > 2,
        "a collection this size must chunk into an index plus content parts, got {} part(s) — the \
         single-part path would leave the multi-part seam untested",
        env.ciphertexts.len()
    );
    let imported = write_then_read(ctx, &slug, &env)?;
    ensure!(imported == env, "the manifest did not survive a file write/read byte-for-byte");

    // The import path proper: verify under the browsed author, decrypt, render.
    let parts = imported.open(&key, &owner.public_key())?;
    let rendered = render_listing(&parts)?;
    ensure!(
        rendered.complete(),
        "the imported manifest rendered incomplete ({} of {} parts, missing {:?})",
        rendered.parts_present,
        rendered.parts_total,
        rendered.missing
    );
    ensure!(
        rendered.entries.len() == ENTRIES,
        "restitched {} of {ENTRIES} entries — the manifest did not restore the whole tree",
        rendered.entries.len()
    );
    ensure!(
        rendered.entries.len() > shown,
        "the manifest returned no more than the teaser already showed ({shown}) — it lifted nothing"
    );
    // Parity with the teaser on the relay: same source tree ⇒ the import is NOT flagged stale.
    ensure!(
        imported.matches_fingerprint(&teaser_fp),
        "the manifest's fingerprint does not match the teaser browsed off the relay — a current \
         manifest would be surfaced as stale"
    );
    Ok(())
}

/// MAN2 — an older manifest against a newer teaser: flagged stale, but still openable.
///
/// The mirror of BIG2 for the file carrier, with the opposite disposition, and that difference is the
/// point. The big-relay gate *withholds* a stale snapshot (it supersedes the teaser silently, so it
/// must not). A file the user was handed deliberately is *surfaced* as stale and imported anyway —
/// `open_manifest` returns `stale: true` rather than an error. Pinning that here stops a later
/// "consistency" change from turning a surfaced warning into a refusal.
async fn man2(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let key = bk(42);
    let slug = ctx.tag("man2");

    // The relay holds the CURRENT snapshot; the file the recipient was handed is an older one.
    let (_shown, teaser_fp) =
        publish_teaser_and_browse(ctx, &owner, &slug, key, &full_listing(&slug, ENTRIES, FP)).await?;
    let env = export_envelope(&owner, &slug, &key, &full_listing(&slug, ENTRIES, FP_OLD), FP_OLD)?;
    let imported = write_then_read(ctx, &slug, &env)?;

    ensure!(
        !imported.matches_fingerprint(&teaser_fp),
        "an older snapshot must be detectable against the teaser's fingerprint, or the recipient is \
         shown a stale tree as if it were current"
    );
    // Stale is a label, not a rejection: it still verifies and still renders the full tree.
    let rendered = render_listing(&imported.open(&key, &owner.public_key())?)?;
    ensure!(rendered.complete(), "a stale manifest must still open completely — stale is not corrupt");
    ensure!(rendered.entries.len() == ENTRIES, "a stale manifest lost entries on import");
    Ok(())
}

/// MAN3 — the completeness gate and the author pin, against real serializer output.
///
/// A signed envelope carrying only the split *index* decrypts and verifies perfectly — it is
/// genuinely the author's — yet describes a tree it does not contain. `open_manifest` refuses it on
/// `rendered.complete()`; this proves the render actually reports incomplete for a real index part,
/// which is the fact that gate depends on. No relay needed, so it is deliberately cheap.
async fn man3(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let key = bk(43);
    let slug = ctx.tag("man3");
    let listing = full_listing(&slug, ENTRIES, FP);

    let parts: Vec<String> =
        split_listing(&slug, &listing, LISTING_MAX_BYTES)?.into_iter().map(|p| p.json).collect();
    ensure!(parts.len() > 2, "fixture must split into an index plus content parts, got {}", parts.len());

    // Keep the index, drop every content part, and re-sign — a valid manifest describing N parts
    // while carrying one.
    let index_only =
        build_manifest_envelope(&owner, &slug, &key, FP, now(), &parts[..1])?;
    index_only.verify_author(&owner.public_key())?; // genuinely the author's: the sig is not the defence
    let rendered = render_listing(&index_only.decrypt(&key)?)?;
    ensure!(
        !rendered.complete(),
        "an index-only manifest reported complete — a partial family can masquerade as the full tree"
    );
    ensure!(
        !rendered.missing.is_empty() && rendered.parts_present < rendered.parts_total,
        "the render must name the missing parts ({} of {}, missing {:?})",
        rendered.parts_present,
        rendered.parts_total,
        rendered.missing
    );

    // The author pin, on a real multi-part artifact: A's manifest must not verify while browsing B.
    let full = build_manifest_envelope(&owner, &slug, &key, FP, now(), &parts)?;
    let other = Identity::generate();
    ensure!(
        full.verify_author(&other.public_key()).is_err(),
        "a manifest authored by one peer verified while browsing another"
    );
    ensure!(
        full.open(&bk(99), &owner.public_key()).is_err(),
        "the wrong browse key opened the manifest body"
    );
    Ok(())
}
