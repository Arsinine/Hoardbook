//! Suite CAP — the paywall teaser's *envelope budget*, over the wire.
//!
//! A published listing's metadata and its directory tree share one `LISTING_MAX_BYTES` budget, and
//! `truncate_listing` measures the metadata first: `entries_budget = max_bytes - overhead`. Metadata
//! big enough to consume the budget leaves the tree nothing, and the resulting event is **still
//! valid and still under the relay's size cap** — so it publishes, and every holder of the share
//! code browses a collection that looks empty.
//!
//! That failure mode is invisible to the existing coverage. N4 proves an oversize listing *cannot*
//! publish as one event (the relay rejects it); nothing proved that a listing which publishes
//! *successfully* actually carries its tree. The gap is "accepted but empty", and only a real relay
//! plus a real browse shows it — locally the bytes look fine and `shown_items` stays self-consistent.
//!
//! CAP1 — the largest envelope production can emit (every serialized field at its hb-core ceiling,
//!        filled with the worst-case character) publishes a teaser the relay accepts, and browsing
//!        it back yields a NON-EMPTY tree. The regression guard: it grows with whichever ceiling
//!        moves, so raising any of them past what the budget affords turns it red.
//! CAP2 — RETIRED 2026-09-27 (owner: "retire CAP2"). It demonstrated that metadata alone over the
//!        budget publishes a teaser that browses back EMPTY. That hazard no longer exists in
//!        production: `hb_core::Collection::clamp_metadata` bounds the envelope to a few hundred
//!        bytes, and since QURATOR-344 the budget measures the compressed, sealed size, so even
//!        unclamped repetitive metadata seals to almost nothing. Reproducing it took incompressible
//!        metadata at a budget ten times tighter than production's — a code-path pin for a failure
//!        the app cannot produce. CAP1 remains the guard that capped metadata leaves the tree its
//!        budget.
//!
//! MEASURED, 2026-07-29, against this suite's strfry: at the production 40 KB budget a 42 KB
//! description yields a **66,035-byte event**, over strfry's 65,536 `maxEventSize` — so the relay
//! rejects it (that is N4's loud band). Encrypted expansion is ~1.56x, not the ~1.33x a naive
//! base64 estimate gives, because NIP-44 pads plaintext into buckets. The genuinely *silent* band
//! is therefore narrow — roughly 40,000-42,000 bytes of metadata — and its exact edge moves with
//! NIP-44's padding steps. (The retired CAP2 scaled the BUDGET down rather than the metadata up
//! for that reason.)
//!
//! RE-MEASURED, 2026-09-27 (QURATOR-344 fallout): the publish budgets now measure the **SEALED**
//! size (`hb_core::sealed_listing_len`: zstd, then NIP-44, then base64), so a repetitive fixture
//! that padded in *plaintext* now seals to a few KB and exercises nothing. The size-driving bytes
//! in every fixture here are therefore deterministic per-index xorshift noise (6 bits/char, so
//! zstd cannot shrink them), while the metadata fields stay at their ceilings with the worst-case
//! escape character. Under compression, *compressible* metadata can no longer starve the tree at
//! any realistic size, which is why CAP2 was retired (see above).

use anyhow::{ensure, Result};
use hb_core::{Identity, ShareCode};
use hb_core::{
    FILESYSTEM_SLUG_CHARS, MAX_CONTENT_TYPES, MAX_DESCRIPTION_CHARS, MAX_EST_SIZE_CHARS,
    MAX_LANGUAGES, MAX_LIST_ITEM_CHARS, MAX_PATH_ALIAS_CHARS, MAX_TAGS, MAX_TAG_CHARS,
};
use hb_net::{browse_share_code, publish_listing_capped};
use serde_json::Value;

use crate::harness::{result, settle, Ctx, FETCH_TIMEOUT};
use crate::tap::TestResult;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("CAP1 capped metadata → teaser carries its tree", cap1(ctx).await),
    ]
}

/// The publish budget the app uses. hb-it cannot depend on hb-app (the Tauri crate), so the value is
/// restated — and pinned to hb-app's source, because a silently-restated constant is exactly how a
/// budget change leaves this suite green while production behaviour moves.
pub(crate) const LISTING_MAX_BYTES: usize = 40_000;

pub(crate) fn ensure_budget_matches_hb_app() -> Result<()> {
    const SRC: &str = include_str!("../../hb-app/src/commands/collection.rs");
    const DECL: &str = "const LISTING_MAX_BYTES: usize = 40_000;";
    ensure!(
        SRC.contains(DECL),
        "hb-app no longer declares `{DECL}` — suite CAP's restated publish budget has drifted from \
         production and its assertions no longer describe the shipped behaviour"
    );
    Ok(())
}

fn bk(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// Deterministic incompressible padding — a per-seed xorshift64 stream over a 64-char alphabet (6
/// bits/char, so zstd cannot shrink it). The budgets measure the SEALED body, so repetitive
/// padding is vacuous; this keeps the fixture byte-identical across runs (round-trip assertions
/// stay exact) while making the SPLIT, not the compressor, decide the sizes. The same pattern as
/// suite_n/suite_browse's `big_listing` and suite_v5's `pseudo_hex`.
pub(crate) fn noise(seed: u64, len: usize) -> String {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(ALPHABET[(state & 63) as usize] as char);
    }
    out
}

/// The most expensive character JSON can carry: serde escapes U+0001 to the six bytes `\u0001`,
/// which costs more than a 4-byte emoji. Using it makes the fixture an upper bound rather than a
/// typical case — the same choice hb-app's `envelope_at_caps_leaves_the_tree_its_budget` makes.
const WORST: &str = "\u{1}";

fn entries(n: usize) -> Vec<Value> {
    // `item_type`/`tags`/`children` are the keys a real `collection_to_listing_json` tree emits, so
    // the truncated teaser's digest re-derivation (audit #25 / QURATOR-123) has a decodable
    // `DirectoryItem` tree to hash — without them the teaser takes the "drop the digest" path and
    // the fixture stops exercising the production shape. Cheap (~30 bytes/entry) relative to the
    // padding that drives the budget.
    // Each entry also carries a 96-char noise `note` (see [`noise`]): under the sealed measure a
    // repetitive entry list seals to a few KB and the teaser never truncates, so CAP1's premise
    // ("far past the budget") depends on the entries themselves being size-real. ~80 B sealed
    // per entry.
    (0..n)
        .map(|i| {
            serde_json::json!({
                "name": format!("title-{i:05}-padding-padding-padding-xx"),
                "note": noise(i as u64 ^ 0x5A5A_5A5A_5A5A_5A5A, 96),
                "item_type": "File", "tags": [], "children": [],
            })
        })
        .collect()
}

/// The **largest envelope production can emit**: every field `collection_to_listing_json` serializes,
/// each at its hb-core ceiling, filled with the worst-case character.
///
/// Every ceiling appears here on purpose. A fixture that maxes only the description would stay green
/// if `MAX_CONTENT_TYPES` or `MAX_LANGUAGES` were raised — the test has to grow with whichever
/// ceiling moves, or it is not a guard on the ceilings at all. `slug` is the exception: it is the
/// relay `d` tag, so it stays a valid slug and is padded to its filesystem-imposed bound instead.
fn max_metadata_listing(slug: &str, n: usize) -> String {
    let pad = FILESYSTEM_SLUG_CHARS.saturating_sub(slug.len() + 1);
    serde_json::json!({
        "slug": format!("{slug}-{}", "s".repeat(pad)),
        "path_alias": WORST.repeat(MAX_PATH_ALIAS_CHARS),
        "description": WORST.repeat(MAX_DESCRIPTION_CHARS),
        "item_count": u64::MAX,
        "est_size": WORST.repeat(MAX_EST_SIZE_CHARS),
        "content_types": (0..MAX_CONTENT_TYPES).map(|_| WORST.repeat(MAX_LIST_ITEM_CHARS)).collect::<Vec<_>>(),
        "tags": (0..MAX_TAGS).map(|_| WORST.repeat(MAX_TAG_CHARS)).collect::<Vec<_>>(),
        "languages": (0..MAX_LANGUAGES).map(|_| WORST.repeat(MAX_LIST_ITEM_CHARS)).collect::<Vec<_>>(),
        "visibility": "Private",
        "sorted": true,
        "last_updated": "2026-07-29T12:34:56.789012345Z",
        "snapshot_fingerprint": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "entries": entries(n),
    })
    .to_string()
}

/// Publish a teaser and browse it straight back with the share code, returning the rendered entries.
async fn publish_and_browse(
    ctx: &Ctx,
    owner: &Identity,
    slug: &str,
    key: [u8; 32],
    listing: &str,
    max_bytes: usize,
) -> Result<(usize, Vec<Value>)> {
    let client = ctx.connect(owner).await?;
    let teaser = publish_listing_capped(&client, owner, slug, &key, listing, max_bytes).await?;
    client.disconnect().await;
    settle().await;

    let browser = ctx.connect(&Identity::generate()).await?;
    let code = ShareCode::Full { pubkey: owner.public_key(), browse_key: key };
    let res = browse_share_code(&browser, &code, slug, &ctx.relays, &ctx.relays, FETCH_TIMEOUT).await?;
    browser.disconnect().await;

    let rendered = res
        .listing
        .ok_or_else(|| anyhow::anyhow!("the teaser did not come back — the relay rejected the event"))?;
    Ok((teaser.shown_items, rendered.entries))
}

/// CAP1 — metadata at every ceiling still leaves the tree its budget, proven by browsing it back.
async fn cap1(ctx: &Ctx) -> Result<()> {
    let owner = Identity::generate();
    let key = bk(31);
    ensure_budget_matches_hb_app()?;
    let slug = ctx.tag("cap1");
    // 1300 noise-note entries seal far past the 40 KB budget (measured ~250 KB sealed), so the
    // teaser MUST truncate — truncation is where a fat envelope silently eats the whole allowance.
    // Measured 2026-09-27: ~198 entries survive into the teaser.
    let listing = max_metadata_listing(&slug, 1300);

    let (shown, entries) =
        publish_and_browse(ctx, &owner, &slug, key, &listing, LISTING_MAX_BYTES).await?;

    ensure!(shown > 0, "a capped collection published a teaser with {shown} items — the tree was starved");
    ensure!(
        !entries.is_empty(),
        "the teaser round-tripped but rendered 0 entries: peers would see an empty collection"
    );
    // The point of the caps is that the tree keeps *effectively all* of the budget, not a token
    // slice of it. Worst-case metadata measures ~8.5 KB of 40 KB (hb-app's envelope test), so a
    // capped teaser should carry hundreds of entries, not a handful.
    ensure!(
        shown >= 100,
        "a capped teaser carried only {shown} items — the envelope is eating the tree's budget"
    );
    Ok(())
}
