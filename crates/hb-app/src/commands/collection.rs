use std::path::Path;
use globset::{Glob, GlobSetBuilder};
use hb_core::types::{Collection, DirectoryItem, ItemType, Visibility};
use hb_core::{BrowseKey, Identity};
use hb_net::{publish_listing_capped, publish_listing_to, split_listing};
use nostr::{Event, Filter, Kind};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    commands::profile::{compute_content_types, teaser_from_profile},
    error::{CmdResult, cmd_err},
    net::{self, SharedRelay},
    store::{DataStore, PublishedSave},
    SharedIdentity,
};

/// NIP-44 listing split budget (NIP-44 caps plaintext, the relay caps the event ~64 KiB; ≤40 KB
/// keeps a single part well under both). Larger listings split per-folder (hb-net::split_listing).
///
/// `pub(crate)` so the WAN-E2E harness (`crate::wan_it`) can use the exact production truncation
/// threshold — a harness-private copy could drift from the real budget and make the truncating-seed
/// assertion meaningless.
pub(crate) const LISTING_MAX_BYTES: usize = 40_000;

/// Split budget for the `.hbmanifest` / iroh manifest path, decoupled from [`LISTING_MAX_BYTES`].
/// Nothing here publishes per-part to a relay — one signed envelope carries every part inline as a
/// single framed iroh stream or one `.hbmanifest` file — so the anti-ban budget is irrelevant and
/// the real ceiling is NIP-44's 65_408-byte plaintext cap (the value `hb-net`'s split tests pin).
pub(crate) const MANIFEST_SPLIT_MAX_BYTES: usize = 65_408;

/// Hard cap on items (files + folders, [`count_items`]'s definition) per collection — owner
/// ruling 2026-08-19, paired with raising [`hb_core::MANIFEST_MAX_TRANSPORT_BYTES`] to 16 MiB.
/// Enforced in [`scan_selective`], the single chokepoint every scan caller (the add/rescan
/// command, the WAN-IT harness's `--seed-dir`) goes through, so nothing can bypass it by calling
/// a different entry point.
///
/// This guards a DIFFERENT failure shape than the byte ceiling does. The byte ceiling bounds
/// large-content-per-file (a media library with big per-entry metadata); this bounds
/// many-tiny-files (a software/game library, where file count tracks disk contents rather than
/// anything a person browses one entry at a time — see `MANIFEST_MAX_TRANSPORT_BYTES`'s doc
/// comment for the full reasoning). The two decouple exactly for that shape: a collection can
/// blow past one cap while sitting comfortably under the other, so both are needed.
///
/// Reject, not truncate — the byte ceiling's philosophy applies here too: an honest refusal at
/// scan/add time (before the user invests effort organizing an unshareable collection) beats a
/// silent partial listing.
pub(crate) const MAX_COLLECTION_ITEMS: u64 = 100_000;

/// Item count past which a collection is **OVERSIZED** (owner ruling 2026-09-25, QURATOR-336): it
/// cannot possibly be sent in full, so the scan stops early and its listing becomes a bounded
/// breadth-first teaser instead of a whole tree (and [`build_slug_manifest`] refuses it outright).
/// A lower-bound count at or under this line takes *today's* path unchanged — including the
/// [`MAX_COLLECTION_ITEMS`] refusal, which is strictly tighter.
///
/// Where the number comes from: the measured worst case is a game library of 71,547 items sealing
/// to 11.2 MB — ≈157 B/item against `hb_core::MANIFEST_MAX_TRANSPORT_BYTES`'s 16 MiB ceiling — which
/// bounds a full manifest at ≈107,000 items. 150,000 is that bound rounded up with headroom, so
/// anything past it *obviously* cannot fit and there is no reason to walk the tree to find out.
pub(crate) const OVERSIZED_ITEM_THRESHOLD: u64 = 150_000;

/// The result of publishing a Public collection (devtest #7) — whether it was truncated to a paywall
/// teaser, and how many item nodes browsers can see vs how many the full collection holds. The
/// frontend uses this to tell the user their large collection is showing a preview.
#[derive(Debug, Clone, Serialize)]
pub struct PublishSummary {
    pub truncated: bool,
    pub shown_items: usize,
    pub total_items: usize,
    /// M16 W3 (Layer 3) — how many full-manifest part events were published to the big relay. `0`
    /// when the listing fit whole (not truncated) or no big relay is configured (feature off). A
    /// non-zero value means the full listing family is available on the big relay behind the teaser.
    #[serde(default)]
    pub big_relay_parts: usize,
}

impl PublishSummary {
    /// A non-truncated publish (fits whole, or a Private collection — never truncated).
    fn whole() -> Self {
        Self { truncated: false, shown_items: 0, total_items: 0, big_relay_parts: 0 }
    }
}

/// M16 W3 classifier — the big relay to *also* publish the full manifest family to, or `None`.
/// Returns `Some(url)` iff the listing was truncated to a paywall teaser AND a big relay is
/// configured (a non-empty, non-whitespace URL). A listing that fit whole, or an unset/blank
/// setting, yields `None`: no big-relay write, and the shipped small-collection publish stays
/// byte-identical (M16 headline failure mode #2). The returned slice is the trimmed URL.
fn big_relay_target(truncated: bool, big_relay_url: &str) -> Option<&str> {
    let url = big_relay_url.trim();
    (truncated && !url.is_empty()).then_some(url)
}

/// Stamp the owner's big relay URL into a listing JSON's top-level metadata (M16 W3, browse-side
/// **option b**): it rides through `truncate_listing` into the paywall teaser, so a browser holding
/// the share code can discover *this hoarder's* big relay and fetch the full family from it even when
/// its own big-relay setting points elsewhere. The teaser is browse-key-*encrypted*, so the URL
/// reaches only share-code holders — never the public (INV-2 untouched: a relay URL is not the
/// browse-key). A blank setting is a no-op returning the input **unchanged** (feature off ⇒ the
/// small-collection teaser stays byte-identical). Pure — unit-tested without a relay.
fn stamp_big_relay_url(listing_json: &str, big_relay_url: &str) -> Result<String, String> {
    let url = big_relay_url.trim();
    if url.is_empty() {
        return Ok(listing_json.to_string());
    }
    let mut v: serde_json::Value = serde_json::from_str(listing_json).map_err(cmd_err)?;
    if let serde_json::Value::Object(ref mut map) = v {
        map.insert("big_relay_url".into(), serde_json::Value::String(url.to_string()));
    }
    serde_json::to_string(&v).map_err(cmd_err)
}

/// Stamp the **teaser digest** (`teaser_fingerprint`) into a full listing's metadata (audit #25 /
/// QURATOR-123): the digest the truncated teaser of this exact listing carries — over the VISIBLE
/// entries + the elided count, as re-derived by `hb_net::truncate_listing` itself. This is the one
/// value a full carrier (the big-relay family, the `.hbmanifest` plaintext) can still share with the
/// teaser: the teaser may no longer carry a digest of the content truncation hides from it, so the
/// carriers carry the teaser's digest instead of the reverse.
///
/// Derived by running the SAME `truncate_listing` call the teaser publish performs, on the SAME
/// bytes, and reading the digest back out of the resulting artifact (not a re-implementation, so
/// the two cannot drift). A listing that fits the budget whole is returned **unchanged** — nothing
/// is hidden, the teaser carries the full-tree `snapshot_fingerprint`, and the byte-identical
/// untruncated publish survives. Pure.
pub(crate) fn stamp_teaser_fingerprint(listing_json: &str) -> Result<String, String> {
    let t = hb_net::truncate_listing(listing_json, LISTING_MAX_BYTES).map_err(cmd_err)?;
    if !t.truncated {
        return Ok(listing_json.to_string());
    }
    let v: serde_json::Value = serde_json::from_str(&t.json).map_err(cmd_err)?;
    let Some(fp) = v.get("snapshot_fingerprint").and_then(|f| f.as_str()) else {
        // `truncate_listing` drops an underivable digest (its kept entries did not decode as a
        // `DirectoryItem` tree) — there is no teaser digest to stamp, and none is needed: the
        // browse-side gates read `None` and keep the teaser.
        return Ok(listing_json.to_string());
    };
    let mut out: serde_json::Value = serde_json::from_str(listing_json).map_err(cmd_err)?;
    if let serde_json::Value::Object(ref mut map) = out {
        map.insert("teaser_fingerprint".into(), serde_json::Value::String(fp.to_string()));
    }
    serde_json::to_string(&out).map_err(cmd_err)
}

/// The one place that decides whether a listing will truncate and, when it will, stamps it into its
/// final publishable form (QURATOR-205): the `will_truncate` decision is made on the UNSTAMPED bytes
/// (stamping a ~40-byte URL must never itself tip a near-limit collection into truncation), then, only
/// on that branch, the big-relay URL and the teaser digest are stamped in — in that order, since the
/// URL adds meta bytes that shift the entries budget `truncate_listing` uses to decide what stays
/// visible. Shared by the publish path (`publish_collection_inner`) and the mint path
/// (`build_slug_manifest`) so the two cannot derive different visible-entry sets, and therefore
/// different `snapshot_fingerprint` digests, for the SAME unchanged collection. A listing that fits
/// the budget whole is returned byte-identical.
pub(crate) fn stamp_for_teaser(listing_json: &str, big_relay_url: &str) -> Result<String, String> {
    let will_truncate =
        hb_net::truncate_listing(listing_json, LISTING_MAX_BYTES).map_err(cmd_err)?.truncated;
    if will_truncate {
        stamp_teaser_fingerprint(&stamp_big_relay_url(listing_json, big_relay_url)?)
    } else {
        Ok(listing_json.to_string())
    }
}

/// Collection with publication status, returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionEntry {
    #[serde(flatten)]
    pub collection: Collection,
    /// True if this collection has been signed and published.
    pub published: bool,
    /// Total bytes on disk (devtest 2026-06-25 #5). Carried on the UI wrapper — **not** on the
    /// published `Collection` (which deliberately omits exact bytes; see the hb-core invariant test)
    /// — so the home "Total Size" / "Disk size (auto)" aggregate works without leaking byte counts
    /// into the relay listing. Sourced from the per-slug `ScanSpec` sidecar.
    #[serde(default)]
    pub total_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct ScanOptions {
    pub path: String,
    pub path_alias: String,
    /// Relative, "/"-separated paths the user checked in the picker. A checked *folder* (and
    /// everything under it) is walked in full; a checked *file* is force-included even when its parent
    /// folder is not checked (devtest #10); root-level loose files are always included. Replaces the
    /// former `depth` slider (M8, HANDOVER §A2.1).
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// An immediate child of a scanned path — one node of the folder-tree picker. Both directories and
/// files are listed (devtest #10 — individual files are selectable, not just whole folders).
#[derive(Debug, Clone, Serialize)]
pub struct SubdirEntry {
    pub name: String,
    /// Absolute path on disk (handed back so the frontend can lazily expand this node).
    pub path: String,
    /// True if this node has expandable children (a sub-directory OR loose files). A file is always
    /// `false`; a directory is `true` iff it contains at least one child (drives the ▶ expander).
    pub has_children: bool,
    /// True for a file leaf, false for a directory (devtest #10 — the picker renders + selects them
    /// differently: a checked file is force-included even when its parent folder is not checked).
    #[serde(default)]
    pub is_file: bool,
}

#[tauri::command]
pub async fn scan_directory(
    opts: ScanOptions,
    store: State<'_, DataStore>,
) -> CmdResult<CollectionEntry> {
    let collection = scan_directory_inner(opts, store.inner()).await?;
    // Surface the scanned byte total to the freshly-added collection immediately (devtest #5).
    // QURATOR-207: the scan persists nothing durable, so the total is read back from the in-memory
    // scan cache `scan_directory_inner` just populated — it rides there until Publish promotes it
    // (Publish persists the ScanSpec sidecar `get_collections` reads).
    let total_bytes = scan_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&collection.slug)
        .map(|c| c.total_bytes)
        .unwrap_or(0);
    Ok(CollectionEntry { collection, published: false, total_bytes })
}

/// List the immediate child directories of `path` for the folder-tree picker. Lazy (called once per
/// expand), sorted, deadline-guarded — a wedged SMB mount must not hang `read_dir` forever.
#[tauri::command]
pub async fn list_subdirs(path: String) -> CmdResult<Vec<SubdirEntry>> {
    // Same off-runtime + deadline discipline as `scan_directory` (see the comment there).
    match tauri::async_runtime::spawn_blocking(move || {
        run_blocking_with_deadline(
            move || list_subdirs_core(&path),
            std::time::Duration::from_secs(30),
        )
        .map_err(|e| {
            if e == DEADLINE_EXCEEDED {
                "Listing sub-folders timed out — check that the path is accessible and try again."
                    .to_string()
            } else {
                e
            }
        })
    })
    .await
    {
        Ok(inner) => inner,
        Err(e) => Err(format!("Sub-folder listing task failed: {e}")),
    }
}

/// QURATOR-207 (owner ruling 2026-09-13) — the in-memory scan cache. A collection is PUBLISHED or
/// it does not exist: `scan_directory` persists NOTHING durable, so the scanned tree lives here
/// until Publish promotes it to the store ([`promote_cached_scan`]). Closing the Add-collection
/// wizard before Publish discards it — nothing was written, so there is no cleanup path (that is
/// the design; do not add one). Keyed by slug; a hit additionally requires the same path +
/// include/exclude settings, so any change re-walks. Best-effort and process-lifetime only — it
/// evaporates on app exit and is never persisted to disk.
struct CachedScan {
    path: String,
    include: Vec<String>,
    exclude: Vec<String>,
    total_bytes: u64,
    collection: Collection,
}

/// The process-global scan cache (same OnceLock idiom as `inflight_access_stats` above).
fn scan_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CachedScan>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, CachedScan>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Core scan logic, extracted for testability (mirrors `publish_collection_inner`). Walks the
/// directory off the async runtime thread under a deadline, then builds the collection — and
/// persists NOTHING (QURATOR-207): the result goes to the in-memory scan cache, and Publish is
/// the single durable write.
async fn scan_directory_inner(opts: ScanOptions, store: &DataStore) -> CmdResult<Collection> {
    let root = std::path::PathBuf::from(&opts.path);

    // Bound the alias here, before the slug is derived from it — this is the one place shortening
    // it is safe, because no slug exists yet to be re-addressed (see `Collection::clamp_metadata`).
    let path_alias = hb_core::truncate_alias(&opts.path_alias);
    let slug = Collection::slug_from_alias(&path_alias);
    if !is_valid_slug(&slug) {
        return Err(format!(
            "'{}' produces an invalid collection slug — use only letters, numbers, hyphens, or Unicode characters; avoid spaces and symbols",
            opts.path_alias
        ));
    }
    // QURATOR-249: collection markers and the fixed-key profile teaser share one flat marker
    // namespace (`published/<key>.json`). A collection slugged "profile" would clobber the
    // teaser marker on publish (and vice versa), corrupting tier/big-relay bookkeeping and
    // flipping `is_published("profile")` on — republishing profile teasers the user never
    // consented to. Reserved here, the single site collection slugs are created; lookups by
    // slug (delete/unpublish/export) deliberately still accept it so a legacy draft stays
    // manageable.
    if is_reserved_marker_slug(&slug) {
        return Err(format!(
            "'{slug}' is a reserved name — pick a different alias for this collection"
        ));
    }
    // Cache fast path (the ruling's "reopening the add/edit flow loads instantly instead of
    // re-walking the directory"): the SAME path scanned with the SAME include/exclude settings
    // returns the cached tree. Two deliberate limits: it applies only while the slug has NO
    // durable record — the row-menu Rescan verb targets a persisted collection and must always
    // re-walk the disk, never serve the last walk — and a changed alias, path, or settings misses
    // and re-walks. Best-effort by design; everything evaporates on app exit.
    if store.load_collection_draft(&slug).map_err(cmd_err)?.is_none() {
        let cache = scan_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&slug) {
            if hit.path == opts.path && hit.include == opts.include && hit.exclude == opts.exclude {
                return Ok(hit.collection.clone());
            }
        }
    }

    let globs = build_glob_set(&opts.exclude)?;
    let include = IncludeSet::new(opts.include.clone());

    // Walk the filesystem off the async runtime thread under a hard deadline.
    // We deliberately avoid `tokio::time::timeout` + `tokio::task::spawn_blocking`:
    // those require the executing runtime's tokio time/blocking drivers, and when
    // that requirement isn't met the command panics. Release builds set
    // `windows_subsystem = "windows"` (no console), so such a panic is silent — the
    // IPC response is never sent and the dialog hangs on "Scanning…" forever.
    // Tauri's own `spawn_blocking` plus a std `recv_timeout` deadline has no such
    // hidden runtime dependency.
    let scan = move || -> anyhow::Result<SelectiveScan> {
        anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
        scan_selective_sized(&root, &include, &globs)
    };
    let SelectiveScan { items: listing, total_bytes, oversized } = match tauri::async_runtime::spawn_blocking(move || {
        run_blocking_with_deadline(scan, std::time::Duration::from_secs(30)).map_err(|e| {
            // Preserve the prior caller-facing timeout copy.
            if e == DEADLINE_EXCEEDED {
                "Directory scan timed out after 30 seconds — check that the path is accessible and try again."
                    .to_string()
            } else {
                e
            }
        })
    })
    .await
    {
        Ok(inner) => inner?,
        Err(e) => return Err(format!("Scan task failed: {e}")),
    };
    // QURATOR-336: an oversized scan hands back the count the estimator had reached when it crossed
    // `OVERSIZED_ITEM_THRESHOLD` — a **LOWER BOUND**, which is what the UI renders as "n+ items" —
    // rather than `count_items` of the teaser (that would be the teaser's count, not the
    // collection's). Persisting this onto `Collection::item_count` is also how the oversized bit
    // reaches the published listing: `collection_is_oversized` reads it back at publish time.
    let item_count = match oversized {
        Some(lower_bound) => lower_bound,
        None => count_items(&listing),
    };
    let est_size = if total_bytes > 0 { Some(format_size(total_bytes)) } else { None };

    let mut collection = Collection {
        slug,
        path_alias,
        description: None,
        item_count,
        est_size,
        content_types: vec![],
        tags: vec![],
        languages: vec![],
        // A freshly-scanned collection is Public by default; the user opts a collection into
        // Private explicitly via the visibility selector (M10). A rescan preserves the prior
        // visibility below (alongside notes + the sorted flag).
        visibility: Visibility::Public,
        // The `sorted` browse signal (#7) is a user declaration, not derived from the scan — default
        // false on a fresh scan; a rescan preserves the prior value below.
        sorted: false,
        last_updated: chrono::Utc::now(),
        listing,
    };

    // Preserve per-item notes AND the prior visibility + sorted flag from the existing draft (rescan
    // scenario) — a rescan must never silently flip a Private collection back to Public (that would
    // re-publish privately-marked data on the public path next publish) nor drop the sorted signal.
    if let Ok(Some(prev)) = store.load_collection_draft(&collection.slug) {
        let notes = collect_notes(&prev.listing, "");
        collection.listing = apply_notes(collection.listing, &notes, "");
        collection.visibility = prev.visibility;
        collection.sorted = prev.sorted;
    }

    collection.clamp_metadata();

    // QURATOR-207: no durable write here — the scan inputs ride along in the cache so Publish can
    // persist the draft, the share root, and the scan spec in one place (the single durable write,
    // [`promote_cached_scan`]). A Close/Cancel before Publish discards this entry, which is the
    // design: nothing was written, so there is nothing to clean up.
    scan_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            collection.slug.clone(),
            CachedScan {
                path: opts.path.clone(),
                include: opts.include.clone(),
                exclude: opts.exclude.clone(),
                total_bytes,
                collection: collection.clone(),
            },
        );

    Ok(collection)
}

/// Re-scan a published collection's source tree using its persisted [`ScanSpec`], returning the
/// freshly-scanned directory tree (notes preserved from the existing draft). Returns `Ok(None)` if
/// the collection has no scan spec (e.g. a pre-M9 draft) — the watch then skips it. Touches the
/// filesystem (under the same 30 s deadline as the initial scan) but **not** the network, so the
/// re-scan decision is testable without a relay.
pub(crate) fn rescan_listing(slug: &str, store: &DataStore) -> Result<Option<Vec<DirectoryItem>>, String> {
    let Some(spec) = store.load_scan_spec(slug).map_err(cmd_err)? else {
        return Ok(None);
    };
    let root = std::path::PathBuf::from(&spec.root);
    let globs = build_glob_set(&spec.exclude)?;
    let include = IncludeSet::new(spec.include.clone());
    let scan = move || -> anyhow::Result<SelectiveScan> {
        anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
        scan_selective_sized(&root, &include, &globs)
    };
    let SelectiveScan { items: mut listing, oversized, .. } =
        run_blocking_with_deadline(scan, std::time::Duration::from_secs(30)).map_err(|e| {
            if e == DEADLINE_EXCEEDED {
                format!("re-scan of '{slug}' timed out after 30 seconds")
            } else {
                e
            }
        })?;
    // Preserve per-item notes from the existing draft (same as the manual rescan path).
    if let Ok(Some(prev)) = store.load_collection_draft(slug) {
        let notes = collect_notes(&prev.listing, "");
        listing = apply_notes(listing, &notes, "");
    }
    // QURATOR-336: a re-scan owns the draft's `item_count`, in BOTH directions. Oversized ⇒ the
    // estimator's **lower bound**, which is both what the UI renders as "n+ items" and what carries
    // the oversized bit into the published listing (`collection_is_oversized` derives from it).
    // Not oversized ⇒ the real count — which is what UN-marks a collection that shrank back under
    // the threshold. `RelayPublishSink::republish` (watch.rs) therefore no longer recounts: a
    // recount of the capped preview would drop an oversized collection below the threshold and
    // silently un-mark it.
    let item_count = oversized.unwrap_or_else(|| count_items(&listing));
    if let Ok(Some(mut draft)) = store.load_collection_draft(slug) {
        if draft.item_count != item_count {
            draft.item_count = item_count;
            store.save_collection_draft(&draft).map_err(cmd_err)?;
        }
    }
    Ok(Some(listing))
}

#[tauri::command]
pub async fn delete_collection(
    slug: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;
    // QURATOR-249 (teardown half): a reserved-marker slug has no collection listing to unpublish —
    // its `is_published` bit is the profile teaser's marker read through the key collision — so
    // bypass `unpublish_collection_inner` (which refuses these slugs) and remove only the draft's
    // OWN sidecars, sparing `published/<slug>.json`: that file is the live profile teaser's
    // marker, and `DataStore::delete_collection` sweeps it (INV-8). A blanket refusal here
    // instead would strand the legacy draft as undeletable for every user whose profile is
    // published, which the permissive `is_valid_slug` above deliberately avoids. QURATOR-288: this
    // branch used to hand-roll its own four-path list, which could silently drift from the store's
    // five-path sweep the day a sixth sidecar appeared. Both now consume the store's single
    // `collection_sidecars` list, so drift is impossible by construction rather than watched for.
    if is_reserved_marker_slug(safe_slug) {
        store.delete_collection_sparing_published_marker(safe_slug).map_err(cmd_err)?;
        return Ok(());
    }
    // devtest #11 + QURATOR-138 (owner ruling 2026-08-30): a published collection contributes to
    // the profile teaser's content_types union and still has listing events on relays. Unpublish it
    // first (tombstone + NIP-09 delete + teaser content_types/tags recompute, all best-effort
    // offline) so removal drops it from the public teaser AND from relays, then delete the local
    // draft/marker — the local record goes too. This never touches the user's files (only the
    // local record and the published event), so QURATOR-202 (owner ruling) drops the confirm step
    // in the UI — no INV-8 confirmation gate here.
    if store.is_published(safe_slug) {
        let (id_clone, key_clone) = {
            let guard = identity.read().await;
            let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
            (id.identity.clone(), *id.browse_key.bytes())
        };
        unpublish_collection_inner(safe_slug, &store, &id_clone, &key_clone, &relay).await?;
    }
    store.delete_collection(safe_slug).map_err(cmd_err)
}

#[tauri::command]
pub async fn get_collections(store: State<'_, DataStore>) -> CmdResult<Vec<CollectionEntry>> {
    // Every collection is a local draft; `published` reflects whether a listing was published.
    let mut entries: Vec<CollectionEntry> = Vec::new();
    for slug in store.list_collection_slugs().map_err(cmd_err)? {
        if let Ok(Some(col)) = store.load_collection_draft(&slug) {
            let published = store.is_published(&slug);
            // Byte total from the per-slug sidecar (devtest 2026-06-25 #5) — 0 if never scanned with
            // the field (pre-existing spec) so the UI shows "—" rather than a wrong number.
            let total_bytes =
                store.load_scan_spec(&slug).ok().flatten().map(|s| s.total_bytes).unwrap_or(0);
            entries.push(CollectionEntry { collection: col, published, total_bytes });
        }
    }
    entries.sort_by(|a, b| a.collection.path_alias.cmp(&b.collection.path_alias));
    Ok(entries)
}

/// Roots with an accessibility stat currently in flight — dedups concurrent/repeated checks for the
/// same source root so a dead mount (whose `metadata()` blocks past our timeout and keeps running
/// detached) can't pile up blocking threads across the frontend's periodic re-checks (codex review).
fn inflight_access_stats() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Whether a collection's **source root** is currently reachable — a **timeout-bounded** metadata stat,
/// so an unreachable or slow SMB / removable mount can't hang the caller (the freeze this fixes). `true`
/// = reachable; `false` = unreachable, slow past the timeout, a stat already in flight, or the check
/// couldn't run. A collection with no recorded scan root reports `true` (nothing to gate on). The Home
/// list greys a collection until this returns `true`, and re-checks on a slow tick so a mount coming
/// back online fills back in.
#[tauri::command]
pub async fn collection_source_accessible(slug: String, store: State<'_, DataStore>) -> CmdResult<bool> {
    // QURATOR-270 (sibling site): the slug is IPC-supplied and `scan_spec_path` joins it naively
    // into `collections/<slug>.scan.json`, so an unguarded traversal slug reads a parsed file
    // outside the store — an existence/parse oracle. Same guard, same message as every other
    // supplied-slug command.
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;
    let Some(spec) = store.load_scan_spec(safe_slug).map_err(cmd_err)? else {
        return Ok(true);
    };
    if spec.root.is_empty() {
        return Ok(true);
    }
    let root = spec.root;
    // Dedup: if a stat for this root is already running (e.g. a prior dead-mount check still blocked in
    // the OS), don't launch another — report "not reachable (yet)" so detached blocking stats can never
    // accumulate. The running task removes the key when the OS finally returns.
    {
        let mut set = inflight_access_stats().lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(root.clone()) {
            return Ok(false);
        }
    }
    let root_for_task = root;
    let handle = tokio::task::spawn_blocking(move || {
        let reachable = std::path::Path::new(&root_for_task).metadata().is_ok();
        inflight_access_stats().lock().unwrap_or_else(|e| e.into_inner()).remove(&root_for_task);
        reachable
    });
    // The stat is bounded by a timeout — a dead mount blocks the OS call, but we stop waiting and report
    // "not reachable (yet)". Even on our timeout the detached task runs to completion and self-removes
    // from the in-flight set, so at most ONE blocking stat per root exists at a time.
    match tokio::time::timeout(std::time::Duration::from_secs(6), handle).await {
        Ok(Ok(reachable)) => Ok(reachable),
        _ => Ok(false),
    }
}

/// Update the editable metadata fields of a collection draft.
#[tauri::command]
pub async fn update_collection_meta(
    slug: String,
    description: Option<String>,
    content_types: Vec<String>,
    tags: Vec<String>,
    languages: Vec<String>,
    // The `sorted` browse signal (#7). The frontend already sent this in every call; it was silently
    // dropped until the command accepted it (there was no parameter to bind to).
    sorted: bool,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;

    // Load the draft, update fields, and re-save. QURATOR-207: before Publish there is no durable
    // record — a freshly-scanned collection lives in the in-memory scan cache, so the Details
    // form's edits land there until Publish promotes them; an already-persisted collection (the
    // edit flow) still writes the store directly below.
    let mut col = match store.load_collection_draft(safe_slug).map_err(cmd_err)? {
        Some(col) => col,
        None => {
            let mut cache = scan_cache().lock().unwrap_or_else(|e| e.into_inner());
            let Some(cached) = cache.get_mut(safe_slug) else {
                return Err(format!("No draft found for collection '{safe_slug}'"));
            };
            cached.collection.description = description;
            cached.collection.content_types = content_types;
            cached.collection.tags = tags;
            cached.collection.languages = languages;
            cached.collection.sorted = sorted;
            // Same publish-budget clamp as the persisted path below.
            cached.collection.clamp_metadata();
            return Ok(());
        }
    };

    col.description = description;
    col.content_types = content_types;
    col.tags = tags;
    col.languages = languages;
    col.sorted = sorted;
    // The metadata and the directory tree share one 40 KB publish budget, and the envelope is
    // measured first — uncapped metadata starves the tree (see `Collection::clamp_metadata`).
    col.clamp_metadata();
    store.save_collection_draft(&col).map_err(cmd_err)
}

/// Whether a draft is one the scan marked **oversized** (QURATOR-336): too big to ever be sent in
/// full, so it is published as a bounded breadth-first teaser.
///
/// ⚠ CARRIER (QURATOR-336 slice A). The owner's design puts `oversized: bool` on
/// `hb_core::Collection` with `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`. That
/// field is OWED, not built here: adding a field to `Collection` is not a local edit — Rust has no
/// defaulted struct-literal field, so every `Collection { … }` literal in the workspace (~30 across
/// a dozen modules, several of them files another lane holds, `browse.rs` included) would have to
/// grow an initialiser in the same commit. Slice A therefore derives the bit from the count the
/// scan already persists, and the migration is one line: this body becomes `col.oversized`, and the
/// injection in `collection_to_listing_json` goes away.
///
/// The derivation is unambiguous because the scan writes the estimator's **lower bound** into
/// `item_count` for an oversized tree (always `> OVERSIZED_ITEM_THRESHOLD`), while the full-scan
/// path refuses anything past [`MAX_COLLECTION_ITEMS`] = 100,000 — so no collection that fits can
/// ever be mistaken for one that does not.
///
/// Wire rule (CLAUDE.md §6): `oversized` is a new OPTIONAL listing field, and absent means today's
/// behaviour — peers may still ask, and the owner's own seal refuses anything past the 16 MiB
/// ceiling — so there is no weaker path to fall into and no discriminant bump.
fn collection_is_oversized(col: &Collection) -> bool {
    col.item_count > OVERSIZED_ITEM_THRESHOLD
}

/// Map a `Collection` draft to the render-model listing JSON: the directory tree moves from
/// `listing` to `entries` (what `hb-net::render_listing` consumes), the rest stays as metadata.
/// Pure — unit-tested without a relay.
pub(crate) fn collection_to_listing_json(mut col: Collection) -> Result<String, String> {
    // The single choke point for every published envelope — public listings, per-recipient private
    // ones, and `.hbmanifest` exports all arrive here — so it is where the metadata gets bounded.
    // `col` is taken by value precisely so this cannot touch anything the caller keeps.
    col.clamp_for_publish();
    let mut v = serde_json::to_value(&col).map_err(cmd_err)?;
    if let serde_json::Value::Object(ref mut map) = v {
        if let Some(listing) = map.remove("listing") {
            map.insert("entries".into(), listing);
        }
        // QURATOR-336: the oversized bit rides in the listing metadata — extra meta keys are the
        // established shape here (`snapshot_fingerprint` below is one), `truncate_listing` clones
        // the object minus `entries` so it preserves them, and `split_listing` carries them into
        // every part's meta too. Inserted only when true, the `skip_serializing_if` half of the
        // owed field: a listing that is not oversized keeps exactly the shape it had before this
        // change, so nothing downstream has to learn a new key to keep working.
        if collection_is_oversized(&col) {
            map.insert("oversized".into(), serde_json::Value::Bool(true));
            // The Browse teaser reads `total_items` for its "N+ items" lower bound. The BFS preview
            // is built to fit the teaser budget, so `truncate_listing` usually never truncates it and
            // never stamps a count — stamp the estimator's lower bound here instead. If truncation
            // does fire it overwrites this with the preview's own node count, which is why the UI
            // treats the figure as a lower bound in every case.
            map.insert("total_items".into(), serde_json::Value::from(col.item_count));
        }
        // M16 W3: stamp the full-tree snapshot fingerprint into the listing metadata so it rides —
        // through `truncate_listing` (the paywall teaser) and `split_listing` (the big-relay full
        // family, W2) — into `RenderedListing.meta`, where the browse-side staleness gate reads it.
        // An order-independent content hash of the whole tree.
        //
        // Audit #25 / QURATOR-123 (owner ruling 2026-08-25): the truncated teaser must NOT carry
        // this value — a digest of the exact content the truncation hides is an offline
        // confirm-or-deny oracle. `hb_net::truncate_listing` (the single choke point every truncated
        // teaser passes through) re-stamps the field to `hb_core::teaser_fingerprint` (visible
        // entries + elided count), so this stamp survives verbatim only where nothing is hidden: an
        // untruncated listing (identical by construction — `teaser_fingerprint(_, 0)` equals this)
        // and the full carriers (the big-relay family and the `.hbmanifest`, whose own gates use the
        // `teaser_fingerprint` stamped below).
        let fp = hb_core::snapshot_fingerprint(&col.listing);
        map.insert("snapshot_fingerprint".into(), serde_json::Value::String(fp.0));
    }
    serde_json::to_string(&v).map_err(cmd_err)
}

/// Validate + load a draft and produce its listing JSON, ready to publish. Pure (no relay) so the
/// validation paths are L1-testable.
pub(crate) fn prepare_listing(slug: &str, store: &DataStore) -> Result<String, String> {
    let safe_slug = is_valid_slug(slug).then_some(slug).ok_or("Invalid collection slug")?;
    // QURATOR-249: the reserved marker namespace is refused at scan time (the creation site) and
    // again here — the single validation every publish passes through — so a draft that reached
    // the store without scanning (a restored backup, or one created before the scan guard
    // existed) can still never write the colliding marker. The teardown half is closed
    // symmetrically: unpublish refuses these slugs and delete removes the draft while sparing
    // the marker (see `unpublish_collection_inner` / `delete_collection`), so a legacy draft
    // stays manageable — deletable, never publishable, never able to destroy the teaser marker.
    if is_reserved_marker_slug(safe_slug) {
        return Err(format!(
            "'{safe_slug}' is a reserved name — rename the collection before publishing it"
        ));
    }
    let collection = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?;
    if collection.content_types.is_empty() {
        return Err("At least one content type is required before publishing a collection.".into());
    }
    collection_to_listing_json(collection)
}

/// Current unix time in seconds (the seal/publish timestamp). A clock before 1970 reads as 0.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Collect the recipient pubkeys a **Private** collection must be sealed to: every `npub` in the
/// explicit Private audience (`private_audience.json`), parsed + deduped. Errs if the audience is
/// empty (publishing a Private collection to nobody is a mistake, not a silent no-op). An
/// unparseable id (e.g. a legacy non-Nostr contact) is skipped. Pure — unit-tested without a relay.
///
/// **M21 W5:** the audience is decoupled from contact groups (owner ruling 2026-08-04). Joining a
/// group or topic never enrols anyone here — only the explicit per-contact "Receives my Private
/// collections" toggle does. This closed the defect where filing a contact under a trusted group
/// silently made them a Private recipient.
pub(crate) fn private_recipients(store: &DataStore) -> Result<Vec<nostr::PublicKey>, String> {
    use std::collections::BTreeSet;
    let audience = store.load_private_audience().map_err(cmd_err)?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<nostr::PublicKey> = Vec::new();
    for npub in &audience {
        if seen.insert(npub.clone()) {
            if let Ok(pk) = hb_core::identity::parse_npub(npub) {
                out.push(pk);
            }
        }
    }
    if out.is_empty() {
        return Err(
            "This collection is Private, but you haven't chosen anyone to receive it. Open a \
             contact and turn on 'Receives my Private collections' before publishing."
                .into(),
        );
    }
    Ok(out)
}

/// The explicit "revoked during publish" outcome (CWE-367): a concurrent Unpublish bumped the
/// revocation generation while this publish was writing to the relay, so the published marker must
/// NOT be re-created (INV-8: a deliberately-skipped save, reported — never a silent success).
fn revoked_during_publish(slug: &str) -> String {
    format!(
        "collection '{slug}' was unpublished while its publish was in flight; not re-creating the published marker"
    )
}

/// The marker-save tail shared by [`publish_collection_inner`] (public path) and
/// [`publish_private_collection_inner`] (private path) — the CWE-367 guarded save. The relay write
/// happens before the marker save, so an Unpublish issued mid-publish would otherwise be silently
/// undone by the save re-creating the marker. Extracted so the pinning test ends where production
/// ends (P-6). QURATOR-251: the outcome is surfaced to the caller — on [`PublishedSave::Revoked`]
/// the marker was deliberately NOT written (INV-8: a deliberately-skipped save, reported — never a
/// silent success) and each tier's save site adds its own tail: the PUBLIC path retracts the
/// listing it already pushed ([`save_public_marker_or_retract`]); the PRIVATE path only reports,
/// because its gift-wrapped events are authored by per-recipient ephemeral keys this identity
/// cannot NIP-09 (see `unpublish_collection_inner`'s doc — an honest limit, not a bug).
fn save_collection_marker_guarded(
    store: &DataStore,
    slug: &str,
    marker: &str,
    gen_at_start: u64,
) -> Result<PublishedSave, String> {
    store
        .save_published_guarded(slug, marker, gen_at_start)
        .map_err(cmd_err)
}

/// QURATOR-251 — the PUBLIC path's guarded-save tail: the store guard above, plus the retraction
/// its `Revoked` outcome now owes. The listing events this publish pushed (teaser + any big-relay
/// family) are already live on relays when the guard reports an Unpublish/Delete landed mid-flight;
/// skipping only the marker save left a state mismatch — local "not published", relay-visible
/// content. The Revoked branch therefore runs the SAME best-effort retraction Unpublish itself runs
/// ([`retract_public_listing`]: NIP-09 by `d`-tag + the QURATOR-138 tombstone, shared pool + big
/// relay) for exactly this slug, then reports the race. Honest limits, by ruling and by mechanism:
/// (1) NIP-09 is a REQUEST a relay may ignore (N5 — best-effort like all deletion here); the
/// tombstone replaces only on conforming relays. This closes the intent gap; it cannot promise the
/// bytes are unreadable anywhere — a public listing's browse key is a forwardable string, so
/// retraction here is a courtesy with no security value (CLAUDE.md revocation semantics).
/// (2) The marker this publish would have saved was never written, so `retract_public_listing`'s
/// big-relay half cannot read the family's target from it and falls back to the current setting —
/// bounded noise when the setting changed mid-race, same posture as a pre-marker-era unpublish.
/// (3) The retraction serves the UNPUBLISH's intent (the newest generation-moving event); a publish
/// of the same slug that STARTED after that unpublish and saved its own marker first would have
/// its listing superseded by this tombstone — a publish/unpublish/publish triple-race transient,
/// healed by the next fingerprint-change republish, and strictly better than the pre-251 state
/// where the losing publish's events stayed live with nothing pointing at them.
async fn save_public_marker_or_retract(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    browse_key: &BrowseKey,
    relay: &SharedRelay,
    marker: &str,
    gen_at_start: u64,
) -> Result<(), String> {
    if save_collection_marker_guarded(store, slug, marker, gen_at_start)?
        == PublishedSave::Revoked
    {
        // Best-effort, exactly like every other retraction call site: a failure to retract must
        // not mask the revocation report, which is the fact the user needs.
        if let Err(e) = retract_public_listing(slug, store, identity, browse_key, relay).await {
            tracing::warn!(
                "post-revocation retraction for '{slug}' failed ({e}); its listing may stay live on relays"
            );
        }
        return Err(revoked_during_publish(slug));
    }
    Ok(())
}

/// QURATOR-207: promote a scan waiting in the in-memory cache to the store — the single durable
/// write. Persists the draft, the share root, and the scan spec (exactly the writes the scan used
/// to do at scan time), then drops the cache entry. Returns `false` when no scan is waiting (an
/// already-persisted collection that was not re-scanned — the plain edit flow; a no-op).
///
/// Merge rule for a RESCAN of an already-persisted collection: listing-side fields come from the
/// freshly-walked cache copy, metadata-side fields from the persisted record. The Details form
/// wrote its edits to the persisted record (the draft exists), so taking the cache copy wholesale
/// would silently drop them; taking the record's tree would publish the stale pre-rescan walk.
fn promote_cached_scan(slug: &str, store: &DataStore) -> Result<bool, String> {
    // The ≥1-content-type publish gate (the ruling keeps it) runs BEFORE anything is consumed or
    // written: a gate-failing publish must persist nothing, and the waiting scan must stay in the
    // cache so the user can tick a content type in the Details form and retry without rescanning.
    // Same message as `prepare_listing`'s gate — one rule, two choke points.
    {
        let cache = scan_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cache.get(slug) {
            if cached.collection.content_types.is_empty() {
                // A rescan promoting over a persisted record takes its metadata (merge rule
                // below), so the RECORD's content types satisfy the gate too — the Details form
                // wrote them there. Refuse only when both are empty.
                let record_has_types = store
                    .load_collection_draft(slug)
                    .map_err(cmd_err)?
                    .is_some_and(|prev| !prev.content_types.is_empty());
                if !record_has_types {
                    return Err(
                        "At least one content type is required before publishing a collection."
                            .into(),
                    );
                }
            }
        }
    }
    let Some(scan) = scan_cache().lock().unwrap_or_else(|e| e.into_inner()).remove(slug) else {
        return Ok(false);
    };
    let mut col = scan.collection;
    if let Some(prev) = store.load_collection_draft(slug).map_err(cmd_err)? {
        col.description = prev.description;
        col.content_types = prev.content_types;
        col.tags = prev.tags;
        col.languages = prev.languages;
        col.visibility = prev.visibility;
        col.sorted = prev.sorted;
    }
    store.save_collection_draft(&col).map_err(cmd_err)?;

    // The on-disk root so the snapshot re-scan can find the tree again.
    let mut share = store
        .load_share_settings(slug)
        .map_err(cmd_err)?
        .unwrap_or_default();
    share.root_path = Some(scan.path.clone());
    store.save_share_settings(slug, &share).map_err(cmd_err)?;

    // M9: the exact scan parameters so the snapshot watch can faithfully re-scan this tree
    // (same root, same checked folders, same exclusions) when the source changes.
    store
        .save_scan_spec(
            slug,
            &crate::store::ScanSpec {
                root: scan.path,
                include: scan.include,
                exclude: scan.exclude,
                // Persisted here (never published) so get_collections can surface the aggregate
                // "Total Size" the home view reads from `total_bytes` (devtest 2026-06-25 #5).
                total_bytes: scan.total_bytes,
            },
        )
        .map_err(cmd_err)?;
    Ok(true)
}

/// Publish a collection's listing. **Branches on visibility (M10):** a *Public* collection is
/// encrypted once under the account browse-key (M3); a *Private* collection is sealed per recipient
/// in the Private audience (M21 W5) and gift-wrapped — the browse-key is **not** used and the public
/// teaser is **not** touched (no private holding leaks). Marks it published locally and (public only)
/// keeps a published
/// teaser's content_types current.
pub(crate) async fn publish_collection_inner(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    browse_key: &BrowseKey,
    relay: &SharedRelay,
    gen_at_start: u64,
) -> Result<PublishSummary, String> {
    // QURATOR-207: Publish is the single durable write — a scan waiting in the in-memory cache is
    // promoted to the store here (draft + share root + scan spec) before the listing is prepared.
    // A collection that was never scanned this session is already persisted; this is a no-op.
    promote_cached_scan(slug, store)?;
    let listing_json = prepare_listing(slug, store)?;

    // Visibility gate: a Private collection takes the sealed, per-recipient path and never touches
    // the browse-key or the public teaser (and is never truncated — trusted recipients get it all).
    // QURATOR-200: no fail-open here — `prepare_listing` above already proved the draft exists, so
    // a missing draft at this point means it was deleted mid-publish, and defaulting that to
    // Public would publish a public event for a collection that no longer exists.
    let draft = store
        .load_collection_draft(slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("No draft found for collection '{slug}'"))?;
    let visibility = draft.visibility;
    // QURATOR-336: an oversized draft's `listing` is the bounded breadth-first teaser, so there is
    // no complete listing to publish anywhere — see the big-relay gate below.
    let oversized = collection_is_oversized(&draft);
    if visibility == Visibility::Private {
        // QURATOR-200 (F10's sibling): republishing as Private must retract the PUBLIC listing the
        // previous publish left live. Without this the marker is overwritten with the private
        // shape, so no later unpublish can ever discover the public tier to retract — the public
        // listing is stranded on relays forever. Only a marker that RECORDS the public tier pulls
        // this (never a missing marker: that means "never published", and retraction would publish
        // a tombstone for a slug that never had a public event). It runs BEFORE the private
        // publish (INV-8 bias: a failed private publish must not leave the public listing
        // re-stranded) and touches no marker and no revocation generation, so the guarded
        // private-marker save below still sees the original `gen_at_start`.
        if marker_tier(store, slug)? == MarkerTier::Public {
            retract_public_listing(slug, store, identity, browse_key, relay).await?;
        }
        publish_private_collection_inner(slug, store, identity, &listing_json, relay, gen_at_start).await?;
        return Ok(PublishSummary::whole());
    }

    // M16 W3 — the big relay setting drives BOTH browse-side discovery (option b) and the Layer-3
    // publish below. Load it once (empty = feature off). Stamp it into the listing meta ONLY when the
    // listing will actually truncate (decided on the UNSTAMPED bytes): a whole listing needs no
    // big-relay discovery (its teaser IS the full tree), and stamping its ~40-byte URL could otherwise
    // tip a near-limit collection over the cap into an unnecessary teaser (Codex finding 3). A whole
    // listing is therefore left byte-identical.
    let big_relay_url = store.load_settings().map_err(cmd_err)?.unwrap_or_default().big_relay_url;
    let listing_json = stamp_for_teaser(&listing_json, &big_relay_url)?;

    let client = net::client(identity, store, relay).await.map_err(cmd_err)?;
    // devtest #7: publish a single event, truncated (paywall teaser) when the listing is too large,
    // instead of splitting it across many part events.
    let published =
        publish_listing_capped(&client, identity, slug, browse_key, &listing_json, LISTING_MAX_BYTES)
            .await
            .map_err(cmd_err)?;

    // M16 W3 classifier (Layer 3, purely additive): when this Public listing was truncated to a
    // paywall teaser AND a big relay is configured, ALSO publish the full split family to the big
    // relay only (`publish_listing_to` → `publish_to`, INV-5 — never the public pool, which keeps
    // just the teaser). The family goes through a **dedicated client connected only to the big relay**,
    // NOT `ensure_relays` on the shared pool (Codex finding 1: a big relay left in the shared pool
    // would let a later untargeted browse mix its family with public teasers and bypass the browse-side
    // completeness/fingerprint gate). A listing that fit whole, or an unset big relay, takes no
    // big-relay write. **Best-effort** — the teaser already went out above, so a big-relay hiccup must
    // not fail the whole publish; the browse side falls back to the teaser and the owner can re-publish.
    // The big relay actually targeted (trimmed), captured so it can be recorded in the marker below —
    // unpublish deletes from the relay the family WENT to, not merely the then-current setting.
    // QURATOR-336: an oversized collection publishes NO full family, so the classifier is fed a
    // `truncated` of `false` for it. Its draft listing is already the bounded teaser, and the family
    // carrier is read brows-side as the COMPLETE listing — publishing the teaser through it would
    // claim completeness for a tree nobody has the bytes for (and the teaser is untruncated, so it
    // carries no `truncated`/`total_items` markers to say otherwise).
    let big_target = big_relay_target(published.truncated && !oversized, &big_relay_url).map(str::to_string);
    let big_relay_parts = match big_target.as_deref() {
        Some(big) => {
            let relays = [big.to_string()];
            // A FRESH client per publish means a fresh write-rate bucket (its first 24 writes unpaced).
            // That's acceptable here on purpose: the big relay is OWNER-RUN (no ban risk — the token
            // bucket exists to avoid bans on relays we don't control), and its `strfry-bigrelay.conf`
            // sets generous server-side limits. Sharing the pool's limiter would re-introduce the
            // finding-1 pool pollution, so the dedicated client wins.
            match hb_net::RelayClient::connect(identity, &relays, net::RELAY_TIMEOUT).await {
                Ok(big_client) => {
                    let parts = match publish_listing_to(
                        &big_client, identity, slug, browse_key, &listing_json, LISTING_MAX_BYTES, &relays,
                    )
                    .await
                    {
                        Ok(family) => family.parts,
                        Err(e) => {
                            tracing::warn!("big-relay publish for '{slug}' failed ({e}); the teaser stands");
                            0
                        }
                    };
                    big_client.disconnect().await;
                    parts
                }
                Err(e) => {
                    tracing::warn!("big relay '{big}' unreachable ({e}); the teaser stands");
                    0
                }
            }
        }
        None => 0,
    };

    // Local published marker (the "published" badge + content_types union), now also recording the
    // truncation state + big-relay part count + the big relay the family was published to (Codex
    // re-review HIGH: unpublish reads this to delete from the ORIGINAL relay even if the setting later
    // changed). `big_relay_url` is empty when no family was published.
    let marker = serde_json::json!({
        "parts": published.parts,
        "truncated": published.truncated,
        "shown_items": published.shown_items,
        "total_items": published.total_items,
        "big_relay_parts": big_relay_parts,
        // Record the big relay whenever one was TARGETED (Codex round-3 HIGH), not only on full success:
        // a partial-family failure still leaves some parts on that relay, so unpublish must know where
        // to clean them up even if the setting later changes. Empty only when no big relay was targeted.
        "big_relay_url": big_target.as_deref().unwrap_or(""),
    })
    .to_string();
    save_public_marker_or_retract(slug, store, identity, browse_key, relay, &marker, gen_at_start)
        .await?;

    // M9: record the snapshot fingerprint of what we just published, so a later watch re-scan that
    // hashes equal is a no-op (the republish-storm guard) and a real change re-publishes exactly once.
    if let Ok(Some(col)) = store.load_collection_draft(slug) {
        let fp = hb_core::snapshot_fingerprint(&col.listing);
        let _ = store.save_snapshot_fingerprint(slug, &fp);
    }

    // Keep a published teaser's content_types/tags aggregation current (M13 W5 item 2 folds tags
    // into the union too — see `refresh_published_teaser`).
    refresh_published_teaser(store, identity, relay).await?;
    Ok(PublishSummary {
        truncated: published.truncated,
        shown_items: published.shown_items,
        total_items: published.total_items,
        big_relay_parts,
    })
}

/// If a profile teaser is currently published, recompute its content_types/tags aggregation and
/// republish it. Shared by [`publish_collection_inner`] (a newly-published collection may add to
/// the union) and `unpublish_collection_inner` (a departing collection may remove from it) — a
/// no-op when no profile teaser is published.
async fn refresh_published_teaser(
    store: &DataStore,
    identity: &Identity,
    relay: &SharedRelay,
) -> Result<(), String> {
    if !store.is_published("profile") {
        return Ok(());
    }
    // Capture the profile's revocation generation BEFORE the relay write, so a profile Unpublish
    // issued mid-refresh (`delete_published("profile")` bumps this generation) is detected at the
    // marker save and not silently undone — the same CWE-367 race `publish_collection_inner` guards.
    let gen_at_start = store.published_generation("profile");
    let Some(mut profile) = store.load_profile_draft().map_err(cmd_err)? else {
        return Ok(());
    };
    profile.content_types = compute_content_types(store);
    store.save_profile_draft(&profile).map_err(cmd_err)?;
    // devtest #5: keep the opt-out honored across a collection-triggered republish too — never
    // silently re-add hashtags a user turned off.
    let discoverable = store.load_settings().map_err(cmd_err)?.unwrap_or_default().discoverable;
    let teaser = teaser_from_profile(store, &profile);
    if let Ok(event) = hb_core::event::build_teaser(identity, &teaser, discoverable) {
        if let Ok(client) = net::client(identity, store, relay).await {
            let _ = client.publish(&event).await;
            if let Ok(json) = serde_json::to_string(&event) {
                // Guarded (CWE-367): `save_published_guarded` skips the write and returns `Revoked`
                // if the profile was unpublished mid-refresh, so this best-effort refresh cannot
                // silently resurrect a revoked marker.
                let _ = store.save_published_guarded("profile", &json, gen_at_start);
            }
        }
    }
    Ok(())
}

/// Seal + publish a Private collection (M10): one gift-wrapped (1059) event per trusted `npub`,
/// multi-published to all relays. The browse-key is unused; the public teaser is untouched.
async fn publish_private_collection_inner(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    listing_json: &str,
    relay: &SharedRelay,
    gen_at_start: u64,
) -> Result<(), String> {
    let recipients = private_recipients(store)?;
    let events = hb_core::seal_private_listing(identity, &recipients, listing_json, now_secs())
        .map_err(cmd_err)?;

    let client = net::client(identity, store, relay).await.map_err(cmd_err)?;
    hb_net::publish_private_listing(&client, &events).await.map_err(cmd_err)?;

    // Local published marker — records the *private* tier + the recipient count (the N× multiplier
    // INV-8 calls out), distinct from the public path's `parts`.
    let marker = serde_json::json!({ "private": true, "recipients": recipients.len() }).to_string();
    // QURATOR-251: same CWE-367 race as the public path, but the private tier CANNOT retract —
    // the wraps were just published under per-recipient ephemeral keys (M10), and a NIP-09
    // deletion must be signed by the target event's own author. The save is skipped and the race
    // reported; exclusion-going-forward is the per-recipient CEK wrap's job (CLAUDE.md: revocation
    // is meaningful only for private collections, enforced by construction).
    if save_collection_marker_guarded(store, slug, &marker, gen_at_start)? == PublishedSave::Revoked
    {
        return Err(revoked_during_publish(slug));
    }

    // M9 storm-guard fingerprint, same as the public path.
    if let Ok(Some(col)) = store.load_collection_draft(slug) {
        let fp = hb_core::snapshot_fingerprint(&col.listing);
        let _ = store.save_snapshot_fingerprint(slug, &fp);
    }
    Ok(())
}

#[tauri::command]
pub async fn publish_collection(
    slug: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<PublishSummary> {
    // QURATOR-270 (sibling site): `prepare_listing` guards the slug, but `published_generation`
    // below is read BEFORE it and joins naively into `published/<slug>.gen`, so an unguarded
    // traversal slug leaks that file's parsed integer. Guard at the command boundary instead.
    if !is_valid_slug(&slug) {
        return Err("Invalid collection slug".into());
    }
    let (id_clone, browse_key) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone())
    };
    let gen_at_start = store.published_generation(&slug);
    publish_collection_inner(&slug, &store, &id_clone, browse_key.bytes(), &relay, gen_at_start).await
}

/// Build the signed, browse-key-encrypted **manifest envelope** for a Public collection draft — the
/// `.hbmanifest` artifact (M16 W4). Its plaintext is the SAME canonical full listing JSON the publish
/// path feeds to `truncate_listing`, so an imported manifest's `snapshot_fingerprint` matches the
/// teaser's exactly (the browse-side staleness gate). Private collections are refused: they never
/// truncate (Private-audience recipients already receive the whole sealed listing) and are sealed per
/// recipient, not under the browse-key this envelope uses. Pure w.r.t. the relay — L1-testable with a
/// temp store.
pub(crate) fn build_slug_manifest(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    browse_key: &BrowseKey,
) -> Result<hb_core::manifest::ManifestEnvelope, String> {
    let safe_slug = is_valid_slug(slug).then_some(slug).ok_or("Invalid collection slug")?;
    let col = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?;
    if col.visibility == Visibility::Private {
        return Err("Private collections are sealed per recipient, not exported as a shared manifest — \
                    trusted contacts already receive the whole listing."
            .into());
    }
    // QURATOR-336: an oversized collection has no full listing to send — its draft holds the
    // bounded breadth-first teaser precisely because the whole tree cannot fit. Building the
    // envelope anyway would serialize and seal minutes of parts to produce something the 16 MiB
    // transport ceiling then refuses, so the owner is told up front instead of paying for it.
    if collection_is_oversized(&col) {
        return Err(format!(
            "'{safe_slug}' is too large to send in full (more than {OVERSIZED_ITEM_THRESHOLD} \
             items) — it is published as a preview, and no full manifest can fit the transport \
             ceiling. Split it into smaller collections to export one."
        ));
    }
    if col.content_types.is_empty() {
        return Err(
            "At least one content type is required before exporting a collection manifest.".into()
        );
    }
    // Audit #25 / QURATOR-123: the envelope's digest must be the one the truncated teaser carries
    // (visible entries + elided count), because the browse side gates staleness against the teaser
    // the user is looking at. Derive it by running the SAME truncation the teaser publish performs
    // on the SAME bytes, then reading the digest out of the artifact — not by re-implementing the
    // derivation. A listing that fits the budget whole carries the full-tree digest (nothing is
    // hidden), which is what the teaser carries then too, so parity holds on both branches.
    //
    // QURATOR-205: the mint path must stamp the SAME final bytes the publish path truncates — a big
    // relay URL rides into the listing meta before truncation and shifts the entries budget, so what
    // counts as "visible" (and therefore the digest) can differ between an unstamped mint and a
    // stamped publish of the SAME unchanged collection. `stamp_for_teaser` is the one function both
    // paths call, so they cannot drift apart again.
    let plaintext = collection_to_listing_json(col.clone())?;
    let big_relay_url = store.load_settings().map_err(cmd_err)?.unwrap_or_default().big_relay_url;
    let plaintext = stamp_for_teaser(&plaintext, &big_relay_url)?;
    let teaser = hb_net::truncate_listing(&plaintext, LISTING_MAX_BYTES).map_err(cmd_err)?;
    let fingerprint = serde_json::from_str::<serde_json::Value>(&teaser.json)
        .ok()
        .and_then(|v| v.get("snapshot_fingerprint").and_then(|f| f.as_str()).map(str::to_string))
        .unwrap_or_else(|| hb_core::snapshot_fingerprint(&col.listing).0);
    // Chunk the listing exactly like the big-relay carrier — split at the per-part NIP-44 budget, so a
    // `.hbmanifest` can hold a collection of ANY size (the envelope stores the encrypted parts inline,
    // bounded by part count, not by one event's plaintext cap). A listing that fits one event yields a
    // single part (the small-collection case, unchanged).
    let parts: Vec<String> = split_listing(safe_slug, &plaintext, MANIFEST_SPLIT_MAX_BYTES)
        .map_err(cmd_err)?
        .into_iter()
        .map(|part| part.json)
        .collect();
    hb_core::manifest::build_manifest_envelope(
        identity,
        safe_slug,
        browse_key,
        &fingerprint,
        now_secs(),
        &parts,
    )
    .map_err(cmd_err)
}

/// Export a collection's full-listing **manifest** to a user-picked `<slug>.hbmanifest` file (M16 W4).
/// **No longer the only route** (M18 W4): the fulfil verb sends the same manifest over the transport
/// plane, and this is its fallback for when that cannot connect. The hoarder hands the file over
/// however they like — Hoardbook writes the file the user chose and moves no *collection files*
/// (INV-4′). The JS side picks `path` via the save dialog, exactly like `backup_data`.
#[tauri::command]
pub async fn export_manifest(
    slug: String,
    path: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<()> {
    let (id_clone, browse_key) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone())
    };
    let envelope = build_slug_manifest(&slug, &store, &id_clone, browse_key.bytes())?;
    let json = envelope.to_json().map_err(cmd_err)?;
    std::fs::write(&path, json.as_bytes())
        .map_err(|e| format!("Could not write manifest file: {e}"))?;
    Ok(())
}

/// True iff a listing event's `d`-tag belongs to `slug`'s family — the index itself, or one of its
/// split parts (`hb_net::split::split_listing`'s `slug#part{i}` convention: the tail after `#part`
/// is a non-empty digit run, checked so a future `slug#part…`-rooted sidecar d-tag can't be swept
/// into unpublish — chorus M13 #3). Pins the M13 W5 unpublish deletion-targeting choice (see
/// `unpublish_collection_inner`'s doc comment): matched by `d`-tag at unpublish time rather than by
/// a persisted event-id list.
fn listing_dtag_belongs_to_slug(d: &str, slug: &str) -> bool {
    if d == slug {
        return true;
    }
    d.strip_prefix(slug)
        .and_then(|rest| rest.strip_prefix("#part"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The fetched listing events a retraction must NIP-09-delete for `slug` — its `d`-tag family
/// (the index + `slug#part{i}` split parts), and NOTHING else: another collection's events that
/// came back under the same author+kind filter must never be swept into a deletion (INV-8:
/// durable deletion is deliberate, never accidental — a revocation of "films", mid-publish or not,
/// has no business deleting "music"). Extracted from the two identical selection loops in
/// [`retract_public_listing`] (shared pool + big relay) — and reached by the QURATOR-251
/// revoked-during-publish retraction through that same function — so the no-over-delete property
/// is pinnable at the Event level without a live relay (P-6).
fn retraction_targets<'a>(
    events: &'a [Event],
    slug: &'a str,
) -> impl Iterator<Item = &'a Event> + 'a {
    events.iter().filter(move |ev| {
        ev.tags
            .identifier()
            .is_some_and(|d| listing_dtag_belongs_to_slug(d, slug))
    })
}

/// The tier a collection's published marker records (QURATOR-200 / F10): the AUTHORITATIVE record
/// of what is actually live on relays, written at publish time — the public shape by
/// [`publish_collection_inner`] (`{"parts":…}`), the private shape by
/// [`publish_private_collection_inner`] (`{"private":true,"recipients":…}`). Retraction decisions
/// must read THIS, never a re-read of the mutable draft: the F10 sequence (publish Public → flip
/// the draft to Private → unpublish/delete) used to read Private off the draft and skip the entire
/// public retraction while the original public listing stayed live on relays — an INV-8 failure
/// (durable public data surviving a deliberate deletion) wearing a privacy regression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerTier {
    /// The marker records the public tier (or is indeterminate — see [`marker_tier`]).
    Public,
    /// The marker records the private tier (`"private": true`).
    Private,
    /// No marker exists: nothing this build ever published under this slug.
    Unpublished,
}

/// Classify a collection's published marker into its recorded tier (QURATOR-200 / F10).
///
/// **Indeterminate markers fail toward [`MarkerTier::Public`], and that is a decision, not the old
/// code's reflex:** markers written by older builds may carry neither the public keys (`parts`, …)
/// nor `"private": true`. The two misclassification directions are asymmetric. Reading such a
/// marker as Private permanently strands a live public event through a deliberate deletion — this
/// very bug. Reading a genuinely-private marker as Public costs one best-effort author+kind relay
/// fetch that matches nothing by `d`-tag (private listings are gift-wrapped under per-recipient
/// ephemeral keys, so they never appear under this identity's author filter) plus a tombstone that
/// replaces nothing — bounded noise, no invariant broken, no data lost. Only
/// [`publish_private_collection_inner`] ever writes `"private": true`, so its presence is a
/// positive, unambiguous signal.
fn marker_tier(store: &DataStore, slug: &str) -> Result<MarkerTier, String> {
    let marker = store.load_published(slug).map_err(cmd_err)?;
    Ok(match marker.as_deref() {
        None => MarkerTier::Unpublished,
        Some(m) => {
            let private = serde_json::from_str::<serde_json::Value>(m)
                .ok()
                .and_then(|v| v.get("private").and_then(|p| p.as_bool()))
                .unwrap_or(false);
            if private { MarkerTier::Private } else { MarkerTier::Public }
        }
    })
}

/// The public-tier retraction proper (QURATOR-200): NIP-09-delete the author's whole `d`-tag
/// listing family from the shared pool, publish the QURATOR-138 tombstone, then repeat both on the
/// big relay the marker recorded (falling back to the current setting). Extracted VERBATIM from
/// `unpublish_collection_inner` — behaviour unchanged — so `publish_collection_inner`'s Private
/// branch can run the SAME retraction when a republish flips a previously-public collection to
/// Private. Best-effort throughout (a relay MAY ignore a NIP-09 request; the tombstone is the
/// enforced half). Touches no local state and bumps no revocation generation: the marker drop
/// stays with the callers, so a publish-side call cannot trip the CWE-367 guard.
async fn retract_public_listing(
    safe_slug: &str,
    store: &DataStore,
    identity: &Identity,
    browse_key: &BrowseKey,
    relay: &SharedRelay,
) -> Result<(), String> {
    if let Ok(client) = net::client(identity, store, relay).await {
        let filter =
            Filter::new().author(identity.public_key()).kind(Kind::from_u16(hb_core::event::KIND_LISTING));
        if let Ok(events) = client.fetch(filter, net::RELAY_TIMEOUT).await {
            for ev in retraction_targets(&events, safe_slug) {
                if let Ok(deletion) = hb_net::build_deletion(identity, ev) {
                    let _ = client.publish(&deletion).await;
                }
            }
        }
        // QURATOR-138 (owner ruling 2026-08-30) — the TOMBSTONE, published to the shared pool
        // AFTER the NIP-09 loop above (the fetch would otherwise find our own tombstone and
        // NIP-09-delete it). KIND_LISTING is parameterized-replaceable (`d` = slug), so a zeroed
        // listing at `created_at = now` makes every conforming relay REPLACE the published
        // listing — enforced, unlike the NIP-09 request a relay MAY ignore. It reaches clients
        // that re-fetch; a peer who already fetched keeps their copy (the Delete confirmation
        // says exactly that — never promise retraction the mechanism cannot deliver). Sealed
        // under the SAME browse key so a share-code holder decrypts an empty collection rather
        // than a locked one. Best-effort, same posture as the NIP-09 half.
        if let Ok(tombstone) = hb_core::event::build_tombstone_event(identity, safe_slug, browse_key)
        {
            let _ = client.publish(&tombstone).await;
        }
    }

    // Codex finding 2: the big-relay full family (published to the big relay ONLY, W3) is invisible
    // to the shared pool above — delete it from the big relay too, through a dedicated client, so an
    // unpublished collection isn't left readable there by share-code holders who know the URL. The
    // family events are authored by THIS identity, so this identity can NIP-09 them. Best-effort.
    //
    // Codex re-review HIGH: delete from the relay the family was ACTUALLY published to — recorded in
    // the marker at publish time — NOT merely the current setting, which the owner may have changed
    // or cleared since. Fall back to the current setting for a listing published before the marker
    // carried the URL.
    let recorded_big = store
        .load_published(safe_slug)
        .ok()
        .flatten()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
        .and_then(|v| v.get("big_relay_url").and_then(|u| u.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty());
    let big = match recorded_big {
        Some(url) => url.trim().to_string(),
        None => store
            .load_settings()
            .map_err(cmd_err)?
            .unwrap_or_default()
            .big_relay_url
            .trim()
            .to_string(),
    };
    if !big.is_empty() {
        let relays = [big];
        if let Ok(big_client) =
            hb_net::RelayClient::connect(identity, &relays, net::RELAY_TIMEOUT).await
        {
            let filter = Filter::new()
                .author(identity.public_key())
                .kind(Kind::from_u16(hb_core::event::KIND_LISTING));
            if let Ok(events) = big_client.fetch_from(&relays, filter, net::RELAY_TIMEOUT).await {
                for ev in retraction_targets(&events, safe_slug) {
                    if let Ok(deletion) = hb_net::build_deletion(identity, ev) {
                        let _ = big_client.publish_to(&deletion, &relays).await;
                    }
                }
            }
            // QURATOR-138: the tombstone goes to the big relay too — same relay the family was
            // actually published to, AFTER the NIP-09 loop (the fetch above must not see and
            // delete our own replacement event). Best-effort, identical posture to the pool half.
            if let Ok(tombstone) =
                hb_core::event::build_tombstone_event(identity, safe_slug, browse_key)
            {
                let _ = big_client.publish_to(&tombstone, &relays).await;
            }
            big_client.disconnect().await;
        }
    }
    Ok(())
}

/// Unpublish a collection (spec §4 Unpublish): NIP-09 delete every relay event for a **Public**
/// collection (best-effort — the index + every split part), drop the local published marker (which
/// alone stops the watch's auto-republish: `evaluate_rescan` returns `Skipped("not published")`
/// once `is_published` is false — `watch.rs` needs no change), and refresh the profile teaser when
/// one is published (a departing collection must drop its content_types/tags from the union — M13
/// W5 item 2, via [`refresh_published_teaser`]).
///
/// **Deletion-targeting design:** rather than changing the published marker to carry event ids
/// (which would require `hb_net::browse::PublishedListing` to expose the signed events it builds —
/// an `hb-net` API change outside this workstream's file ownership; today the marker carries only a
/// part *count*), this queries the author's own `KIND_LISTING` events at unpublish time and matches
/// them by `d`-tag ([`listing_dtag_belongs_to_slug`]), mirroring the exact family-grouping
/// `hb_net::browse::fetch_listing` already does on the read side. This needs no `hb-net` change, and
/// it also finds a listing published before this feature existed (no marker-format migration).
///
/// **Private collections**: a gift-wrapped (1059) event is authored by a fresh **ephemeral** key
/// per recipient (M10) — this identity cannot produce a valid NIP-09 for it (a deletion request
/// must be signed by the target event's own author). The private path is therefore local-only
/// (marker drop only): an honest limit, not a bug. **Which tier applies is decided by
/// [`marker_tier`] — the PUBLISHED MARKER's recorded tier — never by the draft** (QURATOR-200/F10:
/// a collection published Public and later flipped Private in the draft is still a public listing
/// on relays and must be retracted as one).
pub(crate) async fn unpublish_collection_inner(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    browse_key: &BrowseKey,
    relay: &SharedRelay,
) -> Result<(), String> {
    let safe_slug = is_valid_slug(slug).then_some(slug).ok_or("Invalid collection slug")?;

    // QURATOR-249 (teardown half): "profile" is charset-VALID, so `is_valid_slug` alone admits a
    // legacy draft (restored from backup, or created before the scan guard) — and this function
    // would then treat the profile teaser's marker as a collection marker: `delete_published`
    // below destroys the LIVE teaser marker (INV-8 — `refresh_published_teaser` gates on it and
    // the profile-unpublish path needs it to find the still-live event), and
    // `retract_public_listing` aims tombstone/NIP-09 traffic at the reserved key. No collection
    // listing can exist under this key (the scan + publish guards close both creation routes),
    // so there is nothing legitimate to unpublish: refuse here, before any store read or relay
    // access. `delete_collection` deliberately bypasses this refusal for reserved slugs — see its
    // own QURATOR-249 branch — so a legacy draft stays deletable instead of stranded.
    if is_reserved_marker_slug(safe_slug) {
        return Err(format!(
            "'{safe_slug}' is a reserved name — its published marker is the profile teaser's, \
             not a collection's; deleting the draft is still allowed, but unpublishing it is not"
        ));
    }

    // QURATOR-200 (F10): the retraction tier comes from the PUBLISHED MARKER — the record of what
    // is actually live on relays — never from a re-read of the mutable draft. The F10 sequence
    // (publish Public → flip the draft Private → delete) used to read Private off the draft and
    // skip the entire retraction, leaving the public listing live on relays: an INV-8 failure. A
    // marker that is missing entirely ALSO retracts: `delete_collection` only reaches this
    // function with a marker present, an explicit unpublish must still find a pre-marker-era
    // public listing, and the wrong guess costs one fetch that matches nothing by `d`-tag — while
    // the other direction is this bug.
    let tier = marker_tier(store, safe_slug)?;
    if tier != MarkerTier::Private {
        retract_public_listing(safe_slug, store, identity, browse_key, relay).await?;
    }

    store.delete_published(safe_slug).map_err(cmd_err)?;
    refresh_published_teaser(store, identity, relay).await
}

#[tauri::command]
pub async fn unpublish_collection(
    slug: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let (id_clone, key_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), *id.browse_key.bytes())
    };
    unpublish_collection_inner(&slug, &store, &id_clone, &key_clone, &relay).await
}

/// Set a collection's visibility (Public / Private). The selector default is Public; a collection
/// becomes Private only by explicit choice (M10). The next publish honours the new visibility.
#[tauri::command]
pub async fn update_collection_visibility(
    slug: String,
    visibility: Visibility,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;
    let mut col = match store.load_collection_draft(safe_slug).map_err(cmd_err)? {
        Some(col) => col,
        None => {
            // QURATOR-207: a scanned-but-unpublished collection lives in the in-memory scan cache
            // (see `update_collection_meta`) — the visibility choice rides to Publish with it.
            let mut cache = scan_cache().lock().unwrap_or_else(|e| e.into_inner());
            let Some(cached) = cache.get_mut(safe_slug) else {
                return Err(format!("No draft found for collection '{safe_slug}'"));
            };
            cached.collection.visibility = visibility;
            return Ok(());
        }
    };
    col.visibility = visibility;
    store.save_collection_draft(&col).map_err(cmd_err)
}

/// Export a collection's listing as plain text or markdown checklist.
/// Returns the rendered string; the caller writes it to clipboard.
#[tauri::command]
pub async fn export_collection(
    slug: String,
    format: String,
    store: State<'_, DataStore>,
) -> CmdResult<String> {
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;

    let collection: Collection = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Collection '{safe_slug}' not found"))?;

    // QURATOR-254 (audit #18's missed sibling): `path_alias` is the first line of the exported
    // string and reaches this point un-neutralized — it is bound from webview IPC at scan time
    // (and a restored backup's draft is never field-validated), so an alias like
    // `Films\n\n![p](https://…)` would forge rows and live markup in a checklist the user is
    // told is safe to share. Item names already get the audit #18 treatment in `render_text` /
    // `render_markdown`; the alias now gets the identical one, matched to the same format.
    let (out, alias) = match format.as_str() {
        "markdown" => (
            render_markdown(&collection.listing, 0),
            markdown_escape(&collection.path_alias),
        ),
        _ => (render_text(&collection.listing, 0), strip_control(&collection.path_alias)),
    };

    Ok(format!("{alias}\n\n{out}"))
}

fn render_text(items: &[DirectoryItem], depth: usize) -> String {
    use hb_core::types::ItemType;
    let indent = "  ".repeat(depth);
    items
        .iter()
        .map(|item| {
            let prefix = if item.item_type == ItemType::Folder { "📁 " } else { "   " };
            let size = item.size.as_deref().map(|s| format!(" [{s}]")).unwrap_or_default();
            let children = if !item.children.is_empty() {
                format!("\n{}", render_text(&item.children, depth + 1))
            } else {
                String::new()
            };
            format!("{indent}{prefix}{}{size}{children}", strip_control(&item.name))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_markdown(items: &[DirectoryItem], depth: usize) -> String {
    use hb_core::types::ItemType;
    let indent = "  ".repeat(depth);
    items
        .iter()
        .map(|item| {
            if item.item_type == ItemType::Folder {
                let children = if !item.children.is_empty() {
                    format!("\n{}", render_markdown(&item.children, depth + 1))
                } else {
                    String::new()
                };
                format!("{indent}- **{}**{children}", markdown_escape(&item.name))
            } else {
                let mut meta = vec![];
                if let Some(fmt) = &item.format { meta.push(code_span_escape(fmt)); }
                if let Some(sz) = &item.size { meta.push(code_span_escape(sz)); }
                let meta_str = if meta.is_empty() { String::new() } else { format!(" `{}`", meta.join(", ")) };
                format!("{indent}- [ ] {}{meta_str}", markdown_escape(&item.name))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip control characters (newlines, tabs, and other C0/C1 controls) so an interpolated value
/// cannot forge additional rows in a plain-text export (audit #18, CWE-74).
fn strip_control(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Neutralize a value destined for a markdown *text* position: strip control characters (which
/// cannot be escaped and could forge rows), then backslash-escape markdown punctuation so the value
/// round-trips as literal text rather than being parsed as markup. `[2024] Album (FLAC)` still reads
/// as `[2024] Album (FLAC)`, while `![cov](https://…)` cannot become an image (audit #18, CWE-74).
fn markdown_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            continue;
        }
        match c {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '(' | ')' | '#'
            | '+' | '-' | '.' | '!' | '|' | '~' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Neutralize a value destined for an inline-code span (`` `…` ``): inside a code span only a
/// backtick or a control character can break out, so strip those. Backslash-escapes are inert inside
/// a code span and would render literally, mangling values like `14.2 GB` (audit #18, CWE-74).
fn code_span_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '`')
        .collect()
}

// ---------------------------------------------------------------------------
// Slug validation
// ---------------------------------------------------------------------------

/// A valid slug contains only Unicode alphanumerics and hyphens.
/// Path-traversal characters (`/`, `.`, `\`, `:`, `%`, NUL, whitespace) are
/// not alphanumeric in any Unicode category and are therefore rejected here,
/// preventing path traversal attacks (e.g., "../identity/keypair").
///
/// Canonical implementation: `hb_core::ticket::is_valid_slug` (QURATOR-259) — the SAME charset
/// `verify_shape` enforces on wire tickets, so a slug the wire accepts is never something this
/// node would refuse to create locally, and vice versa. This local gate delegates rather than
/// re-implementing (the `validate_relay_url` pattern: one implementation, every path).
pub(crate) fn is_valid_slug(slug: &str) -> bool {
    hb_core::ticket::is_valid_slug(slug)
}

/// Fixed keys sharing the flat `published/<key>.json` marker namespace (`DataStore::published_path`)
/// with collection slugs. The profile teaser marker is stored under `profile`, so a collection with
/// that slug would clobber it on publish — and vice versa — corrupting tier/big-relay bookkeeping
/// and flipping `is_published("profile")` on (QURATOR-249). Reserved at scan time, the single site
/// collection slugs are created.
const RESERVED_MARKER_SLUGS: &[&str] = &["profile"];

/// True for a charset-valid slug that is nonetheless a fixed marker key (see
/// [`RESERVED_MARKER_SLUGS`]) and must never become a collection.
fn is_reserved_marker_slug(slug: &str) -> bool {
    RESERVED_MARKER_SLUGS.contains(&slug)
}

// ---------------------------------------------------------------------------
// Filesystem scanner
// ---------------------------------------------------------------------------

/// Sentinel returned by `run_blocking_with_deadline` when the work outlives the deadline, so callers
/// can substitute their own user-facing copy.
const DEADLINE_EXCEEDED: &str = "__deadline_exceeded__";

/// Run a blocking `work` closure on its own thread and abandon it after `timeout`. A stale SMB mount
/// can wedge `read_dir` indefinitely; this guarantees the command returns instead of hanging the UI.
/// Pure (no async, no Tauri) so the deadline path is directly unit-testable.
fn run_blocking_with_deadline<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    timeout: std::time::Duration,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(cmd_err),
        Err(_) => Err(DEADLINE_EXCEEDED.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Selection-aware scanner (M8 — folder-tree picker, HANDOVER §A2.1)
// ---------------------------------------------------------------------------

/// The set of relative, "/"-separated directory paths the user checked in the folder-tree picker.
/// Mirrors the frontend `lib/scan-tree.ts` so backend and UI agree on inclusion semantics.
pub(crate) struct IncludeSet {
    checked: Vec<String>,
}

impl IncludeSet {
    pub(crate) fn new(checked: Vec<String>) -> Self {
        Self { checked }
    }

    /// `rel` is included iff it is itself checked or lives under a checked ancestor.
    pub(crate) fn is_included(&self, rel: &str) -> bool {
        self.checked
            .iter()
            .any(|c| rel == c || rel.starts_with(&format!("{c}/")))
    }

    /// True iff some checked path lives strictly below `rel` (so `rel` is only an *ancestor* of a
    /// selection — traverse it to reach the selection, but withhold its own loose files).
    pub(crate) fn has_descendant_under(&self, rel: &str) -> bool {
        let prefix = format!("{rel}/");
        self.checked.iter().any(|c| c.starts_with(&prefix))
    }
}

// ---------------------------------------------------------------------------
// The selection rules, as three pure predicates
// ---------------------------------------------------------------------------
//
// These are the *whole* of the selection logic `scan_selective_walk` applies, lifted out so the
// walk and the oversized estimator (QURATOR-336) cannot drift: two traversals that must agree on
// what a tree contains are one traversal too many when the rules are copied. Each is a one-line
// composition of the [`IncludeSet`] methods, so the rules still live in one place.

/// Whether loose files in the directory at `rel_prefix` are listed: the collection root and any
/// included directory list them; an ancestor-only directory withholds them (its files are reached
/// only by being checked individually).
fn lists_loose_files(rel_prefix: &str, include: &IncludeSet) -> bool {
    rel_prefix.is_empty() || include.is_included(rel_prefix)
}

/// Whether the traversal descends into the directory at `rel_path`: it is selected (whole subtree)
/// or is an ancestor of a selection (descend only to reach the checked descendant).
fn dir_is_descended(rel_path: &str, include: &IncludeSet) -> bool {
    include.is_included(rel_path) || include.has_descendant_under(rel_path)
}

/// Whether a FILE entry at `rel_path` is listed: it lives in a directory that lists loose files
/// (`loose`), or it is itself checked (devtest #10).
fn file_is_listed(rel_path: &str, loose: bool, include: &IncludeSet) -> bool {
    loose || include.is_included(rel_path)
}

/// F1 (privacy boundary): reject an `include` entry that is absolute or contains `..`, then
/// `canonicalize()` the resolved sub-path and assert it still lives under the canonicalized
/// collection root. A crafted `include` (e.g. `../../etc`, an absolute path, or a symlink that
/// escapes) must never let `scan_selective` walk — and therefore publish — files outside the chosen
/// tree. This guard is the scan-path analogue of the slug guard (which does NOT cover scan
/// sub-paths).
pub(crate) fn contained_under_root(root: &Path, rel: &str) -> Result<std::path::PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(format!("include path '{rel}' must be relative to the collection root"));
    }
    if rel_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("include path '{rel}' must not contain '..'"));
    }
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("collection root is not accessible: {e}"))?;
    let resolved = canon_root
        .join(rel_path)
        .canonicalize()
        .map_err(|e| format!("include path '{rel}' is not accessible: {e}"))?;
    if !resolved.starts_with(&canon_root) {
        return Err(format!("include path '{rel}' escapes the collection root"));
    }
    Ok(resolved)
}

/// What a selection-aware scan produced, plus whether the tree turned out to be **oversized**
/// (QURATOR-336). `oversized` is `None` for the ordinary case — the listing is the whole tree and
/// `total_bytes` is its real size — and `Some(lower_bound)` for an oversized one, where `items` is
/// the breadth-first teaser instead (see [`scan_selective_bfs_teaser`]) and `lower_bound` is the
/// count the estimator had reached when it crossed [`OVERSIZED_ITEM_THRESHOLD`].
#[derive(Debug)]
pub(crate) struct SelectiveScan {
    pub items: Vec<DirectoryItem>,
    pub total_bytes: u64,
    /// `Some(n)` ⇒ oversized; `n` is a **LOWER BOUND** on the collection's item count (the
    /// estimator stopped counting the moment it crossed the threshold), so it is what a browser
    /// should render as "n+ items".
    pub oversized: Option<u64>,
}

/// Selection-aware directory walk (replaces the depth-limited `scan_recursive`). Always lists
/// root-level loose files; fully recurses a subdir iff it (or an ancestor) is checked; for a dir
/// that is only an *ancestor* of a selection, traverses it but withholds its own loose files;
/// otherwise skips it. Validates every `include` entry against the root (F1) before any walk.
///
/// **Oversized collections (QURATOR-336)** take a different shape, and it is decided up front: a
/// cheap breadth-first COUNT pass runs first ([`estimate_item_count`]) and stops the instant the
/// tree is provably past [`OVERSIZED_ITEM_THRESHOLD`]. If it is, the full walk below — which
/// materializes one `DirectoryItem` per file and is exactly the minutes-long pass a 40-million-file
/// collection cannot afford — never runs; a breadth-first teaser bounded by [`LISTING_MAX_BYTES`]
/// is built instead. If it is not (the count came back at or under the threshold), the rest of this
/// function is today's path byte for byte, including the [`MAX_COLLECTION_ITEMS`] refusal.
///
/// Signature-preserving wrapper over [`scan_selective_sized`] for the tests that do not consume the
/// oversized bit. Every production caller (add, rescan, the WAN harness's seed) uses the sized form,
/// so this is test-only: a production caller here would silently drop the oversized bit.
#[cfg(test)]
pub(crate) fn scan_selective(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    let scan = scan_selective_sized(root, include, exclude)?;
    Ok((scan.items, scan.total_bytes))
}

/// [`scan_selective`] with the oversized bit — the real entry point.
pub(crate) fn scan_selective_sized(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
) -> anyhow::Result<SelectiveScan> {
    scan_selective_sized_with_threshold(root, include, exclude, OVERSIZED_ITEM_THRESHOLD)
}

/// The threshold is a parameter (not baked in) so the oversized branch is reachable from a test
/// with a handful of real files instead of 150,000 — the same reason [`enforce_item_cap`] is a
/// pure predicate.
fn scan_selective_sized_with_threshold(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
    threshold: u64,
) -> anyhow::Result<SelectiveScan> {
    // F1: containment check on every checked path BEFORE walking anything.
    for c in &include.checked {
        contained_under_root(root, c).map_err(|e| anyhow::anyhow!(e))?;
    }
    if let Some(lower_bound) = estimate_item_count(root, include, exclude, threshold)?
        .into_oversized()
    {
        let (items, total_bytes) = scan_selective_bfs_teaser(root, include, exclude, LISTING_MAX_BYTES)?;
        return Ok(SelectiveScan { items, total_bytes, oversized: Some(lower_bound) });
    }
    let (items, total_bytes) = scan_selective_walk(root, include, exclude)?;
    // MAX_COLLECTION_ITEMS: enforced DURING the walk (the walker aborts the moment its running
    // count exceeds the cap — QURATOR-253, so a many-tiny-files tree is never fully materialized
    // just to be rejected) and again here, on the final assembled tree — the single chokepoint
    // every scan caller goes through, so no entry point can bypass it.
    enforce_item_cap(count_items(&items)).map_err(|e| anyhow::anyhow!(e))?;
    Ok(SelectiveScan { items, total_bytes, oversized: None })
}

/// What the estimator found. `count` is the exact item count when `complete`, and a **LOWER BOUND**
/// on it when not — the pass stopped the moment it crossed the threshold, so the tree's real total
/// was never established. `count > threshold` whenever `complete` is false, by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Estimate {
    count: u64,
    complete: bool,
}

impl Estimate {
    /// `Some(lower_bound)` when the pass stopped at the threshold — the collection is oversized —
    /// and `None` when the whole tree was counted and fits.
    fn into_oversized(self) -> Option<u64> {
        (!self.complete).then_some(self.count)
    }
}

/// Cheap breadth-first **count** pass over the same selection the real walk uses — the "can this
/// collection possibly be sent?" probe that runs before [`scan_selective_walk`] (QURATOR-336). It
/// builds no `DirectoryItem`, keeps no bytes, and stops the instant its running count passes
/// `threshold`, so the cost is proportional to how big the tree *has* to be before the answer is
/// already known — never to how big it is.
///
/// The selection rules are the same predicates the walk calls ([`lists_loose_files`],
/// [`dir_is_descended`], [`file_is_listed`]) over the same `exclude` globset, and the depth bound is
/// the same [`MAX_SCAN_DEPTH`] with the same message — so the count can never describe a different
/// tree than the walk (or the teaser) would produce.
///
/// The threshold is a parameter (not baked in) so the early stop is reachable from a test with a
/// handful of real files instead of 150,000 — the same reason [`enforce_item_cap`] is a pure
/// predicate.
fn estimate_item_count(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
    threshold: u64,
) -> anyhow::Result<Estimate> {
    // (absolute path, relative prefix, depth), FIFO — breadth-first, so the early stop fires as
    // shallow as the tree allows.
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, String, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new(), 0));
    // Files and folders both count, matching `count_items`'s definition of an item (and therefore
    // the units [`MAX_COLLECTION_ITEMS`] / [`OVERSIZED_ITEM_THRESHOLD`] are stated in).
    let mut count: u64 = 0;
    while let Some((dir, rel_prefix, depth)) = queue.pop_front() {
        let loose = lists_loose_files(&rel_prefix, include);
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel_path =
                if rel_prefix.is_empty() { name.clone() } else { format!("{rel_prefix}/{name}") };
            if exclude.is_match(&rel_path) {
                continue;
            }
            let meta = entry.metadata()?;
            if meta.is_dir() {
                if !dir_is_descended(&rel_path, include) {
                    continue;
                }
                // Same bound, same message, same point in the descent as the walk's (the parent
                // refuses before descending).
                if depth >= MAX_SCAN_DEPTH {
                    return Err(anyhow::anyhow!(
                        "directory tree exceeds the {MAX_SCAN_DEPTH}-level depth limit at \
                         '{rel_path}'; refusing to scan deeper (possible hostile nesting)"
                    ));
                }
                count += 1;
                if count > threshold {
                    return Ok(Estimate { count, complete: false });
                }
                queue.push_back((entry.path(), rel_path, depth + 1));
            } else if meta.is_file() && file_is_listed(&rel_path, loose, include) {
                count += 1;
                if count > threshold {
                    return Ok(Estimate { count, complete: false });
                }
            }
        }
    }
    Ok(Estimate { count, complete: true })
}

/// Breadth-first listing builder for an **oversized** collection (QURATOR-336): every entry at
/// depth 1 is placed before any entry at depth 2, and so on, so the published preview shows the
/// collection's top-level shape rather than one arbitrarily deep branch of it. The walk stops once
/// the listing would pass `budget_bytes`, and whatever was reached is a valid `Vec<DirectoryItem>`
/// tree — folders hold whatever children were reached before the stop.
///
/// **The byte measure** is [`teaser_item_bytes`] summed over the items placed, started at 2 for the
/// array's two brackets. That is the SAME serialization the publish uses (`serde_json` over
/// `DirectoryItem`, the type `collection_to_listing_json` writes into `entries`), and per item it
/// accounts one byte more than the separator that item will need — so the running total is a
/// *lower* bound on the real `entries` bytes at any prefix, and the builder can never overfill the
/// budget. Two consequences worth stating plainly: the envelope's metadata (alias, description,
/// markers) sits *outside* `entries`, so `truncate_listing` may still trim a few entries off the
/// tail — bounded by that metadata (a few hundred bytes), never by the tree — and the scan still
/// never walked the millions of entries it stopped in.
fn scan_selective_bfs_teaser(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
    budget_bytes: usize,
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    let mut roots: Vec<DirectoryItem> = vec![];
    let mut total_bytes: u64 = 0;
    // The two brackets `entries` will be wrapped in. Reserving them here keeps the accounting an
    // over-estimate (never an under-estimate) of the array's real serialized size.
    let mut entry_bytes: usize = 2;
    // (absolute path, relative prefix, index path of the folder to expand, its depth). The index
    // path is how a child reaches the folder that owns it without holding a live borrow of the tree.
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, String, Vec<usize>, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new(), vec![], 0));
    while let Some((dir, rel_prefix, idx_path, depth)) = queue.pop_front() {
        let loose = lists_loose_files(&rel_prefix, include);
        // (relative path, absolute path, metadata, display name), then sorted exactly as each walk
        // frame sorts: folders first, then names, case-insensitively.
        let mut entries: Vec<(String, std::path::PathBuf, std::fs::Metadata, String)> = vec![];
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel_path =
                if rel_prefix.is_empty() { name.clone() } else { format!("{rel_prefix}/{name}") };
            if exclude.is_match(&rel_path) {
                continue;
            }
            let meta = entry.metadata()?;
            // Anything that is neither a plain directory nor a file (sockets, fifos, …) is not an
            // item anywhere in this module.
            if !meta.is_dir() && !meta.is_file() {
                continue;
            }
            entries.push((rel_path, entry.path(), meta, name));
        }
        entries.sort_by(|a, b| match (a.2.is_dir(), b.2.is_dir()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
        });
        let mut stop = false;
        for (rel_path, path, meta, name) in entries {
            let item = if meta.is_dir() {
                if !dir_is_descended(&rel_path, include) {
                    continue;
                }
                if depth >= MAX_SCAN_DEPTH {
                    return Err(anyhow::anyhow!(
                        "directory tree exceeds the {MAX_SCAN_DEPTH}-level depth limit at \
                         '{rel_path}'; refusing to scan deeper (possible hostile nesting)"
                    ));
                }
                DirectoryItem {
                    name: name.clone(),
                    item_type: ItemType::Folder,
                    size: None,
                    format: None,
                    year: None,
                    tags: vec![],
                    note: None,
                    children: vec![],
                }
            } else {
                if !file_is_listed(&rel_path, loose, include) {
                    continue;
                }
                total_bytes += meta.len();
                DirectoryItem {
                    name: name.clone(),
                    item_type: ItemType::File,
                    size: Some(format_size(meta.len())),
                    format: path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_uppercase()),
                    year: None,
                    tags: vec![],
                    note: None,
                    children: vec![],
                }
            };
            let cost = teaser_item_bytes(&item);
            if entry_bytes + cost > budget_bytes {
                // The budget is spent: stop the whole breadth-first pass, leaving every folder
                // already placed holding the children that were reached.
                stop = true;
                break;
            }
            entry_bytes += cost;
            let is_dir = item.item_type == ItemType::Folder;
            let children = listing_node_mut(&mut roots, &idx_path);
            let child_index = children.len();
            children.push(item);
            if is_dir {
                let mut child_path = idx_path.clone();
                child_path.push(child_index);
                queue.push_back((path, rel_path, child_path, depth + 1));
            }
        }
        if stop {
            break;
        }
    }
    Ok((roots, total_bytes))
}

/// Mutable access to the children vector of the item at `idx_path` inside a listing under
/// construction — an empty path is the listing itself. How the breadth-first builder hands a child
/// to the folder that owns it while the tree is walked without recursion.
fn listing_node_mut<'a>(
    roots: &'a mut Vec<DirectoryItem>,
    idx_path: &[usize],
) -> &'a mut Vec<DirectoryItem> {
    match idx_path.split_first() {
        None => roots,
        Some((head, rest)) => listing_node_mut(&mut roots[*head].children, rest),
    }
}

/// What one item costs the teaser budget: its serialized length under the SAME `serde_json`
/// serialization the publish path uses, plus one byte for the array separator it will need. Summed
/// over the items actually placed, that is at least the real serialized size of those items — the
/// direction that cannot overfill the budget (see [`scan_selective_bfs_teaser`]). Failing to
/// serialize (it cannot: `DirectoryItem` has no non-string keys and no floats) reads as "infinitely
/// large", which stops the walk rather than filling it with something unmeasurable.
fn teaser_item_bytes(item: &DirectoryItem) -> usize {
    serde_json::to_string(item).map(|s| s.len() + 1).unwrap_or(usize::MAX)
}

/// Pure predicate behind the [`MAX_COLLECTION_ITEMS`] guard — no filesystem, so it's testable
/// without materializing 100,000+ real files. Called incrementally by `scan_selective_walk`
/// (QURATOR-253) and once post-walk by `scan_selective` on the assembled tree.
fn enforce_item_cap(item_count: u64) -> Result<(), String> {
    if item_count > MAX_COLLECTION_ITEMS {
        return Err(format!(
            "this collection has {item_count} items, over the {MAX_COLLECTION_ITEMS}-item cap \
             per collection. Split it into smaller collections, or narrow the selection."
        ));
    }
    Ok(())
}

/// Maximum directory depth [`scan_selective_walk`] descends (the root is level 0). A directory at
/// level [`MAX_SCAN_DEPTH`] that still has a subdirectory is rejected with an `Err` — loud, not
/// silent — so a truncated scan can never masquerade as a complete one. 256 levels is far beyond
/// any legitimate collection, so a normal scan is never affected; this only ever makes a scan
/// return *less* (an error) on a pathological tree.
///
/// This is now a pure *policy* bound ("refuse absurd nesting"), NOT a stack budget: the walk is
/// iterative (see [`scan_selective_walk`]), so its stack usage is O(1) in tree depth and this
/// constant no longer approximates a platform-dependent stack size. The 2026-08-25 follow-up to
/// audit #9: the recursive original consumed ~2–4 KB of scan-thread stack per level, so 256 levels
/// needed ~0.8–1 MB on Linux (more on macOS) — the guard fired only *after* the stack was already
/// exhausted, and macOS CI aborted (SIGABRT) in `scan_selective_rejects_pathologically_deep_tree`.
const MAX_SCAN_DEPTH: usize = 256;

/// One directory in the iterative walk's explicit work-stack. Frames live on the heap, so the
/// deepest tree the policy bound admits costs a few pointers, not a few hundred stack frames.
struct WalkFrame {
    /// Display name of this directory; `None` for the collection root, whose result is the walk's
    /// return value rather than a child of anything.
    name: Option<String>,
    /// Depth of this directory (root = 0) — what the recursive original's `depth` argument held.
    depth: usize,
    /// This directory's items (files so far; finished subdirectories are appended as their frames
    /// complete). Sorted when the frame finalises.
    items: Vec<DirectoryItem>,
    /// Bytes of every file in this subtree found so far (children roll up into it).
    total_bytes: u64,
    /// Subdirectories still to descend into: (absolute path, relative path, display name).
    /// Order is immaterial — every level is sorted when its frame finalises, exactly as the
    /// recursive original sorted each call's items before returning.
    pending: Vec<(std::path::PathBuf, String, String)>,
}

impl WalkFrame {
    /// The listing pass: the body the recursive original ran *before* recursing. Loosely-coupled
    /// files land in `items` now; subdirectories are queued in `pending` and descended into by the
    /// driver loop below, which is what removes the recursion.
    fn scan(
        dir: &Path,
        name: Option<String>,
        rel_prefix: &str,
        depth: usize,
        include: &IncludeSet,
        exclude: &globset::GlobSet,
        seen: &mut u64,
    ) -> anyhow::Result<WalkFrame> {
        let is_root = rel_prefix.is_empty();
        // Loose files are listed at the root and inside any included directory; an ancestor-only
        // directory withholds them. The predicate is shared with the oversized estimator so the two
        // traversals cannot disagree about what this tree contains.
        let list_loose_files = lists_loose_files(rel_prefix, include);

        let mut frame =
            WalkFrame { name, depth, items: vec![], total_bytes: 0, pending: vec![] };
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel_path = if is_root { name.clone() } else { format!("{rel_prefix}/{name}") };
            if exclude.is_match(&rel_path) {
                continue;
            }
            let meta = entry.metadata()?;
            let path = entry.path();
            if meta.is_dir() {
                // Descend into a directory that is selected (full subtree) OR only an ancestor of
                // a selection (to reach the checked descendant). Skip everything else entirely.
                if !dir_is_descended(&rel_path, include) {
                    continue;
                }
                // Depth bound (audit #9): a pathologically nested tree (hostile archive / synced
                // share) is rejected LOUDLY — an Err surfaces to the caller, never a truncated
                // scan masquerading as complete. Checked at the same point the recursive original
                // checked it (in the parent, before descending), with the same message naming the
                // subdirectory being refused.
                if depth >= MAX_SCAN_DEPTH {
                    return Err(anyhow::anyhow!(
                        "directory tree exceeds the {MAX_SCAN_DEPTH}-level depth limit at \
                         '{rel_path}'; refusing to scan deeper (possible hostile nesting)"
                    ));
                }
                frame.pending.push((path, rel_path, name));
            } else if meta.is_file() && file_is_listed(&rel_path, list_loose_files, include) {
                // devtest #10: a file is included when it lives in the root/an included directory
                // (the existing folder rule) OR when it is *itself* checked — so the user can pick
                // individual files inside a directory they did not select wholesale.
                // `has_descendant_under` already keeps this file's ancestor directories traversable
                // (they're withheld as loose files but descended into to reach the checked file).
                frame.total_bytes += meta.len();
                frame.items.push(DirectoryItem {
                    name: name.clone(),
                    item_type: ItemType::File,
                    size: Some(format_size(meta.len())),
                    format: path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_uppercase()),
                    year: None,
                    tags: vec![],
                    note: None,
                    children: vec![],
                });
                // QURATOR-253: count and cap DURING the walk, not after it. The many-tiny-files
                // shape this cap exists for is exactly the shape that must not be fully walked
                // and materialized first — abort at item MAX_COLLECTION_ITEMS + 1 with the same
                // error the post-walk check would give, so a hostile synced share costs at most
                // 100,001 items, never the whole tree.
                *seen += 1;
                enforce_item_cap(*seen).map_err(|e| anyhow::anyhow!(e))?;
            }
        }
        Ok(frame)
    }
}

/// Iterative, selection-aware walk (audit #9 follow-up, 2026-08-25). Depth-first over an explicit
/// heap work-stack of [`WalkFrame`]s, so stack usage is O(1) in tree depth: the recursive original
/// placed ~2–4 KB on the blocking scan thread's stack per level and could not survive its own
/// `MAX_SCAN_DEPTH` bound on a ~2 MB thread (macOS CI aborted here). Semantics are those of the
/// recursion it replaces, verbatim: same selection/ancestor/file rules, same exclude handling, same
/// depth-guard message at the same depth, same per-level dirs-then-alphabetical ordering (each
/// frame sorts exactly when the recursive call it replaces would have), and children's byte totals
/// roll up into their parent frame exactly as the recursive `sub_bytes` accumulation did.
fn scan_selective_walk(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    // QURATOR-253: running item count threaded through every push (files in `WalkFrame::scan`,
    // folders below) so the cap aborts the walk instead of trailing it.
    let mut seen: u64 = 0;
    let mut stack: Vec<WalkFrame> =
        vec![WalkFrame::scan(root, None, "", 0, include, exclude, &mut seen)?];
    loop {
        let next = stack
            .last_mut()
            .expect("walk stack is never empty inside the loop")
            .pending
            .pop();
        match next {
            Some((path, rel_path, name)) => {
                let parent_depth = stack.last().expect("same frame").depth;
                stack.push(WalkFrame::scan(
                    &path,
                    Some(name),
                    &rel_path,
                    parent_depth + 1,
                    include,
                    exclude,
                    &mut seen,
                )?);
            }
            None => {
                // Frame complete: sort its items (the recursion sorted at the end of each call),
                // then hand the finished subtree up to its parent — or, for the root, out.
                let mut finished = stack.pop().expect("same frame");
                finished.items.sort_by(|a, b| match (&a.item_type, &b.item_type) {
                    (ItemType::Folder, ItemType::File) => std::cmp::Ordering::Less,
                    (ItemType::File, ItemType::Folder) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                });
                match stack.last_mut() {
                    Some(parent) => {
                        parent.total_bytes += finished.total_bytes;
                        parent.items.push(DirectoryItem {
                            name: finished.name.expect("only the root is unnamed"),
                            item_type: ItemType::Folder,
                            size: None,
                            format: None,
                            year: None,
                            tags: vec![],
                            note: None,
                            children: finished.items,
                        });
                        // QURATOR-253: folders are items too (`count_items` counts them), so the
                        // running cap counts the exact same units the post-walk check does —
                        // the in-walk abort can never diverge from it.
                        seen += 1;
                        enforce_item_cap(seen).map_err(|e| anyhow::anyhow!(e))?;
                    }
                    None => return Ok((finished.items, finished.total_bytes)),
                }
            }
        }
    }
}

/// Enumerate the immediate children of `path` — sub-directories AND files (devtest #10), sorted
/// directories-first then alphabetical. A directory is tagged with whether it has children of its own
/// (drives the picker's ▶ expander); a file is a leaf. Pure core behind `list_subdirs`.
pub(crate) fn list_subdirs_core(path: &str) -> anyhow::Result<Vec<SubdirEntry>> {
    let root = Path::new(path);
    anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
    let mut entries: Vec<SubdirEntry> = vec![];
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let is_dir = meta.is_dir();
        // Skip anything that is neither a plain directory nor a file (sockets, fifos, …).
        if !is_dir && !meta.is_file() {
            continue;
        }
        let child_path = entry.path();
        entries.push(SubdirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            has_children: is_dir && dir_has_children(&child_path),
            path: child_path.to_string_lossy().into_owned(),
            is_file: !is_dir,
        });
    }
    // Directories first, then files; each group alphabetical (matches `scan_selective_walk`'s order).
    entries.sort_by(|a, b| match (a.is_file, b.is_file) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}

/// Cheap "does this directory contain at least one child (sub-directory or file)?" probe (stops at
/// the first hit) — drives the picker's ▶ expander now that files are selectable too (devtest #10).
/// An unreadable directory reports `false` rather than erroring — the expander simply won't show.
fn dir_has_children(dir: &Path) -> bool {
    let Ok(mut rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.next().is_some()
}

fn build_glob_set(patterns: &[String]) -> Result<globset::GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    let mut rejected: Vec<String> = Vec::new();
    for pat in patterns {
        match Glob::new(pat) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => rejected.push(format!("'{pat}': {e}")),
        }
    }
    if !rejected.is_empty() {
        // Fail closed (audit #31, CWE-636): never silently drop a user's exclusion — one bad
        // pattern must abort the scan rather than fall back to "exclude nothing".
        return Err(format!(
            "invalid exclude pattern{}: {}",
            if rejected.len() == 1 { "" } else { "s" },
            rejected.join(", ")
        ));
    }
    builder
        .build()
        .map_err(|e| format!("failed to build exclusion set: {e}"))
}

fn format_size(bytes: u64) -> String {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    const KB: u64 = 1_024;
    if bytes >= GB { format!("{:.1} GB", bytes as f64 / GB as f64) }
    else if bytes >= MB { format!("{:.1} MB", bytes as f64 / MB as f64) }
    else if bytes >= KB { format!("{:.1} KB", bytes as f64 / KB as f64) }
    else { format!("{bytes} B") }
}

pub(crate) fn count_items(items: &[DirectoryItem]) -> u64 {
    items.iter().fold(0, |acc, item| acc + 1 + count_items(&item.children))
}

/// Build a relative-path→note map from an existing listing for note preservation across rescans.
fn collect_notes(items: &[DirectoryItem], prefix: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for item in items {
        let rel = if prefix.is_empty() { item.name.clone() } else { format!("{prefix}/{}", item.name) };
        if let Some(note) = &item.note {
            map.insert(rel.clone(), note.clone());
        }
        map.extend(collect_notes(&item.children, &rel));
    }
    map
}

/// Apply previously collected notes to a freshly scanned listing, keyed by relative path.
fn apply_notes(
    mut items: Vec<DirectoryItem>,
    notes: &std::collections::HashMap<String, String>,
    prefix: &str,
) -> Vec<DirectoryItem> {
    for item in &mut items {
        let rel = if prefix.is_empty() { item.name.clone() } else { format!("{prefix}/{}", item.name) };
        if let Some(note) = notes.get(&rel) {
            item.note = Some(note.clone());
        }
        item.children = apply_notes(std::mem::take(&mut item.children), notes, &rel);
    }
    items
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// QURATOR-161 (Lane A, slice covering lines 178-807 of this file): Tauri-command guard tests for
// `scan_directory`, `list_subdirs`, `delete_collection`, `get_collections`,
// `collection_source_accessible`, and `update_collection_meta`. Same technique as identity.rs /
// browse.rs: `tauri::test::mock_app()` mints genuine `State<'_, T>` handles through the real
// `StateManager`, so these call the actual `#[tauri::command]` fns at their real signatures — not a
// reimplementation of their guards.
#[cfg(test)]
mod collection_command_guards_a {
    use super::*;
    use tauri::Manager;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    /// A minimal, valid `Collection` draft — enough to exercise the metadata/publish-adjacent
    /// guards without depending on the other module's private `mod tests` helpers (not visible
    /// from here).
    fn a_collection(slug: &str) -> Collection {
        Collection {
            slug: slug.into(),
            path_alias: slug.into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        }
    }

    fn guard_app() -> (TempDir, tauri::App<tauri::test::MockRuntime>) {
        let app = tauri::test::mock_app();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        app.manage(store);
        let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        app.manage(identity);
        app.manage(net::new_shared());
        (dir, app)
    }

    // -- scan_directory --------------------------------------------------------------------------

    /// An alias that collapses to an empty slug (all punctuation) is refused before any filesystem
    /// walk happens. Pins the `is_valid_slug` guard in `scan_directory_inner` (line 231).
    ///
    /// Mutation to redden: in `scan_directory_inner`, change the guard at (current) line 231 from
    /// `if !is_valid_slug(&slug) {` to `if false {` (or delete the `if` block entirely) — the slug
    /// derivation and the message text stay, only the branch is defeated.
    #[tokio::test]
    async fn scan_directory_rejects_an_alias_that_collapses_to_no_slug() {
        let (_dir, app) = guard_app();
        let opts = ScanOptions {
            path: "/does/not/matter/for/this/guard".into(),
            path_alias: "!!!".into(),
            include: vec![],
            exclude: vec![],
        };
        let err = scan_directory(opts, app.state::<DataStore>()).await.unwrap_err();
        assert!(
            err.contains("produces an invalid collection slug"),
            "expected the invalid-slug message, got {err}"
        );
    }

    /// QURATOR-249: an alias that slugs to `profile` — the fixed key the profile-teaser marker
    /// lives under in the same flat `published/<key>.json` namespace collection markers use — is
    /// refused before any filesystem walk, so a collection publish can never clobber the teaser
    /// marker nor flip `is_published("profile")` on. "Profile" is charset-VALID (it passes
    /// `is_valid_slug`), so this pins the reserved-name guard specifically, not the slug-charset
    /// one beside it.
    ///
    /// Mutation to redden: in `scan_directory_inner`, change the reserved-name guard
    /// `if is_reserved_marker_slug(&slug) {` to `if false {` (or delete the whole `if` block) —
    /// the scan then walks past the guard and fails later with "is not a directory" (the test
    /// path does not exist), so the `contains("'profile' is a reserved name")` assert reds.
    #[tokio::test]
    async fn scan_directory_rejects_an_alias_that_slugs_to_the_profile_marker_key() {
        let (_dir, app) = guard_app();
        let opts = ScanOptions {
            path: "/does/not/matter/for/this/guard".into(),
            path_alias: "Profile".into(),
            include: vec![],
            exclude: vec![],
        };
        let err = scan_directory(opts, app.state::<DataStore>()).await.unwrap_err();
        assert!(
            err.contains("'profile' is a reserved name"),
            "expected the reserved-name message, got {err}"
        );
    }

    /// Bonus (not a ticket-listed line, but immediately adjacent): a `path_alias` that survives
    /// slugging but points at a non-directory is refused by the scan closure's `ensure!`, not
    /// silently treated as an empty scan.
    ///
    /// Mutation to redden: in the `scan` closure inside `scan_directory_inner`, change
    /// `anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());` to
    /// `anyhow::ensure!(true, "{} is not a directory", root.display());`.
    #[tokio::test]
    async fn scan_directory_refuses_a_source_root_that_is_not_a_directory() {
        let (dir, app) = guard_app();
        let missing = dir.path().join("nope-does-not-exist");
        let opts = ScanOptions {
            path: missing.to_string_lossy().into_owned(),
            path_alias: "valid-alias".into(),
            include: vec![],
            exclude: vec![],
        };
        let err = scan_directory(opts, app.state::<DataStore>()).await.unwrap_err();
        assert!(err.contains("is not a directory"), "expected a not-a-directory refusal, got {err}");
    }

    /// QURATOR-207 (owner ruling 2026-09-13): a scanned-but-unpublished collection leaves NO
    /// durable trace — `scan_directory` writes no draft, no scan spec, no share settings — and
    /// Publish is the only writer, promoting the in-memory scan cache to the store (gated on ≥1
    /// content type). Also pins that `update_collection_meta`'s pre-Publish edits land in the
    /// cache (still nothing durable), and the promote merge rule: promoting over an existing
    /// record keeps the record's metadata (where the Details form's edits live) and takes the
    /// scan's freshly-walked tree.
    ///
    /// Mutations to redden (orchestrator applies one at a time, then reverts):
    /// 1. Restore the durable write at the end of `scan_directory_inner` — re-add
    ///    `store.save_collection_draft(&collection).map_err(cmd_err)?;` immediately after the
    ///    `collection.clamp_metadata();` line — the `load_collection_draft(&slug).unwrap().is_none()`
    ///    assert must FAIL (the scan would have persisted an orphan draft).
    /// 2. Make `promote_cached_scan` return `Ok(false);` as its first statement — the
    ///    "promote must persist the collection" expect and everything after it must FAIL.
    /// 3. In `update_collection_meta`'s `None` arm, delete the line
    ///    `cached.collection.content_types = content_types;` — the cache entry keeps an empty
    ///    content_types, so the later `promote_cached_scan(&slug, &store).unwrap()` (the
    ///    "a waiting scan must promote" assert) must FAIL on the gate error.
    /// 4. Delete the content-type gate at the top of `promote_cached_scan` — the
    ///    `promote_cached_scan(&slug, &store).unwrap_err()` assert must FAIL (it would return Ok).
    /// 5. Delete the `.is_none()` guard around the cache fast path in `scan_directory_inner`
    ///    (change `if store.load_collection_draft(&slug).map_err(cmd_err)?.is_none() {` to
    ///    `if true {`) — the final `assert_ne!(rewolked.item_count, 9999, ...)` must FAIL (the
    ///    poisoned cache entry would be served to the persisted slug's Rescan).
    ///
    /// No relay pinning needed: neither the scan, the meta update, nor the promote touches the
    /// network.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scan_writes_nothing_durable_until_publish_promotes() {
        let work = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(work.path().join("season-1")).unwrap();
        std::fs::write(work.path().join("film.mkv"), b"x").unwrap();
        std::fs::write(work.path().join("season-1").join("ep.mkv"), b"y").unwrap();
        let (_data, app) = guard_app();
        let store = app.state::<DataStore>();
        let opts = ScanOptions {
            path: work.path().to_string_lossy().into_owned(),
            path_alias: "Q207 Pin".into(),
            include: vec![],
            exclude: vec![],
        };

        let scanned = scan_directory_inner(opts, &store).await.unwrap();
        let slug = Collection::slug_from_alias("Q207 Pin");
        assert_eq!(scanned.slug, slug);
        assert!(
            store.load_collection_draft(&slug).unwrap().is_none(),
            "the scan must leave NO durable draft (QURATOR-207)"
        );
        assert!(
            store.load_scan_spec(&slug).unwrap().is_none(),
            "the scan must leave NO durable scan spec (QURATOR-207)"
        );
        assert!(
            store.load_share_settings(&slug).unwrap().is_none(),
            "the scan must leave NO durable share settings (QURATOR-207)"
        );
        assert!(
            scan_cache().lock().unwrap().get(&slug).is_some(),
            "the scanned tree must be held in the in-memory cache, waiting for Publish"
        );

        // The publish gate holds BEFORE anything durable: a content-type-less scan refuses to
        // promote, and the scan stays waiting in the cache so the user can retry without rescanning.
        let err = promote_cached_scan(&slug, &store).unwrap_err();
        assert_eq!(
            err,
            "At least one content type is required before publishing a collection."
        );
        assert!(
            scan_cache().lock().unwrap().get(&slug).is_some(),
            "a gate-refused promote must not consume the waiting scan"
        );

        // The Details form's edit lands in the CACHE — still nothing durable.
        update_collection_meta(
            slug.clone(),
            Some("notes".into()),
            vec!["video".into()],
            vec![],
            vec![],
            false,
            app.state::<DataStore>(),
        )
        .await
        .unwrap();
        assert!(
            store.load_collection_draft(&slug).unwrap().is_none(),
            "a pre-Publish meta edit must not persist a draft (QURATOR-207)"
        );

        // Publish is the only writer: the promote step persists draft + share root + scan spec.
        assert!(promote_cached_scan(&slug, &store).unwrap(), "a waiting scan must promote");
        let persisted = store
            .load_collection_draft(&slug)
            .unwrap()
            .expect("promote must persist the collection");
        assert_eq!(persisted.listing.len(), scanned.listing.len(), "promote keeps the scanned tree");
        assert_eq!(persisted.content_types, vec!["video".to_string()], "the cached meta edit rode to Publish");
        assert_eq!(persisted.description.as_deref(), Some("notes"), "the cached description rode to Publish");
        let spec = store.load_scan_spec(&slug).unwrap().expect("promote must persist the scan spec");
        assert_eq!(spec.root, work.path().to_string_lossy().into_owned());
        let share =
            store.load_share_settings(&slug).unwrap().expect("promote must persist the share root");
        assert_eq!(share.root_path, Some(work.path().to_string_lossy().into_owned()));
        // Promoted once — the cache entry is consumed, so a second promote is a no-op.
        assert!(!promote_cached_scan(&slug, &store).unwrap(), "a second promote must be a no-op");

        // Merge rule: a rescan waiting in the cache promotes over the record WITHOUT clobbering
        // the metadata the Details form persisted (listing-side from the scan, metadata from disk).
        let mut record = store.load_collection_draft(&slug).unwrap().unwrap();
        record.tags = vec!["night".into()];
        store.save_collection_draft(&record).unwrap();
        let rescan_opts = ScanOptions {
            path: work.path().to_string_lossy().into_owned(),
            path_alias: "Q207 Pin".into(),
            include: vec![],
            exclude: vec![],
        };
        scan_directory_inner(rescan_opts, &store).await.unwrap();
        assert!(promote_cached_scan(&slug, &store).unwrap(), "the rescan must be waiting and promote");
        let merged =
            store.load_collection_draft(&slug).unwrap().expect("record persists after merge-promote");
        assert_eq!(merged.content_types, vec!["video".to_string()], "metadata survives a rescan promote");
        assert_eq!(merged.tags, vec!["night".to_string()], "tags survive a rescan promote");

        // A persisted collection never serves the cache — the Rescan verb must always re-walk.
        // Plant a poisoned cache entry for the now-persisted slug; a fast-path hit would return
        // its item_count verbatim.
        {
            let mut stale = merged.clone();
            stale.item_count = 9999;
            let mut cache = scan_cache().lock().unwrap();
            cache.insert(
                slug.clone(),
                CachedScan {
                    path: work.path().to_string_lossy().into_owned(),
                    include: vec![],
                    exclude: vec![],
                    total_bytes: 0,
                    collection: stale,
                },
            );
        }
        let rewolked = scan_directory_inner(
            ScanOptions {
                path: work.path().to_string_lossy().into_owned(),
                path_alias: "Q207 Pin".into(),
                include: vec![],
                exclude: vec![],
            },
            &store,
        )
        .await
        .unwrap();
        assert_ne!(
            rewolked.item_count, 9999,
            "a scan of a persisted collection must re-walk the disk, never serve the cache"
        );
    }

    // -- list_subdirs -----------------------------------------------------------------------------

    /// `list_subdirs` refuses a non-directory path via `list_subdirs_core`'s `ensure!` — reachable
    /// directly since the command takes no `State`. This does NOT pin the ticket-listed line 216
    /// (the `spawn_blocking` `JoinError` branch); see the OWED note in the report.
    ///
    /// Mutation to redden: in `list_subdirs_core`, change
    /// `anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());` to
    /// `anyhow::ensure!(true, "{} is not a directory", root.display());`.
    #[tokio::test]
    async fn list_subdirs_refuses_a_non_directory_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope-does-not-exist");
        let err = list_subdirs(missing.to_string_lossy().into_owned()).await.unwrap_err();
        assert!(err.contains("is not a directory"), "expected a not-a-directory refusal, got {err}");
    }

    // -- delete_collection --------------------------------------------------------------------------

    /// An invalid slug is refused before any store access. Pins the `is_valid_slug` guard (lines
    /// 369-371).
    ///
    /// Mutation to redden: change the exact literal `"Invalid collection slug"` at that `.ok_or(...)`
    /// to any other text — the test's `assert_eq!` on the exact message reds.
    #[tokio::test]
    async fn delete_collection_rejects_an_invalid_slug() {
        let (_dir, app) = guard_app();
        let err = delete_collection(
            "not a valid slug!!".into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Invalid collection slug");
    }

    /// A published collection refuses to delete when no identity is loaded (the unpublish-first
    /// path needs an identity to sign the retraction), and does so BEFORE touching the on-disk
    /// draft. Pins line 379. Only reachable when `store.is_published(slug)` is true, so the test
    /// marks the collection published directly via the store's on-disk layout.
    ///
    /// Mutation to redden: change the exact literal `"No identity loaded. Generate a keypair first."`
    /// at that `.ok_or(...)` to any other text.
    #[tokio::test]
    async fn delete_collection_refuses_to_unpublish_without_an_identity() {
        let (_dir, app) = guard_app();
        let store = app.state::<DataStore>();
        let slug = "my-slug";
        std::fs::create_dir_all(store.published_path(slug).parent().unwrap()).unwrap();
        std::fs::write(store.published_path(slug), b"{}").unwrap();
        assert!(store.is_published(slug), "test setup: the collection must read as published");

        let err = delete_collection(
            slug.into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "No identity loaded. Generate a keypair first.");
        assert!(
            store.is_published(slug),
            "a refused delete must not have removed the published marker"
        );
    }

    // -- get_collections ----------------------------------------------------------------------------

    /// `get_collections` has no refusal path (confirmed by reading the fn: it never returns `Err`).
    /// This is a pass-side test instead: entries sort by `path_alias`, and a collection scanned
    /// before the byte-total sidecar existed defaults `total_bytes` to 0 rather than erroring.
    ///
    /// Mutation to redden: in `get_collections`, change
    /// `entries.sort_by(|a, b| a.collection.path_alias.cmp(&b.collection.path_alias));` to a no-op
    /// (delete the sort call) — the assertion on ordering reds.
    #[tokio::test]
    async fn get_collections_sorts_by_alias_and_defaults_missing_total_bytes_to_zero() {
        let (_dir, app) = guard_app();
        let store = app.state::<DataStore>();
        store.save_collection_draft(&a_collection("zebra")).unwrap();
        store.save_collection_draft(&a_collection("apple")).unwrap();
        // No scan_spec sidecar saved for either slug — total_bytes must default to 0, not error.

        let entries = get_collections(app.state::<DataStore>()).await.unwrap();
        let aliases: Vec<&str> =
            entries.iter().map(|e| e.collection.path_alias.as_str()).collect();
        assert_eq!(aliases, vec!["apple", "zebra"], "entries must sort by path_alias");
        assert!(entries.iter().all(|e| e.total_bytes == 0), "missing sidecar must default to 0");
    }

    // -- collection_source_accessible --------------------------------------------------------------

    /// No recorded scan spec at all reports reachable (line 421-423) — nothing to gate on.
    ///
    /// Mutation to redden: change `let Some(spec) = store.load_scan_spec(&slug).map_err(cmd_err)?
    /// else { return Ok(true); };` so the `else` arm returns `Ok(false)` instead.
    #[tokio::test]
    async fn collection_source_accessible_is_true_with_no_recorded_scan_spec() {
        let (_dir, app) = guard_app();
        let ok = collection_source_accessible("never-scanned".into(), app.state::<DataStore>())
            .await
            .unwrap();
        assert!(ok, "a collection with no scan spec has nothing to gate on");
    }

    /// A scan spec with an empty `root` reports reachable (line 424-426) rather than stat-ing an
    /// empty path.
    ///
    /// Mutation to redden: change `if spec.root.is_empty() { return Ok(true); }` to
    /// `if spec.root.is_empty() { return Ok(false); }`.
    #[tokio::test]
    async fn collection_source_accessible_is_true_when_root_is_empty() {
        let (_dir, app) = guard_app();
        let store = app.state::<DataStore>();
        store.save_scan_spec("blank-root", &crate::store::ScanSpec::default()).unwrap();
        let ok = collection_source_accessible("blank-root".into(), app.state::<DataStore>())
            .await
            .unwrap();
        assert!(ok, "an empty scan root has nothing to gate on");
    }

    /// A root already mid-stat (present in the process-wide `inflight_access_stats()` set) reports
    /// unreachable instead of launching a second concurrent stat (line 431-435, the dedup insert).
    /// Cleans the key up afterward so this test cannot poison a sibling test sharing the same
    /// process-wide static.
    ///
    /// Mutation to redden: change `if !set.insert(root.clone()) { return Ok(false); }` to
    /// `if !set.insert(root.clone()) { return Ok(true); }`.
    #[tokio::test]
    async fn collection_source_accessible_reports_unreachable_when_a_stat_is_already_in_flight() {
        let (_dir, app) = guard_app();
        let store = app.state::<DataStore>();
        let root = "/a/root/already/being/stat-ed/by/another/caller";
        let spec = crate::store::ScanSpec { root: root.into(), ..Default::default() };
        store.save_scan_spec("busy-slug", &spec).unwrap();

        {
            let mut set = inflight_access_stats().lock().unwrap();
            set.insert(root.to_string());
        }

        let result = collection_source_accessible("busy-slug".into(), app.state::<DataStore>()).await;

        // Clean up unconditionally so a later assertion failure can't leave the static poisoned for
        // other tests in this process.
        inflight_access_stats().lock().unwrap().remove(root);

        assert!(!result.unwrap(), "a root already in flight must report unreachable");
    }

    // -- update_collection_meta ----------------------------------------------------------------------

    /// No draft on disk for the slug is refused (line 473).
    ///
    /// Mutation to redden: change the exact literal in
    /// `.ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?` from
    /// `"No draft found for collection '{safe_slug}'"` to any other text.
    #[tokio::test]
    async fn update_collection_meta_rejects_a_missing_draft() {
        let (_dir, app) = guard_app();
        let err = update_collection_meta(
            "no-such-draft".into(),
            None,
            vec![],
            vec![],
            vec![],
            false,
            app.state::<DataStore>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "No draft found for collection 'no-such-draft'");
    }

    // -- prepare_listing (ticket-table line 525/527; see report note on grouping) --------------------
    //
    // NOTE: `prepare_listing`, `private_recipients`, and `save_collection_marker_guarded` below are
    // NOT inside `update_collection_meta`'s own call graph — they're separate `pub(crate)` helpers
    // used by the (out-of-scope) publish flow. The ticket's guard-site table lists lines 525, 527,
    // 562, 597 under the `update_collection_meta` row, but only line 473 is actually inside that
    // command's body (which ends at line 484). Testing these three directly, as plain functions
    // (they take `&DataStore`, not `State<'_, DataStore>`, so no `mock_app()` is needed) rather than
    // through `update_collection_meta`, which cannot reach them at all.

    /// `prepare_listing` refuses a missing draft (line 525) with the same message shape as
    /// `update_collection_meta`'s own guard, but this is a DIFFERENT call site.
    ///
    /// Mutation to redden: change the literal in `prepare_listing`'s
    /// `.ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?` to any other text.
    #[test]
    fn prepare_listing_rejects_a_missing_draft() {
        let (_dir, store) = test_store();
        let err = prepare_listing("missing-slug", &store).unwrap_err();
        assert_eq!(err, "No draft found for collection 'missing-slug'");
    }

    /// `prepare_listing` refuses a draft with no content types before ever building the listing
    /// JSON (line 527).
    ///
    /// Mutation to redden: change
    /// `if collection.content_types.is_empty() { return Err("At least one content type is required
    /// before publishing a collection.".into()); }` so the condition is always false (e.g.
    /// `if false {`).
    #[test]
    fn prepare_listing_rejects_a_draft_with_no_content_types() {
        let (_dir, store) = test_store();
        let mut col = a_collection("empty-types");
        col.content_types = vec![];
        store.save_collection_draft(&col).unwrap();

        let err = prepare_listing("empty-types", &store).unwrap_err();
        assert_eq!(err, "At least one content type is required before publishing a collection.");
    }

    /// `private_recipients` refuses an empty Private audience (line 562) rather than silently
    /// publishing to nobody.
    ///
    /// Mutation to redden: change `if out.is_empty() { return Err(...) }` so the condition is always
    /// false (e.g. `if false {`).
    #[test]
    fn private_recipients_rejects_an_empty_audience() {
        let (_dir, store) = test_store();
        // No `private_audience.json` written at all — `load_private_audience` defaults to `vec![]`.
        let err = private_recipients(&store).unwrap_err();
        assert!(
            err.contains("haven't chosen anyone to receive it"),
            "expected the empty-audience refusal, got {err}"
        );
    }

    /// `save_collection_marker_guarded` surfaces `Ok(PublishedSave::Revoked)` — never re-creating
    /// a marker for a collection that was unpublished while the publish was in flight — simulated
    /// here by bumping the revocation generation (via `delete_published`, which bumps
    /// unconditionally even when nothing was ever published) past the `gen_at_start` the caller
    /// still holds. QURATOR-251 moved the Revoked→error mapping out to the two tier save sites
    /// (the PUBLIC site also retracts, the PRIVATE site cannot — ephemeral authors), so the
    /// OUTCOME, not an `Err`, is this seam's contract; the exact message both sites map `Revoked`
    /// to is pinned alongside.
    ///
    /// Mutation to redden: in `save_collection_marker_guarded`'s return expression (the
    /// `.map_err(cmd_err)` tail, line 715), append `.map(|_| PublishedSave::Saved)` — the outcome
    /// assert reds while the store still refuses the write.
    #[test]
    fn save_collection_marker_guarded_surfaces_revocation_instead_of_re_creating_the_marker() {
        let (_dir, store) = test_store();
        let slug = "in-flight-slug";
        // Bump the revocation generation from 0 to 1, so the caller's stale `gen_at_start` of 0 no
        // longer matches.
        store.delete_published(slug).unwrap();
        assert_eq!(store.published_generation(slug), 1);

        let outcome = save_collection_marker_guarded(&store, slug, "{}", 0);
        assert!(
            matches!(outcome, Ok(PublishedSave::Revoked)),
            "a mid-write unpublish must surface Revoked"
        );
        assert!(!store.is_published(slug), "a revoked save must not have written the marker");
        // The message both tier save sites map `Revoked` to (production form, pinned verbatim).
        assert_eq!(
            revoked_during_publish(slug),
            "collection 'in-flight-slug' was unpublished while its publish was in flight; not \
             re-creating the published marker"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rejects_path_traversal_sequences() {
        // These inputs must all fail — if any reaches a file-path operation,
        // an attacker could read or overwrite arbitrary files on disk.
        assert!(!is_valid_slug("../identity/keypair"));
        assert!(!is_valid_slug("../../etc/passwd"));
        assert!(!is_valid_slug("foo/bar"));
        assert!(!is_valid_slug("/absolute/path"));
    }

    #[test]
    fn slug_rejects_empty_and_whitespace() {
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("foo bar"));
        assert!(!is_valid_slug(" leading"));
        assert!(!is_valid_slug("trailing "));
    }

    #[test]
    fn slug_rejects_special_characters() {
        assert!(!is_valid_slug("foo.bar"));   // dot could be used in "../" sequences
        assert!(!is_valid_slug("foo\0bar"));  // null byte
        assert!(!is_valid_slug("foo%2Fbar")); // URL-encoded slash
        assert!(!is_valid_slug("foo:bar"));   // colon (Windows path separator)
    }

    #[test]
    fn slug_accepts_valid_patterns() {
        assert!(is_valid_slug("criterion-collection"));
        assert!(is_valid_slug("anime2019"));
        assert!(is_valid_slug("VHS-rips"));
        assert!(is_valid_slug("a")); // single char is fine
        // Non-ASCII scripts: letters are alphanumeric in Unicode, path traversal
        // characters (/.\:%) are not — so non-ASCII collections are allowed.
        assert!(is_valid_slug("映画コレクション"));
        assert!(is_valid_slug("фильмы-2023"));
        assert!(is_valid_slug("韓国ドラマ-collection"));
    }

    /// QURATOR-259 — the charset is single-sourced: `is_valid_slug` here is a pure delegate to
    /// `hb_core::ticket::is_valid_slug`, where `verify_shape` enforces the same predicate on wire
    /// tickets. Pin the DELEGATION, not just the behaviour: if this ever becomes a second, local
    /// implementation that has drifted (e.g. one that admits `_` or `|`), local creation and the
    /// wire boundary disagree — a peer's ticket could then name a slug this node would refuse to
    /// create, or a wire alias (`"X|Y"` re-spelling the re-serve key for author X, slug Y) could
    /// point at a slug the creation gate would never mint. The behaviour tests above stay green
    /// through such a drift; only this equality catches it.
    ///
    /// MUTATION (P-10): replace the delegated body (crates/hb-app/src/commands/collection.rs,
    /// `pub(crate) fn is_valid_slug`, the `hb_core::ticket::is_valid_slug(slug)` line) with a local
    /// re-implementation that also allows `_` — `!slug.is_empty() && slug.chars().all(|c|
    /// c.is_alphanumeric() || c == '-' || c == '_')` — the `under_score` probe reds while every
    /// behaviour test above stays green. (Line-number anchor beats text: the mutation text appears
    /// in this comment too.)
    #[test]
    fn local_slug_charset_is_exactly_the_core_wire_charset() {
        for probe in [
            "ok-slug", "films-2026", "a", "映画コレクション", "фильмы-2023",
            "under_score", "a|b", "a/b", "a.b", "a b", "", "a%b", "a:b",
        ] {
            assert_eq!(
                is_valid_slug(probe),
                hb_core::ticket::is_valid_slug(probe),
                "the local creation gate and the wire gate must be ONE charset; they disagree on {probe:?}"
            );
        }
    }

    /// QURATOR-249: the fixed marker keys that share the flat `published/` namespace with
    /// collection slugs are reserved — charset-valid but collision-bound. `is_valid_slug`
    /// itself deliberately stays silent on them (lookups like delete/unpublish must keep
    /// accepting a legacy draft), so the reservation is its own predicate.
    ///
    /// Mutation to redden: change `is_reserved_marker_slug`'s body to `false` (or empty
    /// `RESERVED_MARKER_SLUGS`) — the first assert reds.
    #[test]
    fn reserved_marker_slugs_are_exactly_the_fixed_keys() {
        assert!(is_reserved_marker_slug("profile"));
        assert!(!is_reserved_marker_slug("profiles"), "prefix-sharing slug is its own key");
        assert!(!is_reserved_marker_slug("films"));
        assert!(!is_reserved_marker_slug(""));
    }

    #[test]
    fn format_size_uses_correct_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1_023), "1023 B");
        assert_eq!(format_size(1_024), "1.0 KB");
        assert_eq!(format_size(1_048_576), "1.0 MB");
        assert_eq!(format_size(1_073_741_824), "1.0 GB");
        assert_eq!(format_size(10 * 1_073_741_824), "10.0 GB");
    }

    // ── T15 acceptance tests ─────────────────────────────────────────────────

    fn make_dir_tree(root: &std::path::Path) {
        // level1/level2/level3/deep.txt  (3 levels under root)
        let deep = root.join("level1").join("level2").join("level3");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.txt"), b"x").unwrap();
        std::fs::write(root.join("level1").join("level2").join("mid.txt"), b"x").unwrap();
        std::fs::write(root.join("level1").join("top.txt"), b"x").unwrap();
        std::fs::write(root.join("root.txt"), b"x").unwrap();
    }

    fn empty_globs() -> globset::GlobSet {
        build_glob_set(&[]).unwrap()
    }

    /// Build an `IncludeSet` from string slices (test ergonomics).
    fn include(paths: &[&str]) -> IncludeSet {
        IncludeSet::new(paths.iter().map(|s| s.to_string()).collect())
    }

    /// Selection-walk fixture:
    ///   root.txt
    ///   a/ a_loose.txt  b/ b_file.txt  c/ c_file.txt
    ///   x/ x_loose.txt  y/ y_file.txt
    fn make_selective_tree(root: &std::path::Path) {
        let abc = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&abc).unwrap();
        std::fs::write(abc.join("c_file.txt"), b"x").unwrap();
        std::fs::write(root.join("a").join("b").join("b_file.txt"), b"x").unwrap();
        std::fs::write(root.join("a").join("a_loose.txt"), b"x").unwrap();
        let xy = root.join("x").join("y");
        std::fs::create_dir_all(&xy).unwrap();
        std::fs::write(xy.join("y_file.txt"), b"x").unwrap();
        std::fs::write(root.join("x").join("x_loose.txt"), b"x").unwrap();
        std::fs::write(root.join("root.txt"), b"x").unwrap();
    }

    // ── Track F: scan_selective (selection-aware walk) ────────────────────────

    /// (a) A subset of subdirs `include`d → only those recurse fully; others skipped.
    #[test]
    fn scan_selective_only_recurses_included_subtree() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        let (items, _) = scan_selective(dir.path(), &include(&["a"]), &empty_globs()).unwrap();
        let json = serde_json::to_string(&items).unwrap();
        // The whole `a` subtree is present...
        assert!(json.contains("a_loose.txt"), "included dir's loose files present");
        assert!(json.contains("b_file.txt"), "included dir recurses fully");
        assert!(json.contains("c_file.txt"), "included dir recurses to full depth");
        // ...and the unselected `x` subtree is entirely absent.
        assert!(!json.contains("x_loose.txt"), "unselected dir's files must be absent");
        assert!(!json.contains("y_file.txt"), "unselected dir is not walked");
    }

    /// (b) Ancestor-only traversal — `include = ["a/b"]` traverses `a` but does NOT list `a`'s loose
    /// files; fully lists `a/b`.
    #[test]
    fn scan_selective_ancestor_only_omits_loose_files() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        let (items, _) = scan_selective(dir.path(), &include(&["a/b"]), &empty_globs()).unwrap();
        let json = serde_json::to_string(&items).unwrap();
        // `a` is only an ancestor → traversed to reach a/b, but its own loose files are withheld.
        assert!(!json.contains("a_loose.txt"), "ancestor-only dir's loose files must be withheld");
        // a/b is the selection → fully listed.
        assert!(json.contains("b_file.txt"), "selected subdir's files present");
        assert!(json.contains("c_file.txt"), "selected subdir recurses fully");
        // the `a` folder node still exists (so the path to a/b renders).
        let a = items.iter().find(|i| i.name == "a").expect("a folder node present as a path");
        assert_eq!(a.item_type, ItemType::Folder);
        assert!(a.children.iter().any(|c| c.name == "b"), "a contains the selected b");
        assert!(!a.children.iter().any(|c| c.item_type == ItemType::File),
            "ancestor-only `a` lists no loose files of its own");
        // unselected sibling `x` absent.
        assert!(!json.contains("x_loose.txt"));
    }

    /// (c) Root-level loose files are always present regardless of `include`.
    #[test]
    fn scan_selective_root_files_always_present() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        for inc in [include(&[]), include(&["x"]), include(&["a/b"])] {
            let (items, _) = scan_selective(dir.path(), &inc, &empty_globs()).unwrap();
            assert!(items.iter().any(|i| i.name == "root.txt"),
                "root-level loose files are always included");
        }
    }

    /// (d) `include = []` → root files only, no subdir contents.
    #[test]
    fn scan_selective_empty_include_is_root_only() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        let (items, _) = scan_selective(dir.path(), &include(&[]), &empty_globs()).unwrap();
        let json = serde_json::to_string(&items).unwrap();
        assert!(json.contains("root.txt"), "root files present");
        // No subdir is selected → none are listed at all.
        assert!(!json.contains("a_loose.txt"));
        assert!(!json.contains("b_file.txt"));
        assert!(!json.contains("x_loose.txt"));
        assert!(items.iter().all(|i| i.item_type == ItemType::File),
            "with no selection, only loose root files appear (no folders)");
    }

    // ── QURATOR-336: oversized collections (estimate → BFS teaser → oversized bit) ────────────

    /// QURATOR-336 (1): the oversized probe must decide from a PREFIX of the tree, not from a full
    /// count — the entire point of the estimator is that a 40-million-file collection costs a
    /// handful of `read_dir`s to classify. The threshold is a parameter precisely so this test can
    /// use four real files instead of 150,000.
    ///
    /// Mutation to redden: in `estimate_item_count` (collection.rs:1905), move the early return out of the loop — e.g.
    /// change the file arm's `return Ok(Estimate { count, complete: false });` to a no-op so the
    /// pass finishes and falls through to `Ok(Estimate { count, complete: true })` — the count then
    /// comes back at the tree's real total (60, `complete: true`) and the first assert reds.
    /// Equally, returning `Estimate { count: threshold, complete: false }` where collection.rs:1950
    /// returns `count` reds it, since the value is pinned.
    #[test]
    fn oversized_estimator_stops_at_the_threshold_without_walking_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        // 60 root files, all listed (root loose files are always items). Threshold 3.
        for i in 0..60 {
            std::fs::write(dir.path().join(format!("f{i:02}.txt")), b"x").unwrap();
        }
        let stopped = estimate_item_count(dir.path(), &include(&[]), &empty_globs(), 3).unwrap();
        assert_eq!(
            stopped,
            Estimate { count: 4, complete: false },
            "the probe stops on the 4th item it sees, not the 60th"
        );

        // …and a tree at or under the threshold is counted in FULL (`complete`), so the caller
        // takes today's path.
        let small = tempfile::tempdir().unwrap();
        std::fs::write(small.path().join("only.txt"), b"x").unwrap();
        assert_eq!(
            estimate_item_count(small.path(), &include(&[]), &empty_globs(), 3).unwrap(),
            Estimate { count: 1, complete: true }
        );
    }

    /// QURATOR-336 (1) cont'd: the estimator is a *second traversal* of the same selection rules,
    /// which is exactly the thing that can silently drift from the walk. This pins the two against
    /// each other on the shared fixture — with the threshold out of reach the estimator's count must
    /// equal `count_items` of the very tree `scan_selective_walk` builds, for every selection shape
    /// and under an exclusion globset too.
    ///
    /// Mutation to redden: in `estimate_item_count` (collection.rs:1947), replace the file arm's
    /// `file_is_listed(&rel_path, loose, include)` with `true` — the ancestor-only case then counts
    /// `a`'s withheld loose files, the estimator's count exceeds the walk's, and the `assert_eq!`
    /// reds. (Equally: drop the `dir_is_descended` guard at collection.rs:1931 — every unselected
    /// subtree is then counted.)
    #[test]
    fn oversized_estimator_counts_the_same_tree_the_walk_builds() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());
        let exclusions = [empty_globs(), build_glob_set(&["root.txt".to_string()]).unwrap()];
        for exclude in &exclusions {
            for inc in
                [include(&[]), include(&["a"]), include(&["a/b"]), include(&["x", "a/b"])]
            {
                let est = estimate_item_count(dir.path(), &inc, exclude, u64::MAX).unwrap();
                assert!(est.complete, "threshold MAX cannot stop early");
                let (walked, _) = scan_selective_walk(dir.path(), &inc, exclude).unwrap();
                assert_eq!(
                    est.count,
                    count_items(&walked),
                    "the estimator and the walk must agree on what this selection contains"
                );
            }
        }
    }

    /// QURATOR-336 (3): the teaser is built **breadth-first** — every depth-1 entry before any
    /// depth-2 entry — and stops on the byte budget. Fixture: three folders (`d1`…`d3`, each holding
    /// three equal-length file names) plus one root file; folders sort before files at each level, as
    /// the walk orders them. The budget is calibrated to fit the whole of depth 1 plus exactly ONE
    /// depth-2 file, so the folder that ends up holding a child is the discriminator: breadth-first
    /// fills `d1` (its children were queued first), a LIFO/depth-first builder fills `d3`, and a
    /// recursive depth-first one never reaches `d2`/`d3` at all.
    ///
    /// Mutation to redden: in `scan_selective_bfs_teaser`, make the queue LIFO — `queue.pop_front()` (collection.rs:1989)
    /// → `queue.pop_back()` (or push_front instead of push_back) — `d3` then holds the single
    /// child and the `d1`/`d2`/`d3` child-count asserts red.
    #[test]
    fn oversized_teaser_is_breadth_first_and_never_overfills_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["d1", "d2", "d3"] {
            let sub = dir.path().join(d);
            std::fs::create_dir(&sub).unwrap();
            for f in ["x1", "x2", "x3"] {
                std::fs::write(sub.join(format!("{f}.txt")), b"x").unwrap();
            }
        }
        std::fs::write(dir.path().join("root.txt"), b"x").unwrap();

        // Calibrate against the production measure, over items of exactly the shapes the scan will
        // build (equal-length names, one-byte files, `.txt`). Files are written as b"x", so the
        // production `format_size(1)`/`"TXT"` pair is what these stand in for.
        let folder_cost = teaser_item_bytes(&DirectoryItem {
            name: "d1".into(),
            item_type: ItemType::Folder,
            size: None,
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        });
        let child_cost = teaser_item_bytes(&DirectoryItem {
            name: "x1.txt".into(),
            item_type: ItemType::File,
            size: Some(format_size(1)),
            format: Some("TXT".into()),
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        });
        let root_file_cost = teaser_item_bytes(&DirectoryItem {
            name: "root.txt".into(),
            item_type: ItemType::File,
            size: Some(format_size(1)),
            format: Some("TXT".into()),
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        });
        // 2 (the array's brackets) + the three folders + the root file + one depth-2 file.
        let budget = 2 + 3 * folder_cost + root_file_cost + child_cost;
        let inc = include(&["d1", "d2", "d3"]);

        let (tea, _) =
            scan_selective_bfs_teaser(dir.path(), &inc, &empty_globs(), budget).unwrap();

        // Depth 1 is complete (folders first, then the loose root file) BEFORE any depth-2 entry.
        let top: Vec<&str> = tea.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(top, ["d1", "d2", "d3", "root.txt"], "every depth-1 entry is present, in order");
        let d1 = &tea[0];
        assert_eq!(d1.children.len(), 1, "d1 was queued first, so it takes the one spare slot");
        assert_eq!(d1.children[0].name, "x1.txt");
        assert!(tea[1].children.is_empty(), "d2 is reached but the budget is spent");
        assert!(tea[2].children.is_empty(), "d3 is reached but the budget is spent");
        // The budget holds: the builder can only under-fill it (its per-item accounting is an
        // over-estimate of the serialized array).
        let serialized = serde_json::to_string(&tea).unwrap().len();
        assert!(serialized <= budget, "{serialized} bytes serialized into a {budget}-byte budget");
    }

    /// QURATOR-336 (2)+(3): which path a scan takes is decided by the estimator alone, and the
    /// ordinary path is untouched. Under the threshold: the full selection walk, `oversized` absent,
    /// real byte total. Past it: the teaser branch, with the crossing count reported (the
    /// threshold-crossing LOWER BOUND the UI renders as "n+ items").
    ///
    /// Mutation to redden: in `scan_selective_sized_with_threshold`, change collection.rs:1859's
    /// `if let Some(lower_bound) = …into_oversized()` to `if false` — the second half's
    /// `assert_eq!(big.oversized, Some(1))` reds (the tree would go through the walk and, at 0, hit
    /// the item cap instead). Also: `OVERSIZED_ITEM_THRESHOLD` → `MAX_COLLECTION_ITEMS` would red the
    /// first half only if the fixture were over 100k items, so it does *not* — the threshold's value
    /// is pinned by `oversized_flag_rides_in_the_listing_and_is_absent_when_not_oversized` instead.
    #[test]
    fn oversized_branch_replaces_the_plain_scan_only_past_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());
        let inc = include(&["a"]);

        let scan =
            scan_selective_sized_with_threshold(dir.path(), &inc, &empty_globs(), 10).unwrap();
        assert_eq!(scan.oversized, None, "a tree under the threshold is not oversized");
        let json = serde_json::to_string(&scan.items).unwrap();
        assert!(json.contains("c_file.txt"), "the ordinary path still recurses the selection");
        assert!(!json.contains("x_loose.txt"), "the ordinary path still skips unselected dirs");
        assert_eq!(scan.total_bytes, 4, "a_loose + b_file + c_file + root.txt, one byte each");

        // Threshold 0 ⇒ the very first counted item crosses it.
        let big =
            scan_selective_sized_with_threshold(dir.path(), &inc, &empty_globs(), 0).unwrap();
        assert_eq!(big.oversized, Some(1), "the estimator's crossing count is what is reported");
    }

    /// QURATOR-336 wire rule (CLAUDE.md §6): `oversized` is a new OPTIONAL listing field. Present ⇒
    /// the browse side can render "n+ items"; absent ⇒ exactly the listing shape that existed before
    /// this change, which any reader (old or new) parses unchanged. So there is no weaker path to
    /// fall into, and no `parts_v` / discriminant bump. Both shapes are pinned here, and the field is
    /// pinned against `OVERSIZED_ITEM_THRESHOLD` itself (the boundary: at the threshold, nothing).
    ///
    /// Mutation to redden: in `collection_to_listing_json` (collection.rs:668), delete the `map.insert("oversized"…)`
    /// block — the first assert reds. Change `collection_is_oversized`'s `>` to `>=` (collection.rs:646) — the
    /// at-the-threshold case then carries the key and the third assert reds.
    #[test]
    fn oversized_flag_rides_in_the_listing_and_is_absent_when_not_oversized() {
        let mut big = a_big_collection("big");
        big.item_count = OVERSIZED_ITEM_THRESHOLD + 1;
        big.content_types = vec!["Games".into()];
        let big_json = collection_to_listing_json(big).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&big_json).unwrap()["oversized"],
            serde_json::Value::Bool(true),
            "an oversized draft stamps the bit into the listing meta"
        );

        // …and it survives the teaser path, which rebuilds the envelope from its metadata (the same
        // place `snapshot_fingerprint` is re-stamped), so a browser sees it on the truncated listing
        // too.
        let teaser = hb_net::truncate_listing(&big_json, 120).unwrap();
        assert!(teaser.truncated, "precondition: this listing is cut down to a teaser");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&teaser.json).unwrap()["oversized"],
            serde_json::Value::Bool(true),
            "truncate_listing preserves unknown meta keys"
        );

        // At the threshold: absent — the key only ever marks a collection that CANNOT be sent whole.
        let mut plain = a_big_collection("plain");
        plain.item_count = OVERSIZED_ITEM_THRESHOLD;
        plain.content_types = vec!["Games".into()];
        let plain_json = collection_to_listing_json(plain).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&plain_json).unwrap();
        assert!(parsed.get("oversized").is_none(), "absent when not oversized: {plain_json}");

        // A listing published before this change carries no key at all and still reads as an
        // ordinary Collection — the "old reader / old listing" half of the wire pin.
        let legacy = r#"{"slug":"old","path_alias":"Old","item_count":3,
            "last_updated":"2026-04-01T00:00:00Z","listing":[]}"#;
        let old: Collection = serde_json::from_str(legacy).unwrap();
        assert!(!collection_is_oversized(&old), "an old listing reads as not oversized");
    }

    /// QURATOR-336 (6): an oversized collection must never be walked into a full manifest — the
    /// envelope would serialize and seal minutes of parts to produce something the 16 MiB transport
    /// ceiling then refuses. The refusal names the reason, and the boundary is exact: a draft at the
    /// threshold still exports.
    ///
    /// Mutation to redden: in `build_slug_manifest` (collection.rs:1182), delete the `if collection_is_oversized(&col)`
    /// block — the first assert reds (the call then proceeds past it and returns `Ok`, so
    /// `unwrap_err` panics).
    #[test]
    fn build_slug_manifest_refuses_an_oversized_collection() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();

        let mut huge = a_big_collection("huge");
        huge.item_count = OVERSIZED_ITEM_THRESHOLD + 1;
        huge.content_types = vec!["Games".into()];
        store.save_collection_draft(&huge).unwrap();
        let err = build_slug_manifest("huge", &store, &identity, &[7u8; 32]).unwrap_err();
        assert!(err.contains("too large to send in full"), "got: {err}");

        // One item under the line is not oversized — the guard must not fire early and refuse a
        // collection that fits.
        let mut fits = a_big_collection("fits");
        fits.item_count = OVERSIZED_ITEM_THRESHOLD;
        fits.content_types = vec!["Games".into()];
        store.save_collection_draft(&fits).unwrap();
        assert!(
            build_slug_manifest("fits", &store, &identity, &[7u8; 32]).is_ok(),
            "at the threshold the manifest still builds"
        );
    }

    /// QURATOR-253: the item cap must fire DURING the walk, not after it. A flat directory of
    /// MAX_COLLECTION_ITEMS + 5 files is over the cap either way, but only the in-walk check can
    /// abort at item 100,001 and say so — a post-hoc-only cap walks the whole tree first and
    /// reports the true total (100,005). The count inside the cap error is therefore the
    /// discriminator between enforcing during the walk and enforcing after it.
    ///
    /// Mutation to redden: in `WalkFrame::scan`'s file arm, delete the two lines immediately
    /// after the `frame.items.push(DirectoryItem { ... });` for files — `*seen += 1;` and
    /// `enforce_item_cap(*seen).map_err(|e| anyhow::anyhow!(e))?;` — the walk then completes,
    /// the post-walk check in `scan_selective` reports "100005 items", and the "100001 items"
    /// assert below reds. (This flat-files tree pins the FILE half of the counting; deleting the
    /// folder half in `scan_selective_walk`'s completion arm alone does not red it, but can only
    /// delay a folder-overflow abort back to the post-walk check — never let an over-cap tree
    /// through, since the post-walk check counts folders too.)
    #[test]
    fn scan_selective_aborts_the_walk_at_the_item_cap_not_after_it() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_COLLECTION_ITEMS + 5) {
            std::fs::write(dir.path().join(format!("f{i:06}.txt")), b"x").unwrap();
        }
        let err = scan_selective(dir.path(), &include(&[]), &empty_globs())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("100001 items, over the"),
            "expected the in-walk cap to abort at item 100001 (not the post-walk total), got: {err}"
        );
    }

    /// (e) F1 containment — an `include` entry that escapes the canonicalized root is a reasoned
    /// `Err` from scan_selective's OWN guard, and NOTHING outside the root is ever listed. This is
    /// the selection-walk privacy boundary.
    #[test]
    fn scan_selective_rejects_escaping_include_relative() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());
        let err = scan_selective(dir.path(), &include(&["../../../etc"]), &empty_globs())
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes") || err.contains(".."), "reasoned containment error: {err}");
    }

    #[test]
    fn scan_selective_rejects_absolute_include() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());
        let abs = if cfg!(windows) { "C:\\Windows" } else { "/etc" };
        let err = scan_selective(dir.path(), &include(&[abs]), &empty_globs())
            .unwrap_err()
            .to_string();
        assert!(!err.is_empty(), "absolute include path rejected: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn scan_selective_rejects_symlink_escape() {
        // A checked subdir that is a symlink pointing OUTSIDE the root must not exfiltrate files:
        // canonicalize() follows the link and the under-root prefix check fails.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"top secret").unwrap();

        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

        let err = scan_selective(dir.path(), &include(&["escape"]), &empty_globs())
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes") || err.contains("root"), "symlink escape rejected: {err}");

        // And the legitimate selection never leaks the outside file.
        let (items, _) = scan_selective(dir.path(), &include(&["a"]), &empty_globs()).unwrap();
        let json = serde_json::to_string(&items).unwrap();
        assert!(!json.contains("secret.txt"), "no file outside the root is ever listed");
    }

    /// audit #9: a pathologically deep nested tree (a hostile archive / synced share) must be
    /// rejected LOUDLY at the depth bound — not recursed until the blocking scan thread's stack
    /// overflows (the 30 s deadline can't cancel a detached walker and the item cap is checked only
    /// after it returns). The bound is far beyond any real collection, so a normal scan is untouched.
    /// `#[cfg(unix)]`: Windows `MAX_PATH` (~260 chars) makes a 257-level tree unbuildable there, and
    /// the OS limit is itself what bounds nesting on that platform.
    #[cfg(unix)]
    #[test]
    fn scan_selective_rejects_pathologically_deep_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        // One-character components on purpose. `d0/d1/…/d258` is ~1,185 chars, which clears Linux's
        // 4096-byte PATH_MAX but NOT macOS's 1024 — it failed there with ENAMETOOLONG while passing
        // on ubuntu, so the tree, not the guard, was the platform-specific part. `d/d/…/d` is ~518:
        // still far past Windows' ~260 MAX_PATH (so `#[cfg(unix)]` above remains right), still
        // MAX_SCAN_DEPTH + 3 levels deep, and now actually buildable on every unix CI runner.
        for _ in 0..(MAX_SCAN_DEPTH + 3) {
            path = path.join("d");
        }
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("leaf.txt"), b"x").unwrap();

        let err = scan_selective(dir.path(), &include(&["d"]), &empty_globs())
            .unwrap_err()
            .to_string();
        assert!(err.contains("depth"), "deep tree is rejected with a loud, reasoned error: {err}");
    }

    /// Equivalence fixture: exercises every selection rule at once — root loose files with mixed
    /// case (pins the dirs-first/case-insensitive sort), a fully-selected subtree with nesting and
    /// varied file sizes (byte roll-up into ancestors), an ancestor-only selection (its own loose
    /// files withheld), an individually-checked file inside an otherwise unselected directory
    /// (devtest #10), an excluded glob, and a fully-skipped sibling.
    fn make_equivalence_tree(root: &std::path::Path) {
        let sel = root.join("Sel").join("nested").join("deeper");
        std::fs::create_dir_all(&sel).unwrap();
        std::fs::write(sel.join("z_deep.bin"), [0u8; 4096]).unwrap();
        std::fs::write(root.join("Sel").join("nested").join("a_mid.txt"), b"mid").unwrap();
        std::fs::write(root.join("Sel").join("m_loose.txt"), b"loose").unwrap();
        let anc = root.join("anc").join("leaf");
        std::fs::create_dir_all(&anc).unwrap();
        std::fs::write(anc.join("l_file.txt"), b"l").unwrap();
        std::fs::write(root.join("anc").join("withheld.txt"), b"w").unwrap();
        let solo = root.join("solo");
        std::fs::create_dir_all(&solo).unwrap();
        std::fs::write(solo.join("picked.txt"), b"p").unwrap();
        std::fs::write(solo.join("not_picked.txt"), b"n").unwrap();
        std::fs::create_dir_all(root.join("skip")).unwrap();
        std::fs::write(root.join("skip").join("never.txt"), b"x").unwrap();
        std::fs::write(root.join("Apple.md"), b"a").unwrap();
        std::fs::write(root.join("banana.TXT"), b"bb").unwrap();
        std::fs::write(root.join("noise.skip"), b"s").unwrap();
    }

    /// Audit #9 follow-up (2026-08-25), equivalence pin: the iterative `scan_selective_walk` must
    /// produce byte-identical output to the recursive implementation it replaced, on a fixture
    /// exercising every selection rule at once (see `make_equivalence_tree`). The expected JSON
    /// below is a verbatim capture from the RECURSIVE code, taken before the rewrite — not
    /// re-derived afterwards, which would pin whatever the new code happens to emit.
    #[test]
    fn scan_selective_matches_recursive_baseline() {
        let dir = tempfile::tempdir().unwrap();
        make_equivalence_tree(dir.path());
        let (items, total) = scan_selective(
            dir.path(),
            &include(&["Sel", "anc/leaf", "solo/picked.txt"]),
            &build_glob_set(&["*.skip".to_string()]).unwrap(),
        )
        .unwrap();
        let json = serde_json::to_string(&items).unwrap();
        assert_eq!(
            json,
            r#"[{"name":"anc","item_type":"Folder","tags":[],"children":[{"name":"leaf","item_type":"Folder","tags":[],"children":[{"name":"l_file.txt","item_type":"File","size":"1 B","format":"TXT","tags":[],"children":[]}]}]},{"name":"Sel","item_type":"Folder","tags":[],"children":[{"name":"nested","item_type":"Folder","tags":[],"children":[{"name":"deeper","item_type":"Folder","tags":[],"children":[{"name":"z_deep.bin","item_type":"File","size":"4.0 KB","format":"BIN","tags":[],"children":[]}]},{"name":"a_mid.txt","item_type":"File","size":"3 B","format":"TXT","tags":[],"children":[]}]},{"name":"m_loose.txt","item_type":"File","size":"5 B","format":"TXT","tags":[],"children":[]}]},{"name":"solo","item_type":"Folder","tags":[],"children":[{"name":"picked.txt","item_type":"File","size":"1 B","format":"TXT","tags":[],"children":[]}]},{"name":"Apple.md","item_type":"File","size":"1 B","format":"MD","tags":[],"children":[]},{"name":"banana.TXT","item_type":"File","size":"2 B","format":"TXT","tags":[],"children":[]}]"#,
            "iterative walk must be byte-identical to the recursive baseline"
        );
        // 4109 = every selected file's bytes, rolled up through ancestors to the root.
        assert_eq!(total, 4109, "children's bytes must roll up into the root total");
    }

    /// Audit #9 follow-up (2026-08-25), THE DISCRIMINATOR: a deep tree scanned from a thread with
    /// a deliberately tiny stack must yield the depth `Err`, not kill the process. The recursive
    /// walk needed ~2–4 KB of stack per level, so 256 levels ≈ 0.8–1 MB on Linux: at this 512 KB
    /// budget it provably overflowed and aborted (verified red against the recursive original,
    /// which died with `thread ... has overflowed its stack` before the guard could fire), while
    /// the iterative walk's stack usage is O(1) in depth and passes. Any refactor that quietly
    /// reintroduces recursion reds here instead of taking down CI.
    ///
    /// 512 KiB is chosen because it is (a) comfortably above the ~32 KiB a plain test thread's
    /// own frames need, and (b) provably below the recursive walk's ~0.8–1 MB footprint — the
    /// tight window where only the iterative implementation survives. `RUST_MIN_STACK` cannot
    /// substitute: it sizes *spawned* threads, not the harness's own main/test threads.
    /// ⚠ Fixture setup AND teardown deliberately stay on the harness thread; ONLY the call under
    /// test runs on the small stack. `TempDir`'s drop calls `std::fs::remove_dir_all`, which is
    /// **itself recursive per directory level on Unix** — tearing down a 259-level tree inside
    /// 512 KiB overflows on macOS no matter how `scan_selective` behaves, which is exactly how the
    /// first version of this test failed CI (run 32821343962, `thread '<unknown>' has overflowed
    /// its stack`). A discriminator has to bound the code under test and nothing else, or it
    /// reports on its own scaffolding.
    #[test]
    fn scan_selective_deep_tree_survives_tiny_thread_stack() {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        // One-char components keep the ~518-char path under macOS's 1024-byte PATH_MAX
        // (see `scan_selective_rejects_pathologically_deep_tree`).
        for _ in 0..(MAX_SCAN_DEPTH + 3) {
            path = path.join("d");
        }
        std::fs::create_dir_all(&path).unwrap();

        let root = dir.path().to_path_buf();
        let err = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                // The loud depth Err — same shape, same depth, as the recursive original's.
                scan_selective(&root, &include(&["d"]), &empty_globs()).unwrap_err().to_string()
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(err.contains("depth"), "deep tree is rejected with a loud, reasoned error: {err}");
    }

    // ── IncludeSet truth table (mirrors the frontend scan-tree.ts) ────────────

    #[test]
    fn include_set_is_included_and_descendant_logic() {
        let inc = include(&["a", "x/y"]);
        // is_included: exact or under a checked ancestor.
        assert!(inc.is_included("a"));
        assert!(inc.is_included("a/b"));
        assert!(inc.is_included("a/b/c"));
        assert!(inc.is_included("x/y"));
        assert!(inc.is_included("x/y/z"));
        assert!(!inc.is_included("x"), "x is only an ancestor of the checked x/y");
        assert!(!inc.is_included("ab"), "prefix must respect the path separator (not 'a' ⊂ 'ab')");
        // has_descendant_under: some checked path lives strictly below `rel`.
        assert!(inc.has_descendant_under("x"));
        assert!(!inc.has_descendant_under("a"), "a is itself checked, not an ancestor-of-checked");
        assert!(!inc.has_descendant_under("x/y"), "x/y is the checked leaf, has no checked descendant");
    }

    // ── list_subdirs (lazy child enumeration for the picker) ──────────────────

    #[test]
    fn list_subdirs_returns_dirs_then_files_with_has_children() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        let entries = list_subdirs_core(&dir.path().to_string_lossy()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        // devtest #10: immediate children now include FILES — directories first (sorted), then files.
        assert_eq!(names, vec!["a", "x", "root.txt"], "dirs first, then files, each sorted");
        let a = entries.iter().find(|e| e.name == "a").unwrap();
        assert!(!a.is_file && a.has_children, "a is a dir with children → expander shown");
        let root_file = entries.iter().find(|e| e.name == "root.txt").unwrap();
        assert!(root_file.is_file && !root_file.has_children, "a file is a leaf, never expandable");
        // A directory that holds only files is still expandable (so its files can be picked).
        let leaf = list_subdirs_core(&dir.path().join("a").join("b").join("c").to_string_lossy()).unwrap();
        assert_eq!(leaf.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["c_file.txt"]);
        assert!(leaf[0].is_file, "c holds a file leaf");
    }

    /// devtest #10: an explicitly-checked FILE deep inside otherwise-unselected directories is
    /// included — without pulling in its siblings — so the user can curate individual files.
    #[test]
    fn scan_selective_includes_an_individually_checked_file() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        let (items, _) = scan_selective(dir.path(), &include(&["a/b/c/c_file.txt"]), &empty_globs()).unwrap();
        let json = serde_json::to_string(&items).unwrap();
        // The checked file is present...
        assert!(json.contains("c_file.txt"), "the individually-checked file is included");
        // ...but its unchecked siblings (a's + b's loose files) are NOT pulled in wholesale.
        assert!(!json.contains("a_loose.txt"), "an ancestor's loose files stay withheld");
        assert!(!json.contains("b_file.txt"), "a sibling file in an ancestor dir is not included");
        // and the unrelated `x` subtree is untouched.
        assert!(!json.contains("x_loose.txt"));
    }

    #[test]
    fn list_subdirs_nonexistent_path_is_reasoned_err_not_panic() {
        let err = list_subdirs_core("/no/such/path/xyzzy-7f3a").unwrap_err().to_string();
        assert!(!err.is_empty(), "missing path returns a reasoned Err");
    }

    #[test]
    fn deadline_helper_returns_err_on_wedged_work() {
        // Simulates a wedged SMB read_dir: the work outlives the deadline → Err, never a hang.
        let res: Result<(), String> = run_blocking_with_deadline(
            || {
                std::thread::sleep(std::time::Duration::from_millis(400));
                Ok(())
            },
            std::time::Duration::from_millis(50),
        );
        assert!(res.is_err(), "work that outlives the deadline must error, not block");
    }

    #[test]
    fn exclude_glob_applied() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("movie.mkv"), b"x").unwrap();
        std::fs::write(dir.path().join("movie.nfo"), b"x").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();

        let globs = build_glob_set(&["*.nfo".to_string()]).unwrap();
        let (items, _) = scan_selective(dir.path(), &include(&[]), &globs).unwrap();
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"movie.mkv"));
        assert!(names.contains(&"readme.txt"));
        assert!(!names.contains(&"movie.nfo"), "*.nfo must be excluded");
    }

    #[test]
    fn exclude_glob_nested() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Season 1");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("ep1.nfo"), b"x").unwrap();
        std::fs::write(sub.join("ep1.mkv"), b"x").unwrap();

        let globs = build_glob_set(&["**/*.nfo".to_string()]).unwrap();
        let (items, _) = scan_selective(dir.path(), &include(&["Season 1"]), &globs).unwrap();
        let json = serde_json::to_string(&items).unwrap();
        assert!(!json.contains("ep1.nfo"), "nested *.nfo must be excluded by **/*.nfo glob");
        assert!(json.contains("ep1.mkv"), "mkv must remain");
    }

    #[test]
    fn item_count_accurate() {
        let dir = tempfile::tempdir().unwrap();
        make_dir_tree(dir.path()); // root.txt + level1/(top.txt + level2/(mid.txt + level3/(deep.txt)))
        // Selecting the top-level `level1` walks its whole subtree (full depth — the point of the
        // selective walk); root.txt is always included.
        let (items, _) = scan_selective(dir.path(), &include(&["level1"]), &empty_globs()).unwrap();
        let total = count_items(&items);
        // Items: root.txt, level1(dir), top.txt, level2(dir), mid.txt, level3(dir), deep.txt = 7
        assert_eq!(total, 7, "expected 7 items (4 files + 3 dirs), got {total}");
    }

    /// Regression: scanning an *empty* directory must return promptly and
    /// successfully — it must not leave the UI stuck on "Scanning…" forever.
    /// This exercises the real async path (`spawn_blocking` + `tokio::time::timeout`)
    /// through `scan_directory_inner`. The outer `timeout` ensures a regression
    /// that makes the command hang (e.g. a panic in the spawn/timeout path that
    /// drops the IPC response) fails the test instead of hanging CI.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scan_empty_directory_completes_without_hang() {
        use tempfile::TempDir;

        let work = TempDir::new().unwrap(); // the empty folder being scanned
        let data = TempDir::new().unwrap(); // datastore root
        let store = DataStore::new(data.path().to_path_buf());

        let opts = ScanOptions {
            path: work.path().to_string_lossy().into_owned(),
            path_alias: "Empty Folder".into(),
            include: vec![],
            exclude: vec![],
        };

        let collection = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            scan_directory_inner(opts, &store),
        )
        .await
        .expect("scan of an empty directory must complete, not hang on \"Scanning…\"")
        .expect("scan of an empty directory must succeed");

        assert_eq!(collection.item_count, 0, "empty folder has zero items");
        assert!(collection.est_size.is_none(), "empty folder has no size estimate");
        assert!(collection.listing.is_empty(), "empty folder has an empty listing");

        // QURATOR-207: the scan persists nothing durable — the empty collection lives in the
        // in-memory scan cache until Publish promotes it (no orphan draft on Cancel).
        let draft = store.load_collection_draft(&collection.slug).unwrap();
        assert!(draft.is_none(), "empty-folder scan must NOT save a draft (QURATOR-207)");
        assert!(
            scan_cache().lock().unwrap().get(&collection.slug).is_some(),
            "empty-folder scan must populate the in-memory cache"
        );
    }

    /// Regression (devtest 2026-06-25 #5): the home "Total Size" / "Disk size (auto)" aggregate
    /// reads `total_bytes`, but the published `Collection` deliberately omits exact bytes (hb-core
    /// privacy invariant). The scanned byte total must therefore be persisted in the per-slug
    /// `ScanSpec` sidecar so `get_collections` can surface it on `CollectionEntry` — before the fix
    /// it was computed at scan time and dropped, so the aggregate always read "—".
    /// QURATOR-207: the spec is now persisted by Publish's promote step, not by the scan.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scan_persists_total_bytes_for_the_size_aggregate() {
        use tempfile::TempDir;
        let work = TempDir::new().unwrap();
        std::fs::write(work.path().join("a.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(work.path().join("b.bin"), vec![0u8; 2048]).unwrap();
        let data = TempDir::new().unwrap();
        let store = DataStore::new(data.path().to_path_buf());
        let opts = ScanOptions {
            path: work.path().to_string_lossy().into_owned(),
            path_alias: "Sized".into(),
            include: vec![], // root-level loose files are always included
            exclude: vec![],
        };
        let collection = scan_directory_inner(opts, &store).await.unwrap();
        // QURATOR-207: nothing is durable until Publish — the spec (and its byte total) appears
        // only when the waiting scan is promoted.
        assert!(
            store.load_scan_spec(&collection.slug).unwrap().is_none(),
            "scan must not persist a spec (QURATOR-207)"
        );
        // The promote gate requires ≥1 content type (the ruling keeps that gate) — tick one on the
        // waiting cache entry, as the Details form's update_collection_meta would.
        scan_cache()
            .lock()
            .unwrap()
            .get_mut(&collection.slug)
            .unwrap()
            .collection
            .content_types = vec!["video".into()];
        assert!(
            promote_cached_scan(&collection.slug, &store).unwrap(),
            "the scanned collection must be waiting in the cache for Publish to promote"
        );
        let spec = store.load_scan_spec(&collection.slug).unwrap().expect("promote persists a spec");
        assert_eq!(spec.total_bytes, 3048, "the byte total must be persisted for the UI size aggregate");
        // And the published Collection must STILL NOT carry total_bytes (privacy invariant unchanged).
        let json = serde_json::to_string(&collection).unwrap();
        assert!(!json.contains("total_bytes"), "the published Collection must not expose total_bytes");
    }

    /// Regression: a path containing spaces with a trailing separator (mimicking
    /// a Windows path such as `C:\Users\Flux T\Downloads\`) plus a display name
    /// with spaces must scan successfully — not hang, and not fail slug validation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scan_path_with_spaces_and_trailing_separator() {
        use tempfile::TempDir;

        let parent = TempDir::new().unwrap();
        let spaced = parent.path().join("My Empty Folder");
        std::fs::create_dir(&spaced).unwrap();
        // Append a trailing separator, mimicking C:\Users\FluxT\Downloads\
        let with_sep = format!("{}{}", spaced.to_string_lossy(), std::path::MAIN_SEPARATOR);

        let data = TempDir::new().unwrap();
        let store = DataStore::new(data.path().to_path_buf());

        let opts = ScanOptions {
            path: with_sep,
            path_alias: "My Downloads Backup".into(), // spaces → slug "my-downloads-backup"
            include: vec![],
            exclude: vec![],
        };

        let collection = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            scan_directory_inner(opts, &store),
        )
        .await
        .expect("scan of a spaced/trailing-separator path must complete, not hang")
        .expect("scan of a spaced/trailing-separator path must succeed");

        assert_eq!(collection.slug, "my-downloads-backup", "spaces in alias map to hyphens");
        assert_eq!(collection.item_count, 0);
    }

    /// Regression: drive the scan on **Tauri's own async runtime** — the same
    /// runtime that executes `#[tauri::command]`s in the real app. If the
    /// `tokio::time::timeout` + `spawn_blocking` construct were to panic on this
    /// runtime (e.g. a missing time driver), the spawned task would die without
    /// sending an IPC response and the dialog would hang on "Scanning…" forever.
    /// `block_on` of the join handle surfaces such a panic as an `Err` here.
    #[test]
    fn scan_completes_on_tauri_async_runtime() {
        use tempfile::TempDir;

        let work = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let store = DataStore::new(data.path().to_path_buf());

        let opts = ScanOptions {
            path: work.path().to_string_lossy().into_owned(),
            path_alias: "Empty".into(),
            include: vec![],
            exclude: vec![],
        };

        let handle =
            tauri::async_runtime::spawn(async move { scan_directory_inner(opts, &store).await });
        let collection = tauri::async_runtime::block_on(handle)
            .expect("command task must not panic on Tauri's async runtime")
            .expect("scan must succeed");

        assert_eq!(collection.item_count, 0);
    }

    #[test]
    fn regenerate_preserves_notes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("film.mkv"), b"x").unwrap();
        std::fs::write(dir.path().join("extra.txt"), b"x").unwrap();

        // First scan — simulate a note added to film.mkv.
        let (mut items, _) = scan_selective(dir.path(), &include(&[]), &empty_globs()).unwrap();
        for item in &mut items {
            if item.name == "film.mkv" {
                item.note = Some("Director's cut".into());
            }
        }

        // Second scan — fresh, no notes yet.
        let (new_items, _) = scan_selective(dir.path(), &include(&[]), &empty_globs()).unwrap();
        assert!(new_items.iter().all(|i| i.note.is_none()), "fresh scan has no notes");

        // Apply preserved notes, keyed by relative path.
        let notes = collect_notes(&items, "");
        let merged = apply_notes(new_items, &notes, "");
        let film = merged.iter().find(|i| i.name == "film.mkv").unwrap();
        assert_eq!(film.note.as_deref(), Some("Director's cut"));
        let extra = merged.iter().find(|i| i.name == "extra.txt").unwrap();
        assert!(extra.note.is_none(), "note-less item stays note-less");
    }

    #[test]
    fn regenerate_no_note_bleed_on_duplicate_names() {
        // Two files in different dirs both named "readme.txt" — notes must not cross-contaminate.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
        std::fs::write(sub.join("readme.txt"), b"x").unwrap();

        let (mut items, _) = scan_selective(dir.path(), &include(&["subdir"]), &empty_globs()).unwrap();
        // Add note only to the root readme.txt.
        for item in &mut items {
            if item.name == "readme.txt" {
                item.note = Some("root note".into());
            }
        }

        let (new_items, _) = scan_selective(dir.path(), &include(&["subdir"]), &empty_globs()).unwrap();
        let notes = collect_notes(&items, "");
        let merged = apply_notes(new_items, &notes, "");

        let root_readme = merged.iter().find(|i| i.name == "readme.txt").unwrap();
        assert_eq!(root_readme.note.as_deref(), Some("root note"));

        let subdir = merged.iter().find(|i| i.name == "subdir").unwrap();
        let sub_readme = subdir.children.iter().find(|i| i.name == "readme.txt").unwrap();
        assert!(sub_readme.note.is_none(), "subdirectory readme must not inherit root note");
    }

    // ── publish-path unit tests (pure; the wire is proven by hb-it Suite BROWSE) ──────────────

    fn make_collection_draft(store: &DataStore, slug: &str, content_types: Vec<String>) {
        let col = Collection {
            slug: slug.to_string(),
            path_alias: slug.to_string(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types,
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
    }

    #[test]
    fn envelope_at_caps_leaves_the_tree_its_budget() {
        // The metadata envelope and the directory tree share LISTING_MAX_BYTES: `truncate_listing`
        // serializes the envelope, subtracts it, and gives `entries` the remainder — a subtraction
        // that saturates to zero if the metadata alone is oversize, publishing a teaser with no
        // entries at all. This pins the worst case the ceilings in hb-core actually permit.
        //
        // Every field is at its ceiling and every character is U+0001, which serde_json escapes to
        // the six bytes `\u0001` — the most expensive encoding a single char has, worse than a
        // 4-byte emoji. So this is an upper bound no real collection can exceed.
        let worst = |n: usize| "\u{1}".repeat(n);
        let mut col = Collection {
            // The slug is not clamped by us — the filesystem bounds it, because it is the draft's
            // filename. Sized here at that bound so the worst case stays honest.
            slug: "s".repeat(hb_core::FILESYSTEM_SLUG_CHARS),
            path_alias: worst(hb_core::MAX_PATH_ALIAS_CHARS),
            description: Some(worst(hb_core::MAX_DESCRIPTION_CHARS)),
            item_count: u64::MAX,
            est_size: Some(worst(hb_core::MAX_EST_SIZE_CHARS)),
            content_types: (0..hb_core::MAX_CONTENT_TYPES)
                .map(|_| worst(hb_core::MAX_LIST_ITEM_CHARS))
                .collect(),
            tags: (0..hb_core::MAX_TAGS).map(|_| worst(hb_core::MAX_TAG_CHARS)).collect(),
            languages: (0..hb_core::MAX_LANGUAGES)
                .map(|_| worst(hb_core::MAX_LIST_ITEM_CHARS))
                .collect(),
            visibility: Visibility::Private,
            sorted: true,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        // Already at the ceilings, so this is a no-op — asserted, because if clamp_metadata ever
        // stopped covering a field this test would otherwise still pass on the clamped value.
        let before = col.clone();
        col.clamp_metadata();
        assert_eq!(
            serde_json::to_string(&col).unwrap().len(),
            serde_json::to_string(&before).unwrap().len(),
            "a field at its documented ceiling was clamped further — ceilings and clamp disagree"
        );

        let envelope = collection_to_listing_json(col).unwrap();
        // `truncate_listing` adds `truncated` + `total_items` on top of this before measuring.
        let markers = r#","truncated":true,"total_items":18446744073709551615"#.len();
        let overhead = envelope.len() + markers;

        assert!(
            overhead < LISTING_MAX_BYTES,
            "worst-case metadata ({overhead} B) does not fit the publish budget ({LISTING_MAX_BYTES} B) \
             — entries_budget would saturate to 0 and publish an empty teaser"
        );
        // Not merely "fits": the tree must keep effectively all of the budget.
        let left_for_entries = LISTING_MAX_BYTES - overhead;
        assert!(
            left_for_entries >= 30_000,
            "worst-case metadata leaves only {left_for_entries} B for the tree; \
             tighten the ceilings in hb-core or raise LISTING_MAX_BYTES"
        );
    }

    #[test]
    fn listing_json_maps_listing_to_entries_and_renders() {
        // collection_to_listing_json moves the tree to `entries`; the result must round-trip through
        // hb-net::render_listing (the format the browse side consumes).
        let col = Collection {
            slug: "criterion".into(),
            path_alias: "Criterion".into(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![DirectoryItem {
                name: "Ran (1985)".into(),
                item_type: ItemType::File,
                size: Some("12GB".into()),
                format: Some("MKV".into()),
                year: Some(1985),
                tags: vec![],
                note: None,
                children: vec![],
            }],
        };
        let json = collection_to_listing_json(col.clone()).unwrap();
        assert!(json.contains("\"entries\""), "tree must be under `entries`");
        assert!(!json.contains("\"listing\""), "`listing` key must be renamed away");
        let rendered = hb_net::render_listing(&[json]).unwrap();
        assert!(rendered.complete());
        assert_eq!(rendered.entries.len(), 1);
        assert_eq!(rendered.meta.get("slug").and_then(|v| v.as_str()), Some("criterion"));
        // M16 W3: the full-tree snapshot fingerprint rides into meta (the browse-side staleness gate,
        // W2, reads it) and equals the standalone `snapshot_fingerprint` of the same tree.
        assert_eq!(
            rendered.meta.get("snapshot_fingerprint").and_then(|v| v.as_str()),
            Some(hb_core::snapshot_fingerprint(&col.listing).0.as_str()),
            "the listing JSON must carry the full-tree fingerprint the teaser + big-relay family share",
        );
    }

    // ── audit #25 / QURATOR-123: the truncated teaser's digest + the carriers' teaser digest ────

    /// A collection big enough to truncate at the real `LISTING_MAX_BYTES` (40 KB).
    fn a_big_collection(slug: &str) -> Collection {
        let listing: Vec<DirectoryItem> = (0..1300)
            .map(|i| DirectoryItem {
                name: format!("title-{i:05}-padding-padding-padding-xx.mkv"),
                item_type: ItemType::File,
                size: None,
                format: None,
                year: None,
                tags: vec![],
                note: None,
                children: vec![],
            })
            .collect();
        Collection {
            item_count: listing.len() as u64,
            listing,
            ..a_video_collection(slug, Visibility::Public)
        }
    }

    #[test]
    fn stamp_teaser_fingerprint_puts_the_teaser_digest_beside_the_full_tree_one() {
        // The full-carrier stamp: the SAME digest `truncate_listing` re-derives for the teaser, read
        // back out of the artifact (not re-implemented here), inserted as `teaser_fingerprint`.
        let col = a_big_collection("vault");
        let full = collection_to_listing_json(col).unwrap();
        let stamped = stamp_teaser_fingerprint(&full).unwrap();
        let t = hb_net::truncate_listing(&full, LISTING_MAX_BYTES).unwrap();
        assert!(t.truncated, "the fixture must truncate");
        let teaser_fp = serde_json::from_str::<serde_json::Value>(&t.json)
            .unwrap()
            .get("snapshot_fingerprint")
            .and_then(|v| v.as_str())
            .expect("the teaser carries a digest")
            .to_string();
        assert_ne!(
            teaser_fp,
            serde_json::from_str::<serde_json::Value>(&full)
                .unwrap()
                .get("snapshot_fingerprint")
                .and_then(|v| v.as_str())
                .unwrap(),
            "the teaser digest must not be the full-tree digest (the #25 oracle)"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stamped)
                .unwrap()
                .get("teaser_fingerprint")
                .and_then(|v| v.as_str()),
            Some(teaser_fp.as_str()),
            "the carrier's `teaser_fingerprint` must equal what the teaser actually carries"
        );
    }

    #[test]
    fn stamp_teaser_fingerprint_leaves_an_untruncated_listing_byte_identical() {
        // The storm-guard-adjacent property: a collection that fits the budget whole must not be
        // perturbed (its teaser carries the full-tree digest — nothing is hidden).
        let col = a_video_collection("criterion", Visibility::Public);
        let json = collection_to_listing_json(col).unwrap();
        assert!(!hb_net::truncate_listing(&json, LISTING_MAX_BYTES).unwrap().truncated);
        assert_eq!(stamp_teaser_fingerprint(&json).unwrap(), json);
    }

    #[test]
    fn build_slug_manifest_gates_on_the_teaser_digest_not_the_full_tree_one() {
        // The envelope's signed digest must be what the truncated teaser carries, so the browse
        // side's `stale` comparison against the teaser it is showing still matches. This is the
        // hb-app half of the gate coherence fix — without it every manifest would read stale.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let browse_key: BrowseKey = [7u8; 32];
        let col = a_big_collection("vault");
        store.save_collection_draft(&col).unwrap();
        let env = build_slug_manifest("vault", &store, &identity, &browse_key).unwrap();
        // The teaser the same publish produces, derived through the production truncation.
        let full = collection_to_listing_json(col).unwrap();
        let t = hb_net::truncate_listing(&full, LISTING_MAX_BYTES).unwrap();
        assert!(t.truncated);
        let teaser_fp = serde_json::from_str::<serde_json::Value>(&t.json)
            .unwrap()
            .get("snapshot_fingerprint")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        let entries = serde_json::from_str::<serde_json::Value>(&full)
            .unwrap()
            .get("entries")
            .cloned()
            .unwrap();
        let tree: Vec<DirectoryItem> = serde_json::from_value(entries).unwrap();
        assert_ne!(
            env.snapshot_fingerprint,
            hb_core::snapshot_fingerprint(&tree).0,
            "the envelope must not carry the full-tree digest of a truncated collection"
        );
        assert_eq!(env.snapshot_fingerprint, teaser_fp, "the envelope gates on the teaser digest");
        assert!(env.matches_fingerprint(&teaser_fp), "the manifest is not stale against its teaser");
    }

    #[test]
    // QURATOR-205: publish and mint must agree on the digest of the SAME unchanged collection when a
    // big relay is configured and the listing truncates. Before the fix, `build_slug_manifest`
    // truncated the bare, unstamped `plaintext` while `publish_collection_inner` stamped
    // `big_relay_url` (then `teaser_fingerprint`) into the listing BEFORE truncating — the extra meta
    // bytes shift `truncate_listing`'s budget, so the two paths can keep different visible entries and
    // therefore mint different `snapshot_fingerprint` digests for a collection that never changed.
    //
    // Mutation-proof (P-10): in `build_slug_manifest`, replace
    //     `let plaintext = stamp_for_teaser(&plaintext, &big_relay_url)?;`
    // with
    //     `let plaintext = plaintext;` (i.e. drop the `stamp_for_teaser` call, reverting to the bare,
    //     unstamped `plaintext` the pre-fix code truncated) — this test must then fail on the final
    //     `assert_eq!` below, since the mint path would once again truncate different bytes than the
    //     publish-equivalent computation it's compared against.
    fn build_slug_manifest_matches_the_publish_digest_when_a_big_relay_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // Pin the relay set unroutable first (avoids the empty-relay-set → real DEFAULT_RELAYS
        // fallback), then re-save with the big relay ALSO set: `pin_unroutable_relays` replaces the
        // whole `Settings` struct via `..Default::default()`, so a later save must re-specify BOTH
        // fields together or it would silently wipe the sentinel relay back to empty.
        crate::store::tests::pin_unroutable_relays(&store);
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["wss://hoardbook-test-sentinel.invalid".into()],
                big_relay_url: "wss://big.example:7777".into(),
                ..Default::default()
            })
            .unwrap();
        let identity = Identity::generate();
        let browse_key: BrowseKey = [7u8; 32];
        let col = a_big_collection("vault");
        store.save_collection_draft(&col).unwrap();

        let env = build_slug_manifest("vault", &store, &identity, &browse_key).unwrap();

        // The publish-path-equivalent computation: the SAME stamping sequence
        // `publish_collection_inner` runs (via the shared `stamp_for_teaser`), on the SAME
        // collection, then truncated the SAME way, reading the digest back out of the artifact.
        let full = collection_to_listing_json(col).unwrap();
        let stamped = stamp_for_teaser(&full, "wss://big.example:7777").unwrap();
        let t = hb_net::truncate_listing(&stamped, LISTING_MAX_BYTES).unwrap();
        assert!(t.truncated, "the fixture must truncate");
        let publish_fp = serde_json::from_str::<serde_json::Value>(&t.json)
            .unwrap()
            .get("snapshot_fingerprint")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();

        assert_eq!(
            env.snapshot_fingerprint, publish_fp,
            "mint and publish must agree on the teaser digest for the same unchanged collection \
             once a big relay is configured"
        );
    }

    // ── M16 W4: manifest export (the `.hbmanifest` envelope) ─────────────────────
    // `build_slug_manifest` is the pure core the `export_manifest` command wraps around a file write;
    // the export→import round-trip over the wire is proven by the import-path tests in browse.rs.

    fn a_video_collection(slug: &str, visibility: Visibility) -> Collection {
        Collection {
            slug: slug.into(),
            path_alias: slug.into(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![DirectoryItem {
                name: "Ran (1985)".into(),
                item_type: ItemType::File,
                size: Some("12GB".into()),
                format: Some("MKV".into()),
                year: Some(1985),
                tags: vec![],
                note: None,
                children: vec![],
            }],
        }
    }

    #[test]
    fn build_slug_manifest_roundtrips_and_matches_the_teaser_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let browse_key: BrowseKey = [7u8; 32];
        let col = a_video_collection("criterion", Visibility::Public);
        store.save_collection_draft(&col).unwrap();

        let env = build_slug_manifest("criterion", &store, &identity, &browse_key).unwrap();
        // Verifies under the browsed author + opens under the browse-key into the listing parts, which
        // restitch into the complete original tree (a small collection is a single part).
        let parts = env.open(&browse_key, &identity.public_key()).unwrap();
        let rendered = hb_net::render_listing(&parts).unwrap();
        assert!(rendered.complete());
        assert_eq!(rendered.meta.get("slug").and_then(|v| v.as_str()), Some("criterion"));
        assert_eq!(rendered.entries.len(), 1, "the one file round-trips through split → open → render");
        // The envelope's fingerprint equals the teaser's (the browse-side staleness gate) and the slug
        // is bound into the signature (an envelope for another collection can't masquerade as this one).
        let fp = hb_core::snapshot_fingerprint(&col.listing).0;
        assert_eq!(env.snapshot_fingerprint, fp);
        assert!(env.matches_fingerprint(&fp));
        assert_eq!(env.slug, "criterion");
    }

    #[test]
    fn build_slug_manifest_refuses_a_private_collection() {
        // A Private collection is sealed per recipient and never truncates — there is no browse-key
        // manifest to export; the attempt is a reasoned error, not a wrong-crypto artifact.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let col = a_video_collection("secret", Visibility::Private);
        store.save_collection_draft(&col).unwrap();
        let err = build_slug_manifest("secret", &store, &identity, &[7u8; 32]).unwrap_err();
        assert!(err.contains("Private"), "got: {err}");
    }

    #[test]
    fn build_slug_manifest_refuses_an_unknown_slug() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        assert!(build_slug_manifest("nope", &store, &identity, &[7u8; 32]).is_err());
    }

    #[test]
    fn build_slug_manifest_chunks_a_large_listing_into_multiple_parts() {
        // A listing far over one NIP-44 event now SUCCEEDS (the W4 residual): it is split into parts
        // stored inline in the envelope, so the file carrier is bounded by part count, not one event.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let listing: Vec<DirectoryItem> = (0..3000)
            .map(|i| DirectoryItem {
                name: format!("file-{i:06}-with-a-reasonably-long-name.mkv"),
                item_type: ItemType::File,
                size: Some("12GB".into()),
                format: Some("MKV".into()),
                year: Some(1985),
                tags: vec![],
                note: None,
                children: vec![],
            })
            .collect();
        let col = Collection {
            slug: "huge".into(),
            path_alias: "huge".into(),
            description: None,
            item_count: listing.len() as u64,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing,
        };
        store.save_collection_draft(&col).unwrap();
        let env = build_slug_manifest("huge", &store, &identity, &[7u8; 32]).unwrap();
        assert!(env.ciphertexts.len() > 1, "a large listing chunks into multiple parts");
    }

    #[test]
    fn build_slug_manifest_accepts_an_index_over_40k_but_under_the_nip44_cap() {
        // The manifest/iroh carrier publishes nothing per-part to a relay, so its split budget is
        // NIP-44's 65_408-byte plaintext cap, not the 40_000-byte relay anti-ban budget. This pins
        // the fix: a collection whose listing INDEX (metadata + one `sha256`/`part_d` row per part)
        // lands between 40,001 and 65,408 bytes must still export — under the old 40 KB budget it
        // hard-errored in `split_listing`'s `index_json.len() > max_bytes` check.
        //
        // The long slug inflates each index descriptor row (`slug#part{i}` + the sha256) to ~250 B
        // vs ~100 B for a short slug, so the 40 KB+ index is reachable with ~200 parts instead of
        // ~500 — far less data to split and hash. A ~4 KB note per leaf keeps the entry count low.
        let slug = "a".repeat(160);
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let note = "x".repeat(4000);
        let listing: Vec<DirectoryItem> = (0..3000)
            .map(|i| DirectoryItem {
                name: format!("file-{i:05}.mkv"),
                item_type: ItemType::File,
                size: Some("12GB".into()),
                format: Some("MKV".into()),
                year: Some(1985),
                tags: vec![],
                note: Some(note.clone()),
                children: vec![],
            })
            .collect();
        let col = Collection {
            slug: slug.clone(),
            path_alias: "huge".into(),
            description: None,
            item_count: listing.len() as u64,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing,
        };
        store.save_collection_draft(&col).unwrap();
        let env = build_slug_manifest(&slug, &store, &identity, &[7u8; 32]).unwrap();
        // The index is the first decrypted part. Pin its size directly so this test can't silently
        // drift out of the window it exists to prove (an index that fit 40 KB would green even
        // against the old budget).
        let parts = env.open(&[7u8; 32], &identity.public_key()).unwrap();
        let index_len = parts[0].len();
        assert!(
            index_len > LISTING_MAX_BYTES && index_len <= MANIFEST_SPLIT_MAX_BYTES,
            "index is {index_len} bytes, expected in ({LISTING_MAX_BYTES}, {MANIFEST_SPLIT_MAX_BYTES}]"
        );
        assert!(env.ciphertexts.len() > 1, "a large listing chunks into multiple parts");
    }

    // ── M16 W3: big-relay classifier (Layer 3 routing) ───────────────────────────
    // The routing decision is a pure function; the actual big-relay wire (family → big relay only,
    // no leak to public) is proven by hb-it Suite BIG1/BIG2 against a live strfry, same split as the
    // publish-path tests above.

    #[test]
    fn enforce_item_cap_is_inclusive_at_exactly_the_ceiling() {
        // Mirrors transport_payload.rs's the_boundary_is_inclusive_at_exactly_the_ceiling — the
        // cliff is documented, so an off-by-one here is a real regression, not a rounding nit.
        assert!(
            enforce_item_cap(MAX_COLLECTION_ITEMS).is_ok(),
            "exactly the cap must be accepted"
        );
        let err = enforce_item_cap(MAX_COLLECTION_ITEMS + 1)
            .expect_err("one item over the cap must be refused");
        assert!(err.contains(&(MAX_COLLECTION_ITEMS + 1).to_string()), "error names the count");
        assert!(err.contains(&MAX_COLLECTION_ITEMS.to_string()), "error names the cap");
    }

    #[test]
    fn big_relay_target_routes_only_truncated_with_a_configured_relay() {
        // Truncated (too large for one event) + a configured big relay → the full family also goes there.
        assert_eq!(
            big_relay_target(true, "ws://big.example:7777"),
            Some("ws://big.example:7777"),
        );
        // Fit whole (small collection) → NO big-relay write, even with a big relay set. This is the
        // golden guard against failure mode #2 (the classifier must not regress small collections):
        // a non-truncated publish never enters the big-relay branch, so its teaser bytes are unchanged.
        assert_eq!(big_relay_target(false, "ws://big.example:7777"), None);
        // Truncated but no big relay configured → feature off, keep only the teaser.
        assert_eq!(big_relay_target(true, ""), None);
        // A whitespace-only setting is "unset" (guards a stray space saved into settings.json).
        assert_eq!(big_relay_target(true, "   "), None);
        // Both off.
        assert_eq!(big_relay_target(false, ""), None);
    }

    #[test]
    fn big_relay_target_trims_the_configured_url() {
        // The returned target is trimmed so the dedicated big-relay client gets a clean URL.
        assert_eq!(
            big_relay_target(true, "  ws://big.example:7777  "),
            Some("ws://big.example:7777"),
        );
    }

    #[test]
    fn stamp_big_relay_url_rides_into_meta_and_is_a_noop_when_blank() {
        let col = Collection {
            slug: "bigvault".into(),
            path_alias: "Big Vault".into(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![DirectoryItem {
                name: "a.mkv".into(),
                item_type: ItemType::File,
                size: None,
                format: None,
                year: None,
                tags: vec![],
                note: None,
                children: vec![],
            }],
        };
        let base = collection_to_listing_json(col).unwrap();

        // Feature off (blank / whitespace) ⇒ byte-identical: the small-collection teaser is unchanged.
        assert_eq!(stamp_big_relay_url(&base, "").unwrap(), base);
        assert_eq!(stamp_big_relay_url(&base, "   ").unwrap(), base);

        // Set ⇒ the (trimmed) URL rides in the top-level meta and survives the render path the browse
        // side reads (option b: the browser discovers the peer's big relay from this teaser meta).
        let stamped = stamp_big_relay_url(&base, "  ws://big.example:7777  ").unwrap();
        let rendered = hb_net::render_listing(&[stamped]).unwrap();
        assert_eq!(
            rendered.meta.get("big_relay_url").and_then(|v| v.as_str()),
            Some("ws://big.example:7777"),
            "the owner's big relay must ride into the teaser meta for browse-side discovery",
        );
        // ...without disturbing the W3 (1/n) fingerprint already stamped there.
        assert!(
            rendered.meta.get("snapshot_fingerprint").is_some(),
            "stamping the big relay must not drop the snapshot fingerprint",
        );
    }

    #[test]
    fn prepare_listing_rejects_invalid_slug() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let err = prepare_listing("../evil", &store).unwrap_err();
        assert!(err.contains("Invalid collection slug"), "got: {err}");
    }

    /// QURATOR-249 (publish half): a draft slugged `profile` — which can only exist by restore or
    /// by predating the scan-time guard — is refused at `prepare_listing`, the single validation
    /// every publish passes through, so no path can write a collection marker over the profile
    /// teaser marker. The draft is otherwise publishable (charset-valid slug, content types set)
    /// so this pins the reserved-name guard specifically.
    ///
    /// Mutation to redden: in `prepare_listing`, change the reserved-name guard
    /// `if is_reserved_marker_slug(safe_slug) {` to `if false {` (or delete the `if` block) —
    /// the prepare then succeeds and the `unwrap_err` reds.
    #[test]
    fn prepare_listing_refuses_a_draft_in_the_reserved_marker_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "profile", vec!["Films".into()]);
        let err = prepare_listing("profile", &store).unwrap_err();
        assert!(err.contains("'profile' is a reserved name"), "got: {err}");
    }

    #[test]
    fn prepare_listing_rejects_empty_content_types() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "no-types", vec![]);
        let err = prepare_listing("no-types", &store).unwrap_err();
        assert!(err.contains("content type"), "got: {err}");
    }

    #[test]
    fn get_collections_published_flag_tracks_marker() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "films", vec!["video".into()]);
        assert!(!store.is_published("films"));
        store.save_published("films", "{}").unwrap();
        assert!(store.is_published("films"), "marker presence => published");
    }

    // ── CWE-367: the revocation-generation guard at the marker save ────────────────────
    // `publish_collection_inner` writes to the relay BEFORE it saves the published marker, so an
    // Unpublish issued mid-publish can be silently undone: the relay write re-creates the marker and
    // resurrects a revoked catalog for anyone holding the old share code. `save_collection_marker_guarded`
    // is the exact marker-save tail both `publish_collection_inner` and `publish_private_collection_inner`
    // run at their save sites (extracted so the test ends where production ends, P-6). It is driven
    // directly here because the full `publish_collection_inner` needs a live relay (`net::client` +
    // `publish_listing_capped`); the guard itself is pure store I/O. This test is the
    // production-boundary kind: it calls the SAME function production calls, so reverting production
    // to unguarded `save_published` reds it.

    #[test]
    fn publish_guard_refuses_to_resurrect_marker_after_mid_write_unpublish() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let marker = r#"{"parts":1}"#;

        // A published collection, as it stands when the rescan republish begins.
        store.save_published("films", marker).unwrap();
        let gen_at_start = store.published_generation("films");

        // Mid-write Unpublish: `unpublish_collection_inner` ends in `delete_published`, which removes
        // the marker AND bumps the revocation generation.
        store.delete_published("films").unwrap();
        assert!(!store.is_published("films"), "premise: the unpublish removed the marker");

        // The marker-save tail of `publish_collection_inner` — the exact function production runs at
        // its save site — re-run after the relay writes. It must detect the bump and refuse to
        // re-create the marker.
        let outcome = save_collection_marker_guarded(&store, "films", marker, gen_at_start);
        assert!(
            matches!(outcome, Ok(PublishedSave::Revoked)),
            "a mid-write unpublish must surface Revoked, not silently re-save"
        );
        assert!(!store.is_published("films"), "the revoked marker must NOT be resurrected");
    }

    #[test]
    fn publish_guard_saves_when_generation_is_stable() {
        // A missing generation file reads as 0 (fresh install / first publish), and an un-bumped
        // generation must save — the guard must not over-reject and lose a legitimate publish.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let marker = r#"{"parts":1}"#;
        let gen_at_start = store.published_generation("films");
        assert_eq!(gen_at_start, 0, "a missing generation file reads as 0");
        assert!(matches!(
            save_collection_marker_guarded(&store, "films", marker, gen_at_start),
            Ok(PublishedSave::Saved)
        ));
        assert!(store.is_published("films"), "an un-bumped generation must save the marker");
    }

    // ── QURATOR-251: the Revoked branch retracts what the publish already wrote ─────

    /// INV-8 (no over-delete) at the Event level: the retraction selector must keep exactly the
    /// revoked slug's `d`-tag family (the index + `slug#part{i}` split parts) and nothing else —
    /// a revocation of "films" landing mid-publish must never sweep another collection's events
    /// ("music") or a prefix-sibling ("films-extra") into its NIP-09 deletions. Drives the SAME
    /// selector both loops of `retract_public_listing` (shared pool + big relay) run — and
    /// therefore the QURATOR-251 revoked-during-publish retraction through it (P-6: the test ends
    /// where production ends). The events are real signed kind-31111 listing events, so the
    /// `d`-tag shape is production-true.
    ///
    /// Mutation to redden: in `retraction_targets`' filter arm (the
    /// `is_some_and(|d| listing_dtag_belongs_to_slug(d, slug))` predicate, line 1199), replace the
    /// predicate with `true` — "music" and "films-extra" then survive selection and the
    /// `assert_eq!` reds.
    #[test]
    fn retraction_targets_selects_exactly_the_revoked_slugs_family() {
        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let listing =
            |d: &str| hb_core::event::build_listing_event(&identity, d, &bk, "{}").unwrap();
        let events = vec![
            listing("films"),
            listing("films#part0"),
            listing("films#part12"),
            // Everything below must NEVER be selected for "films".
            listing("films-extra"),
            listing("music"),
            listing("films#partition"),
        ];
        let selected: Vec<&str> = retraction_targets(&events, "films")
            .filter_map(|ev| ev.tags.identifier())
            .collect();
        assert_eq!(selected, vec!["films", "films#part0", "films#part12"]);
    }

    /// QURATOR-251 — the PUBLIC path's Revoked tail, driven at its extracted seam
    /// (`save_public_marker_or_retract`, the exact function `publish_collection_inner` runs at its
    /// save site; the full publish needs a live relay — the documented limit above this block).
    /// What a unit test can prove is the tail's LOCAL contract: on a mid-write revocation the
    /// marker is never saved, the retraction is attempted through an unroutable relay (fails fast,
    /// touches no real relay), and the race is reported as the exact revocation error. Whether a
    /// retraction was actually ISSUED — and honoured — is observable only against a live relay:
    /// that is the WAN-E2E escalation row (a publish revoked mid-flight leaves no readable listing
    /// on the relay set), owner-gated, because `hb-it` cannot link `hb-app` (§5 step 2, verified:
    /// hb-it's Cargo.toml declares exactly hb-core + hb-net).
    ///
    /// Mutation to redden: in `save_public_marker_or_retract`'s Revoked arm (line 756), change the
    /// `return Err(revoked_during_publish(slug));` to `return Ok(());` — the `unwrap_err()` reds.
    /// (Removing the `retract_public_listing` call does NOT red this test — that half is the WAN
    /// row's to prove, which is said here plainly rather than pretended into the assert.)
    #[tokio::test]
    async fn public_revoked_tail_reports_the_race_and_never_saves_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // Unroutable local relay — the same posture as the unpublish tests below: the best-effort
        // retraction attempt fails fast without ever touching a real relay.
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["ws://127.0.0.1:1".into()],
                ..Default::default()
            })
            .unwrap();
        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();

        // The mid-write revocation: `delete_published` removes the marker and bumps the generation
        // past the `gen_at_start` of 0 the in-flight "publish" still holds.
        store.delete_published("films").unwrap();

        let err = save_public_marker_or_retract(
            "films",
            &store,
            &identity,
            &bk,
            &relay,
            r#"{"parts":1}"#,
            0,
        )
        .await
        .unwrap_err();
        assert_eq!(err, revoked_during_publish("films"));
        assert!(
            !store.is_published("films"),
            "the revoked tail must not save the marker"
        );
    }

    // ── M13 W5 item 1: unpublish_collection ───────────────────────────────────────

    /// Pins the deletion-targeting design choice documented on `unpublish_collection_inner`: a
    /// listing family is matched by `d`-tag (the index itself, or a `slug#part{i}` split part), not
    /// by a persisted event-id list. A different slug that merely shares a prefix must never match.
    #[test]
    fn unpublish_matches_listing_dtags_by_slug_and_part_prefix() {
        assert!(listing_dtag_belongs_to_slug("films", "films"), "the index d-tag matches");
        assert!(listing_dtag_belongs_to_slug("films#part0", "films"), "a split part matches");
        assert!(listing_dtag_belongs_to_slug("films#part12", "films"));
        assert!(!listing_dtag_belongs_to_slug("films-extra", "films"), "a different slug must not match");
        assert!(!listing_dtag_belongs_to_slug("other", "films"));
        // Chorus M13 finding #3: only a digits tail after `#part` is a split part — a raw prefix
        // match would sweep any future `slug#part…`-rooted sidecar d-tag into unpublish.
        assert!(!listing_dtag_belongs_to_slug("films#partition", "films"));
        assert!(!listing_dtag_belongs_to_slug("films#part", "films"), "no bare #part");
        assert!(!listing_dtag_belongs_to_slug("films#part1x", "films"));
    }

    /// A published **Public** collection: unpublishing must drop the local marker, which alone stops
    /// the watch's auto-republish (`evaluate_rescan` skips once `is_published` is false — no
    /// watch.rs change). The relay is configured to an unroutable local address so the best-effort
    /// NIP-09 deletion attempt fails fast without touching a real relay — the wire side (whether a
    /// compliant relay actually honours the deletion) is `hb-it` Suite BROWSE's job, same as
    /// `publish_collection_inner` today; this test asserts the local effects only.
    #[tokio::test]
    async fn unpublish_public_collection_drops_marker_and_stops_republish() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["ws://127.0.0.1:1".into()],
                ..Default::default()
            })
            .unwrap();
        make_collection_draft(&store, "films", vec!["video".into()]);
        store.save_published("films", r#"{"parts":1}"#).unwrap();
        assert!(store.is_published("films"));

        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        unpublish_collection_inner("films", &store, &identity, &bk, &relay).await.unwrap();

        assert!(!store.is_published("films"), "the published marker must be gone");
        match crate::watch::evaluate_rescan("films", &store).unwrap() {
            crate::watch::RescanDecision::Skipped(reason) => {
                assert_eq!(reason, "not published", "the watch's gate gains nothing from watch.rs")
            }
            other => panic!("expected Skipped(\"not published\"), got {other:?}"),
        }
    }

    /// A published **Private** collection: unpublishing must drop the local marker without ever
    /// reaching `net::client` (gift-wrapped events are authored by ephemeral keys this identity
    /// cannot NIP-09). No relay is configured — if the private path attempted a network deletion it
    /// would fall back to the real `DEFAULT_RELAYS` and this test would attempt a live connection;
    /// completing instantly with no such attempt is exactly what proves the "no network deletion"
    /// contract.
    #[tokio::test]
    async fn unpublish_private_collection_drops_marker_without_network_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "vault", vec!["forbidden".into()]);
        let mut col = store.load_collection_draft("vault").unwrap().unwrap();
        col.visibility = Visibility::Private;
        store.save_collection_draft(&col).unwrap();
        store.save_published("vault", r#"{"private":true,"recipients":1}"#).unwrap();

        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        unpublish_collection_inner("vault", &store, &identity, &bk, &relay).await.unwrap();

        assert!(!store.is_published("vault"), "the published marker must be gone");
    }

    // ── QURATOR-200 (F10): the retraction tier comes from the published MARKER, not the draft ──

    /// The tier classifier on the exact store states of the defect and its mirror. The public and
    /// private marker shapes are the ones `publish_collection_inner` /
    /// `publish_private_collection_inner` write today; `{}` and unparseable bytes are the legacy
    /// indeterminate shapes. **Why the wire behaviour itself is not asserted here:** with relays
    /// configured unroutable (the only offline-safe configuration) the public retraction is
    /// locally SILENT — every step is best-effort against a dead pool — so the branch choice is
    /// pinned at the decision (`marker_tier`) and the read-wiring (the two tests below); the
    /// emitted NIP-09/tombstone is `hb-it`'s to observe on a live relay, same split as the
    /// existing unpublish tests above.
    #[test]
    fn marker_tier_reads_the_published_tier_never_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());

        // F10 state: published PUBLIC (marker), draft since flipped PRIVATE.
        make_collection_draft(&store, "films", vec!["video".into()]);
        let mut col = store.load_collection_draft("films").unwrap().unwrap();
        col.visibility = Visibility::Private;
        store.save_collection_draft(&col).unwrap();
        store.save_published("films", r#"{"parts":1,"truncated":false}"#).unwrap();
        assert_eq!(
            marker_tier(&store, "films").unwrap(),
            MarkerTier::Public,
            "F10: the marker's recorded public tier wins over the flipped draft"
        );

        // Mirror (criterion 2): published PRIVATE (marker), draft since flipped PUBLIC — the
        // marker is authoritative in BOTH directions, so the fix is not merely an inversion.
        make_collection_draft(&store, "vault", vec!["forbidden".into()]);
        store.save_published("vault", r#"{"private":true,"recipients":1}"#).unwrap();
        assert_eq!(
            marker_tier(&store, "vault").unwrap(),
            MarkerTier::Private,
            "a genuinely-private marker keeps the local-only path even though the draft says Public"
        );

        // Indeterminate legacy markers fail toward Public (retraction attempted), never Private.
        store.save_published("legacy", "{}").unwrap();
        assert_eq!(marker_tier(&store, "legacy").unwrap(), MarkerTier::Public);
        store.save_published("ancient", "not-json-at-all").unwrap();
        assert_eq!(marker_tier(&store, "ancient").unwrap(), MarkerTier::Public);

        // Never published.
        assert_eq!(marker_tier(&store, "ghost").unwrap(), MarkerTier::Unpublished);
    }
    // MUTATION (P-10): in `marker_tier` (collection.rs, the `Some(m)` arm), swap the classification
    // line `if private { MarkerTier::Private } else { MarkerTier::Public }` to
    // `if !private { MarkerTier::Private } else { MarkerTier::Public }` — every assertion above
    // reds (Public↔Private swap; `Unpublished` is the untouched `None` arm).

    /// Wiring of the read (F10 proper): `unpublish_collection_inner` must CONSULT THE MARKER to
    /// decide the tier — the defect was exactly that it read the draft. Probe: the marker FILE is
    /// replaced by a directory, so a marker read fails with `load_published`'s
    /// "loading published event" context, while the draft-driven code never reads the marker on
    /// this path and instead fails later at `delete_published`'s `remove_file` with a bare io
    /// message. **No relay is configured and none is needed:** the fixed path errors at the marker
    /// read (before any relay I/O), and the draft-driven path sees a Private draft and takes the
    /// local-only branch — neither can reach the network, which is what makes this test safe.
    #[tokio::test]
    async fn unpublish_decides_its_tier_from_the_marker_not_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "films", vec!["video".into()]);
        let mut col = store.load_collection_draft("films").unwrap().unwrap();
        col.visibility = Visibility::Private;
        store.save_collection_draft(&col).unwrap();
        store.save_published("films", r#"{"parts":1}"#).unwrap();
        // Make the recorded-public marker UNREADABLE, so WHICH read failed is observable.
        let marker_path = store.published_path("films");
        assert!(marker_path.is_file(), "fixture premise: the marker write created the file");
        std::fs::remove_file(&marker_path).unwrap();
        std::fs::create_dir(&marker_path).unwrap();

        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        let err = unpublish_collection_inner("films", &store, &identity, &bk, &relay)
            .await
            .unwrap_err();
        assert!(
            err.contains("loading published event"),
            "the tier decision must fail on the MARKER read (QURATOR-200); got: {err}"
        );
    }
    // MUTATION (P-10): in `unpublish_collection_inner` (collection.rs, the tier-decision lines at
    // the top of the fn, right after the `safe_slug` let), replace `let tier = marker_tier(store,
    // safe_slug)?;` with a draft-derived tier:
    //   let tier = if store.load_collection_draft(safe_slug).map_err(cmd_err)?
    //       .map(|c| c.visibility) == Some(Visibility::Private) { MarkerTier::Private }
    //   else { MarkerTier::Public };
    // (compiles; recreates F10). The error then comes from the later `delete_published`
    // `remove_file`, so the "loading published event" assertion reds. This test is also RED on the
    // pre-fix tree by construction — it is the red half of this fix's own red-green.

    /// Wiring of the republish flip (F10's sibling): `publish_collection_inner`'s Private branch
    /// must consult the RECORDED tier before the audience gate, so a stranded public listing is
    /// retracted at the flip. Same unreadable-marker probe, and again **no relay is configured and
    /// none is reached**: the fixed path errors at the marker read, and the retract-free path
    /// errors at `private_recipients` (empty audience) — both before any relay I/O.
    #[tokio::test]
    async fn republish_as_private_consults_the_recorded_public_tier() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "flipzone", vec!["video".into()]);
        let mut col = store.load_collection_draft("flipzone").unwrap().unwrap();
        col.visibility = Visibility::Private;
        store.save_collection_draft(&col).unwrap();
        store.save_published("flipzone", r#"{"parts":1}"#).unwrap();
        let marker_path = store.published_path("flipzone");
        assert!(marker_path.is_file(), "fixture premise: the marker write created the file");
        std::fs::remove_file(&marker_path).unwrap();
        std::fs::create_dir(&marker_path).unwrap();

        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        let err = publish_collection_inner("flipzone", &store, &identity, &bk, &relay, 0)
            .await
            .unwrap_err();
        assert!(
            err.contains("loading published event"),
            "the Private branch must consult the recorded tier BEFORE the audience gate; got: {err}"
        );
        assert!(
            !err.contains("haven't chosen anyone"),
            "reaching the audience gate means the marker was never consulted; got: {err}"
        );
    }
    // MUTATION (P-10): in `publish_collection_inner` (collection.rs, the Private-visibility
    // branch), delete the `if marker_tier(store, slug)? == MarkerTier::Public {
    // retract_public_listing(...).await?; }` consult. The error then comes from
    // `private_recipients` ("haven't chosen anyone") and both assertions above red.

    /// devtest #11: deleting a *published* collection must drop its content_types from the published
    /// profile teaser's union. The delete path routes a published collection through
    /// `unpublish_collection_inner` (which recomputes + persists the teaser) before removing the local
    /// draft. Unroutable relay so the best-effort NIP-09 attempt fails fast — local effects only.
    #[tokio::test]
    async fn deleting_a_published_collection_drops_its_content_types_from_the_teaser() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["ws://127.0.0.1:1".into()],
                ..Default::default()
            })
            .unwrap();

        // A published profile teaser + two published public collections.
        let profile = hb_core::types::Profile {
            display_name: "Me".into(),
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
            picture: None, hide_in_rosters: false,
            updated: chrono::Utc::now(),
        };
        store.save_profile_draft(&profile).unwrap();
        store.save_published("profile", "{}").unwrap();
        make_collection_draft(&store, "films", vec!["video".into()]);
        store.save_published("films", r#"{"parts":1}"#).unwrap();
        make_collection_draft(&store, "music", vec!["audio".into()]);
        store.save_published("music", r#"{"parts":1}"#).unwrap();
        assert_eq!(compute_content_types(&store), vec!["audio".to_string(), "video".to_string()]);

        // Delete "films" exactly as `delete_collection` does for a published collection.
        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        unpublish_collection_inner("films", &store, &identity, &bk, &relay).await.unwrap();
        store.delete_collection("films").unwrap();

        // The union — and the persisted teaser draft — no longer carry the deleted collection's type.
        assert_eq!(compute_content_types(&store), vec!["audio".to_string()]);
        assert_eq!(
            store.load_profile_draft().unwrap().unwrap().content_types,
            vec!["audio".to_string()],
            "the published teaser draft dropped the deleted collection's content_type"
        );
    }

    // ── M10: visibility + private-recipient gathering ────────────────────────────────

    #[test]
    fn collection_draft_defaults_public_and_flips_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        make_collection_draft(&store, "vault", vec!["video".into()]);
        assert_eq!(
            store.load_collection_draft("vault").unwrap().unwrap().visibility,
            Visibility::Public,
            "a fresh draft is Public"
        );
        // Mirror update_collection_visibility's core (load → set → save).
        let mut col = store.load_collection_draft("vault").unwrap().unwrap();
        col.visibility = Visibility::Private;
        store.save_collection_draft(&col).unwrap();
        assert_eq!(
            store.load_collection_draft("vault").unwrap().unwrap().visibility,
            Visibility::Private,
            "visibility change persists through the store"
        );
    }

    #[test]
    fn private_recipients_requires_an_explicit_audience() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // No audience file → Err (a Private collection with no audience is a mistake).
        assert!(private_recipients(&store).is_err());
        // A group with members is still not an audience — the audience is a separate, explicit list.
        let a = hb_core::Identity::generate().npub();
        store
            .save_groups(&[crate::store::Group {
                name: "friends".into(),
                pubkeys: vec![a],
                modified_at: chrono::Utc::now(),
                color: None,
            }])
            .unwrap();
        assert!(private_recipients(&store).is_err(), "a group is not a recipient set (M21 W5)");
    }

    #[test]
    fn private_recipients_collects_audience_deduped_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let a = hb_core::Identity::generate();
        let b = hb_core::Identity::generate();
        // The audience is an explicit list of npubs (M21 W5) — no group affiliation involved.
        // a, b, and a legacy non-Nostr id that must be skipped (not crash); a duplicated → collapsed.
        store
            .save_private_audience(&[a.npub(), b.npub(), "hb1_legacy_junk".into(), a.npub()])
            .unwrap();
        let recips = private_recipients(&store).unwrap();
        assert_eq!(recips.len(), 2, "two distinct valid npubs (junk skipped, dup collapsed)");
        assert!(recips.contains(&a.public_key()) && recips.contains(&b.public_key()));
    }

    /// M21 W5 key regression: adding a contact to a group (the data mutation `groups_assign` and
    /// `contact_update_groups` perform) does NOT change `private_recipients`. Before W5 this would
    /// have failed — the recipients were a live query over trusted groups, so filing a contact
    /// silently enrolled them as a Private recipient.
    #[test]
    fn private_recipients_unaffected_by_group_membership() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let a = hb_core::Identity::generate();
        let b = hb_core::Identity::generate();
        // Seed the audience with `a` only.
        store.save_private_audience(&[a.npub()]).unwrap();
        // Add `b` to a group (the `groups_assign` mutation).
        let now = chrono::Utc::now();
        store
            .save_groups(&[crate::store::Group {
                name: "friends".into(),
                pubkeys: vec![b.npub()],
                modified_at: now,
                color: None,
            }])
            .unwrap();
        // Audience is still just `a` — `b` is in a group, not the recipient set.
        let recips = private_recipients(&store).unwrap();
        assert_eq!(recips.len(), 1, "group membership must not add recipients (M21 W5)");
        assert!(recips.contains(&a.public_key()));
        assert!(!recips.contains(&b.public_key()), "b is in a group but NOT in the audience");
    }

    #[test]
    fn no_sha256_in_draft() {
        let item = DirectoryItem {
            name: "film.mkv".into(),
            item_type: ItemType::File,
            size: Some("14.2 GB".into()),
            format: Some("MKV".into()),
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(!json.contains("sha256"), "DirectoryItem must not expose sha256: {json}");
    }

    // ── Audit #31 (QURATOR-123, CWE-636): build_glob_set fails closed ──────

    #[test]
    fn build_glob_set_rejects_unclosed_character_class() {
        let err = build_glob_set(&["[".to_string()]).unwrap_err();
        assert!(err.contains("invalid exclude pattern"), "names the failure: {err}");
        assert!(err.contains("["), "names the offending pattern: {err}");
    }

    #[test]
    fn build_glob_set_reports_every_rejected_pattern() {
        let err = build_glob_set(&["[abc".to_string(), "{a,b".to_string()]).unwrap_err();
        assert!(err.contains("[abc"), "first rejected pattern named: {err}");
        assert!(err.contains("{a,b"), "second rejected pattern also named: {err}");
    }

    #[test]
    fn build_glob_set_aborts_when_any_pattern_invalid() {
        // One typo among valid patterns must not silently drop the valid ones too.
        let err = build_glob_set(&["*.nfo".to_string(), "[".to_string()]).unwrap_err();
        assert!(err.contains("invalid exclude pattern"), "scan must abort: {err}");
    }

    // ── Audit #18 (QURATOR-124, CWE-74): markdown/text export neutralizes filenames ──

    #[test]
    fn render_markdown_neutralizes_image_markup() {
        let item = DirectoryItem {
            name: "![cov](https://attacker.example/pixel.png)".into(),
            item_type: ItemType::File,
            size: None,
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        let md = render_markdown(&[item], 0);
        assert!(!md.contains("![cov]("), "image markup must not survive: {md}");
        assert!(!md.contains("https://attacker.example"), "link target must not be live: {md}");
        assert!(md.contains("\\!\\[cov\\]\\("), "name must be backslash-escaped: {md}");
    }

    #[test]
    fn render_markdown_round_trips_legitimate_brackets() {
        let item = DirectoryItem {
            name: "[2024] Album (FLAC)".into(),
            item_type: ItemType::File,
            size: None,
            format: Some("FLAC".into()),
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        let md = render_markdown(&[item], 0);
        assert!(md.contains("\\[2024\\] Album \\(FLAC\\)"), "escaped, not markup: {md}");
        assert!(!md.contains("[2024] Album (FLAC)"), "raw link-shaped text must be escaped: {md}");
    }

    #[test]
    fn render_markdown_strips_newlines_and_size_stays_readable() {
        let item = DirectoryItem {
            name: "leak\n[forged](https://attacker.example/x)".into(),
            item_type: ItemType::File,
            size: Some("14.2 GB".into()),
            format: Some("MKV".into()),
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        let md = render_markdown(&[item], 0);
        assert!(!md.contains('\n'), "filename newline must not forge a row: {md:?}");
        assert!(md.contains("`MKV, 14.2 GB`"), "size/format must render un-mangled: {md}");
    }

    #[test]
    fn render_text_strips_newlines_from_name() {
        let item = DirectoryItem {
            name: "real.txt\n  forged row".into(),
            item_type: ItemType::File,
            size: None,
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        let txt = render_text(&[item], 0);
        assert!(!txt.contains('\n'), "filename newline must not forge a row: {txt:?}");
    }
}

// Guards pinned here are the ones reachable through the PURE cores: `build_slug_manifest` (what
// `export_manifest` wraps) and `unpublish_collection_inner` (what `unpublish_collection` wraps).
// Both take real types directly, so each guard is exercised against the actual production fn.
//
// The five commands' OWN inline guards (the "No identity loaded" checks in `publish_collection`/
// `export_manifest`/`unpublish_collection`, and the slug/draft-not-found checks in
// `update_collection_visibility`/`export_collection`) are NOT covered here and remain OWED.
// They ARE reachable: `tauri`'s `test` feature is enabled in hb-app's dev-dependencies and
// `mock_app`/`guard_app` are used throughout this crate — see `collection_command_guards_a`
// above, and identity.rs/fulfil.rs. An earlier draft of this module asserted the opposite; that
// was written against a stale worktree predating QURATOR-161 and is not true of this branch.
#[cfg(test)]
mod collection_command_guards_b {
    use super::*;

    // ── export_manifest → build_slug_manifest (line 830) ───────────────────────────────────

    /// Pins `build_slug_manifest`'s OWN copy of the slug-format guard (line 836:
    /// `is_valid_slug(slug).then_some(slug).ok_or("Invalid collection slug")?`). This is a
    /// distinct call site from `prepare_listing`'s identical-looking check (already pinned by
    /// `prepare_listing_rejects_invalid_slug` above) — a future edit that drops the check from
    /// `build_slug_manifest` specifically, while leaving `prepare_listing`'s intact, would not be
    /// caught by that other test.
    #[test]
    fn build_slug_manifest_refuses_an_invalid_slug() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let err = build_slug_manifest("../evil", &store, &identity, &[7u8; 32]).unwrap_err();
        assert!(err.contains("Invalid collection slug"), "got: {err}");
    }

    /// Pins `build_slug_manifest`'s empty-content-types refusal (lines 846-849) — the manifest
    /// path's own copy of this check, distinct from `prepare_listing`'s publish-path copy (pinned
    /// by `prepare_listing_rejects_empty_content_types` above). Not covered anywhere else: the
    /// existing `build_slug_manifest_*` tests all use `a_video_collection`, which always carries
    /// `content_types: vec!["video".into()]`.
    #[test]
    fn build_slug_manifest_refuses_empty_content_types() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let col = Collection {
            slug: "empty-types".into(),
            path_alias: "empty-types".into(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![DirectoryItem {
                name: "file.txt".into(),
                item_type: ItemType::File,
                size: None,
                format: None,
                year: None,
                tags: vec![],
                note: None,
                children: vec![],
            }],
        };
        store.save_collection_draft(&col).unwrap();
        let err = build_slug_manifest("empty-types", &store, &identity, &[7u8; 32]).unwrap_err();
        assert!(
            err.contains("At least one content type is required"),
            "got: {err}"
        );
    }

    // ── unpublish_collection → unpublish_collection_inner (line 944) ───────────────────────

    /// Pins `unpublish_collection_inner`'s own slug-format guard (line 950). Every existing
    /// `unpublish_collection_inner` test (`unpublish_*`, `deleting_a_published_collection_*`)
    /// calls it with an already-valid slug, so this guard has no coverage anywhere in the file.
    /// The guard fires before any store or relay access, so an unrouted `SharedRelay` and an
    /// empty store are both fine here.
    #[tokio::test]
    async fn unpublish_collection_inner_refuses_an_invalid_slug() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        let err = unpublish_collection_inner("../evil", &store, &identity, &bk, &relay)
            .await
            .unwrap_err();
        assert!(err.contains("Invalid collection slug"), "got: {err}");
    }

    /// QURATOR-249 (teardown half): unpublishing a legacy "profile" draft — the exact population
    /// the publish-half guard's comment names (restored from backup, or predating the scan guard)
    /// — used to pass `is_valid_slug` and reach `delete_published("profile")`, destroying the LIVE
    /// profile teaser marker. The refusal fires before any store read or relay access. The
    /// survival assert is the INV-8 substance: the marker must still be there afterwards.
    ///
    /// Mutation to redden: in `unpublish_collection_inner` (the `pub(crate) async fn` whose body
    /// starts with the `is_valid_slug` `.ok_or("Invalid collection slug")?` line), change the
    /// `if is_reserved_marker_slug(safe_slug) {` of the QURATOR-249 teardown-half guard — the one
    /// sitting between that `is_valid_slug` line and the QURATOR-200 `marker_tier` comment — to
    /// `if false {` (or delete the whole block). The call then walks into
    /// `retract_public_listing` (best-effort against the unroutable relay set up below, so no
    /// real network) and on to `delete_published("profile")`: `unwrap_err()` panics on the Ok,
    /// and even a `.unwrap()`-tolerant run leaves `is_published("profile")` false, redding the
    /// final assert.
    #[tokio::test]
    async fn unpublish_collection_inner_refuses_a_reserved_marker_slug() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // Unroutable relay, as in `deleting_a_published_collection_*`: a mutated run walks into
        // the best-effort retraction and must fail fast offline, not hang on the real defaults.
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["ws://127.0.0.1:1".into()],
                ..Default::default()
            })
            .unwrap();
        // The live profile teaser marker — the durable state the unguarded code destroyed.
        store.save_published("profile", "{}").unwrap();
        let identity = Identity::generate();
        let bk: BrowseKey = rand::random();
        let relay = crate::net::new_shared();
        let err = unpublish_collection_inner("profile", &store, &identity, &bk, &relay)
            .await
            .unwrap_err();
        assert!(err.contains("'profile' is a reserved name"), "got: {err}");
        assert!(
            store.is_published("profile"),
            "INV-8: a refused unpublish must leave the profile teaser's marker in place"
        );
    }

    // ── OWED — reported, not refactored into reach ──────────────────────────────────────────
    //
    // Each of these is a guard living directly in a `#[tauri::command]` body with no `*_inner`
    // (or, for the identity checks, one whose own delegate takes the ALREADY-unwrapped identity —
    // the guard itself never runs except inside the command). Reaching any of them from a test
    // would mean either enabling `tauri`'s `test` feature in `hb-app/Cargo.toml` (a Cargo.toml
    // change) or extracting a new `*_inner` function (a production restructuring) — both out of
    // scope for a tests-only slice, per the ticket's hard constraint.
    //
    //   - `publish_collection` (line 808), "No identity loaded" guard at line 816.
    //   - `export_manifest` (line 892), "No identity loaded" guard at line 900.
    //   - `unpublish_collection` (line 1036), "No identity loaded" guard at line 1044.
    //   - `update_collection_visibility` (line 1052): `is_valid_slug` guard at line 1059, and the
    //     "No draft found for collection '{safe_slug}'" guard at line 1063. Nothing in the command
    //     body delegates to an extractable pure function — load → mutate `visibility` → save is
    //     all inline.
    //   - `export_collection` (line 1071): `is_valid_slug` guard at line 1078, and the
    //     "Collection '{safe_slug}' not found" guard at line 1083. Same shape — no `*_inner`.
}

/// The five commands above (`publish_collection`, `export_manifest`, `unpublish_collection`,
/// `update_collection_visibility`, `export_collection`) hold their guards INLINE with no
/// extractable `*_inner`, and take `State<'_, DataStore>` (plus, for three of them,
/// `State<'_, SharedIdentity>` / `State<'_, SharedRelay>`) — so `collection_command_guards_b`
/// reported them OWED, reachable only by enabling `tauri`'s `test` feature. That feature is now
/// on (`hb-app/Cargo.toml` dev-dependencies), and `collection_command_guards_a`'s `guard_app()`
/// pattern (mock `tauri::App`, `.manage()` the same states the real app manages) already proves
/// the route works for sibling commands in this same file. This module drives all seven guards
/// listed as OWED above through that same route.
#[cfg(test)]
mod collection_command_guards_c {
    use super::*;
    use tauri::Manager;
    use tempfile::TempDir;

    fn guard_app() -> (TempDir, tauri::App<tauri::test::MockRuntime>) {
        let app = tauri::test::mock_app();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        app.manage(store);
        let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        app.manage(identity);
        app.manage(net::new_shared());
        (dir, app)
    }

    // -- publish_collection (line 808) -------------------------------------------------------

    /// With no identity loaded, `publish_collection` refuses before touching the store or relay.
    /// Pins the "No identity loaded" guard at line 816.
    ///
    /// Mutation to redden: in `publish_collection`, change the exact literal
    /// `"No identity loaded. Generate a keypair first."` at that `.ok_or(...)` to any other text.
    #[tokio::test]
    async fn publish_collection_refuses_without_identity() {
        let (_dir, app) = guard_app();
        let err = publish_collection(
            "my-slug".into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "No identity loaded. Generate a keypair first.");
    }

    // -- export_manifest (line 892) ----------------------------------------------------------

    /// With no identity loaded, `export_manifest` refuses before it ever calls
    /// `build_slug_manifest` or touches the filesystem. Pins the "No identity loaded" guard at
    /// line 900.
    ///
    /// Mutation to redden: in `export_manifest`, change the exact literal
    /// `"No identity loaded. Generate a keypair first."` at that `.ok_or(...)` to any other text.
    #[tokio::test]
    async fn export_manifest_refuses_without_identity() {
        let (dir, app) = guard_app();
        let out_path = dir.path().join("out.hbmanifest").to_string_lossy().into_owned();
        let err = export_manifest(
            "my-slug".into(),
            out_path.clone(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "No identity loaded. Generate a keypair first.");
        assert!(!std::path::Path::new(&out_path).exists(), "a refused export must not write a file");
    }

    // -- unpublish_collection (line 1036) ----------------------------------------------------

    /// With no identity loaded, `unpublish_collection` refuses before delegating to
    /// `unpublish_collection_inner`. Pins the "No identity loaded" guard at line 1044.
    ///
    /// Mutation to redden: in `unpublish_collection`, change the exact literal
    /// `"No identity loaded. Generate a keypair first."` at that `.ok_or(...)` to any other text.
    #[tokio::test]
    async fn unpublish_collection_refuses_without_identity() {
        let (_dir, app) = guard_app();
        let err = unpublish_collection(
            "my-slug".into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "No identity loaded. Generate a keypair first.");
    }

    // -- delete_collection (line 427) ---------------------------------------------------------

    /// QURATOR-249 (teardown half): deleting a legacy "profile" draft — with the profile teaser
    /// marker live, so `is_published("profile")` reads TRUE through the key collision — must
    /// still delete the draft (no stranding) while bypassing the unpublish half and sparing the
    /// teaser's marker file, which `DataStore::delete_collection` would otherwise sweep. Drives
    /// the real command through the mock app; no identity is loaded, which is fine because the
    /// reserved branch must return before the identity read.
    ///
    /// Mutation to redden (primary): in `delete_collection` (the `#[tauri::command]` whose body
    /// starts with the `is_valid_slug` `.ok_or("Invalid collection slug")?` guard), change the
    /// `if is_reserved_marker_slug(safe_slug) {` of the QURATOR-249 branch — the one between
    /// that guard and the devtest #11 / QURATOR-138 comment — to `if false {`. The flow then
    /// enters the `store.is_published` branch, hits the missing identity, and `unwrap()` reds
    /// with "No identity loaded". (With an identity loaded it would redden via the
    /// `unpublish_collection_inner` refusal instead — the two guards back each other up.)
    ///
    /// Mutation to redden (secondary, pins the spared marker): in that same branch, replace the
    /// four-path `for` loop with `store.delete_collection(safe_slug).map_err(cmd_err)?;` — the
    /// marker file is swept and the `is_published("profile")` assert reds.
    #[tokio::test]
    async fn deleting_a_legacy_profile_draft_spares_the_teaser_marker() {
        let (_dir, app) = guard_app();
        let store = app.state::<DataStore>();
        // The collision population: a live profile teaser marker + a draft slugged "profile".
        store.save_published("profile", "{}").unwrap();
        let col = Collection {
            slug: "profile".into(),
            path_alias: "profile".into(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();

        delete_collection(
            "profile".into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap();

        assert!(
            store.load_collection_draft("profile").unwrap().is_none(),
            "the legacy draft must actually be deleted — a reserved slug must not strand it"
        );
        assert!(
            store.is_published("profile"),
            "INV-8: deleting the draft must spare the profile teaser's marker — it belongs to \
             the profile subsystem, not this collection"
        );
    }

    /// QURATOR-284: the reserved-slug branch hand-rolls a FOUR-path sidecar sweep that mirrors
    /// `DataStore::delete_collection`'s FIVE-path list minus `published_path` (sparing the marker
    /// is the fix). Nothing else pins the two lists together, and drift is worse than an orphan
    /// file: a leftover sidecar keeps `target_path_is_occupied` (backup.rs) true, blocking future
    /// restores until someone cleans the store by hand. So materialise ALL FIVE sidecars via the
    /// store's own save methods and assert exactly `published_path` survives.
    ///
    /// ⚠ SCOPE, corrected after the 2026-09-16 review: this does NOT automatically red when the
    /// store gains a SIXTH sidecar. It hardcodes today's five save methods, and nothing anywhere
    /// pins `DataStore::delete_collection`'s own list, so a sixth path would be ignored by the
    /// command, the store's caller here, and this test alike. What it genuinely pins is the
    /// command's FOUR-path list and the spare-the-marker invariant — each path mutation-proven.
    /// Closing the drift gap for real needs the consolidation the branch's own comment names: one
    /// store-level sidecar list consumed by both deleters. Filed separately; do not read this
    /// test as already providing it.
    /// Unlike `deleting_a_legacy_profile_draft_spares_the_teaser_marker`, which creates only the
    /// draft, this covers every path the branch must remove.
    ///
    /// Mutation to redden: in `delete_collection`'s reserved-marker branch, delete the
    /// `store.share_settings_path(slug),` entry from the `vec![` inside `DataStore::
    /// collection_sidecars` (store.rs) — the SINGLE list both delete variants now consume since
    /// QURATOR-288. Resolve it by line number inside that fn; the same text appears in store.rs's
    /// own test doc comments, so an unqualified text match would mutate a comment and report a
    /// good control as decorative. The `share_settings_path` entry in the `gone` loop below reds.
    ///
    /// Second, independent mutation (attributes THIS call site rather than the shared list): in
    /// `delete_collection`'s reserved-marker branch, change
    /// `store.delete_collection_sparing_published_marker(safe_slug)` to
    /// `store.delete_collection(safe_slug)` — the marker-survival assert below reds while the
    /// four `gone` asserts stay green, which is the discrimination this test exists for.
    /// ⚠ Re-pointed 2026-09-16: this comment previously named a hand-rolled four-path `for` loop
    /// in this file. QURATOR-288 replaced that loop with the store call above, so the old anchor
    /// no longer exists — a mutation aimed at it would silently no-op and read as a vacuous control.
    #[tokio::test]
    async fn deleting_a_reserved_slug_removes_every_sidecar_but_the_published_marker() {
        let (_dir, app) = guard_app();
        let store = app.state::<DataStore>();
        // Materialise every sidecar `DataStore::delete_collection` sweeps, via the store's own
        // save methods so the paths cannot drift from the real ones.
        store.save_published("profile", "{}").unwrap();
        let col = Collection {
            slug: "profile".into(),
            path_alias: "profile".into(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        store
            .save_share_settings("profile", &crate::store::ShareSettings::default())
            .unwrap();
        store
            .save_scan_spec("profile", &crate::store::ScanSpec::default())
            .unwrap();
        store
            .save_snapshot_fingerprint("profile", &hb_core::SnapshotFingerprint("pin".into()))
            .unwrap();
        // Precondition: all five exist, so a silently failing save cannot vacuously green the
        // absence asserts below.
        for present in [
            store.published_path("profile"),
            store.collection_draft_path("profile"),
            store.share_settings_path("profile"),
            store.scan_spec_path("profile"),
            store.snapshot_fingerprint_path("profile"),
        ] {
            assert!(present.exists(), "precondition: {} must exist", present.display());
        }

        delete_collection(
            "profile".into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap();

        assert!(
            store.published_path("profile").exists(),
            "the profile teaser's marker is the one sidecar the reserved branch must spare"
        );
        for gone in [
            store.collection_draft_path("profile"),
            store.share_settings_path("profile"),
            store.scan_spec_path("profile"),
            store.snapshot_fingerprint_path("profile"),
        ] {
            assert!(
                !gone.exists(),
                "orphaned sidecar {} keeps target_path_is_occupied true and blocks future \
                 restores — the reserved branch's list has drifted from \
                 DataStore::delete_collection's",
                gone.display()
            );
        }
    }

    /// QURATOR-270 sibling sites (found by the 2026-09-16 adversarial review of the
    /// `get_share_settings` fix): the same defect class survived one file over. Both commands take
    /// an IPC-supplied slug and reach a naive path join BEFORE any guard —
    /// `collection_source_accessible` -> `scan_spec_path` (`collections/<slug>.scan.json`), and
    /// `publish_collection` -> `published_generation` (`published/<slug>.gen`), which is read
    /// before `prepare_listing`'s own guard fires. Each is an existence/parse oracle for a file
    /// outside the store. This is the hardened-path/unhardened-sibling drift pair: fixing one slug
    /// site without grepping the others is how the pair forms.
    ///
    /// Mutation to redden (two independent halves — apply ONE at a time, since mutating two
    /// changes at once cannot attribute the failure):
    /// (1) in `collection_source_accessible`'s production body, change its
    ///     `.ok_or("Invalid collection slug")?;` line to `.unwrap_or("");` — the first assert reds.
    /// (2) in `publish_collection`'s production body, change its `if !is_valid_slug(&slug) {`
    ///     line to `if false {` — the second assert reds.
    /// Neither anchor is unique by text (both forms recur in this file and in this comment), so
    /// resolve each by its enclosing function, never by a bare match.
    #[tokio::test]
    async fn slug_consuming_siblings_refuse_a_traversal_slug_before_any_store_read() {
        let (_dir, app) = guard_app();

        let err = collection_source_accessible("../outside".into(), app.state::<DataStore>())
            .await
            .unwrap_err();
        assert_eq!(
            err, "Invalid collection slug",
            "collection_source_accessible must refuse a traversal slug before load_scan_spec joins it"
        );

        let err = publish_collection(
            "../outside".into(),
            app.state::<DataStore>(),
            app.state::<SharedIdentity>(),
            app.state::<SharedRelay>(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err, "Invalid collection slug",
            "publish_collection must refuse a traversal slug before published_generation reads it"
        );
    }

// -- update_collection_visibility (line 1052) --------------------------------------------

    /// An alias that fails `is_valid_slug` is refused before any store access. Pins the guard at
    /// line 1059.
    ///
    /// Mutation to redden: in `update_collection_visibility`, change the exact literal
    /// `"Invalid collection slug"` at that `.ok_or(...)` to any other text.
    #[tokio::test]
    async fn update_collection_visibility_rejects_an_invalid_slug() {
        let (_dir, app) = guard_app();
        let err = update_collection_visibility(
            "../evil".into(),
            Visibility::Private,
            app.state::<DataStore>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Invalid collection slug");
    }

    /// A valid slug with no saved draft refuses rather than silently creating one. Pins the "No
    /// draft found" guard at line 1063.
    ///
    /// Mutation to redden: in `update_collection_visibility`, change the exact literal
    /// `"No draft found for collection '{safe_slug}'"` at that `.ok_or_else(...)` to different
    /// text (e.g. drop the slug interpolation) — the test's `assert_eq!` on the exact message
    /// reds.
    #[tokio::test]
    async fn update_collection_visibility_refuses_a_missing_draft() {
        let (_dir, app) = guard_app();
        let err = update_collection_visibility(
            "no-such-draft".into(),
            Visibility::Private,
            app.state::<DataStore>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "No draft found for collection 'no-such-draft'");
    }

    // -- export_collection (line 1071) -------------------------------------------------------

    /// An alias that fails `is_valid_slug` is refused before any store access. Pins the guard at
    /// line 1078.
    ///
    /// Mutation to redden: in `export_collection`, change the exact literal
    /// `"Invalid collection slug"` at that `.ok_or(...)` to any other text.
    #[tokio::test]
    async fn export_collection_rejects_an_invalid_slug() {
        let (_dir, app) = guard_app();
        let err = export_collection(
            "../evil".into(),
            "text".into(),
            app.state::<DataStore>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Invalid collection slug");
    }

    /// A valid slug with no saved draft refuses rather than returning an empty export. Pins the
    /// "not found" guard at line 1083.
    ///
    /// Mutation to redden: in `export_collection`, change the exact literal
    /// `"Collection '{safe_slug}' not found"` at that `.ok_or_else(...)` to different text (e.g.
    /// drop the slug interpolation) — the test's `assert_eq!` on the exact message reds.
    #[tokio::test]
    async fn export_collection_refuses_a_missing_collection() {
        let (_dir, app) = guard_app();
        let err = export_collection(
            "no-such-collection".into(),
            "text".into(),
            app.state::<DataStore>(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Collection 'no-such-collection' not found");
    }

    /// QURATOR-254: `path_alias` — the first line of the export — gets the audit #18 treatment
    /// item names already get, matched to the format: control characters stripped (so the alias
    /// forges no rows) and, for markdown, metacharacters escaped (so no live `![image]` markup).
    /// A hostile alias (`Films\n\n![p](https://attacker.example/t.png)`) must arrive as ONE
    /// inert line in both formats.
    ///
    /// Mutation to redden: in `export_collection`, revert the emission to the raw field —
    /// change `Ok(format!("{alias}\n\n{out}"))` to
    /// `Ok(format!("{}\n\n{}", collection.path_alias, out))` — the markdown assert on the
    /// escaped first line reds, and the text export's line count reds (the raw alias's `\n\n`
    /// forges two extra lines).
    #[tokio::test]
    async fn export_collection_neutralizes_a_hostile_path_alias() {
        let (_dir, app) = guard_app();
        let col = Collection {
            slug: "films".into(),
            path_alias: "Films\n\n![p](https://attacker.example/t.png)".into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        app.state::<DataStore>().save_collection_draft(&col).unwrap();

        // Markdown: controls stripped AND metacharacters escaped — one line, no live markup.
        let md = export_collection("films".into(), "markdown".into(), app.state::<DataStore>())
            .await
            .unwrap();
        assert_eq!(
            md.lines().next().unwrap(),
            "Films\\!\\[p\\]\\(https://attacker\\.example/t\\.png\\)",
            "markdown export must escape the alias's metacharacters"
        );
        assert!(!md.contains("\n\n!["), "the alias may not forge a live-image row");

        // Text: controls stripped — the alias is one line; markup syntax is inert in plain text
        // and stays literal, exactly as item names behave in `render_text`.
        let txt = export_collection("films".into(), "text".into(), app.state::<DataStore>())
            .await
            .unwrap();
        assert_eq!(
            txt.lines().next().unwrap(),
            "Films![p](https://attacker.example/t.png)",
            "text export must strip the alias's control characters"
        );
        // The trailing blank line is the export's own `{alias}\n\n{out}` separator (the listing is
        // empty here) — the alias itself contributes exactly ONE line and forges no row.
        assert_eq!(
            txt,
            "Films![p](https://attacker.example/t.png)\n\n",
            "the alias may not forge additional rows (only the separator's blank line may follow)"
        );
    }
}


/// QURATOR-138 — "Unpublish becomes DELETE" (owner ruling 2026-08-30): one destructive operation
/// that removes the local record AND zeroes the published event (a tombstone KIND_LISTING at the
/// same `d`, plus the existing best-effort NIP-09 request). These pins hold the local half; the
/// wire half (a conforming relay actually replacing the listing) is `hb-wan-it`'s, owner-run.
#[cfg(test)]
mod q138_delete_pins {
    use super::*;
    use tauri::Manager;

    fn guard_app_with_identity() -> (tempfile::TempDir, tauri::App<tauri::test::MockRuntime>) {
        let app = tauri::test::mock_app();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // An unroutable relay: the best-effort wire attempts (NIP-09 + tombstone) fail fast
        // without ever touching a real relay — these tests assert LOCAL effects only.
        store
            .save_settings(&crate::store::Settings {
                relay_urls: vec!["ws://127.0.0.1:1".into()],
                ..Default::default()
            })
            .unwrap();
        app.manage(store);
        let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(Some(
            crate::identity_state::AppIdentity::generate(),
        )));
        app.manage(identity);
        app.manage(net::new_shared());
        (dir, app)
    }

    fn make_draft(store: &DataStore, slug: &str) {
        let col = Collection {
            slug: slug.to_string(),
            path_alias: slug.to_string(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
    }

// ── QURATOR-138 — Unpublish becomes DELETE (owner ruling 2026-08-30) ────────────────────

/// AC: the tombstone the delete path publishes must be a real KIND_LISTING event — the same
/// kind, the SAME `d` = slug, a zeroed payload, and `created_at = now` (never future-dated —
/// strfry refuses future-dated writes, which would make the delete silently fail to publish).
/// Built with the EXACT production function the delete path calls
/// (`hb_core::event::build_tombstone_event`), so a production regression reds this.
///
/// Mutation to redden: in `crates/hb-core/src/event.rs`, change `build_tombstone_event` to sign
/// `EventBuilder::new(Kind::from_u16(KIND_LISTING), "")` with NO `.tags(...)` — the
/// `tags.identifier()` assertion fails; or in `tombstone_listing_json`, change `"entries": []`
/// to `"entries": [{"name":"x"}]` — the `entries` assertion fails.
#[test]
fn tombstone_is_a_same_d_zeroed_listing_at_now() {
    let id = Identity::generate();
    let bk: BrowseKey = rand::random();
    let ev = hb_core::event::build_tombstone_event(&id, "films", &bk).unwrap();
    assert_eq!(ev.kind, Kind::from_u16(hb_core::event::KIND_LISTING), "same replaceable kind");
    assert_eq!(ev.tags.identifier(), Some("films"), "same d — so a conforming relay REPLACES");
    assert!(ev.created_at <= nostr::Timestamp::now(), "created_at must never be future-dated");
    let (slug, json) = hb_core::event::parse_listing_event(&ev, &bk).unwrap();
    assert_eq!(slug, "films");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["entries"], serde_json::json!([]), "zeroed");
    assert_eq!(v["item_count"], serde_json::json!(0), "zeroed");
}

/// AC: deleting a published collection destroys the LOCAL record completely — draft, published
/// marker, share settings, scan spec, snapshot fingerprint — leaving nothing for auto-republish
/// or the collections list to resurrect it from. `delete_collection` is called exactly as the
/// UI calls it (the full command, identity loaded, unroutable relay so the best-effort wire
/// side fails fast — local effects only; the wire half is `hb-wan-it`'s, owner-run).
///
/// Mutation to redden: in `delete_collection`, change the final
/// `store.delete_collection(safe_slug).map_err(cmd_err)` to
/// `store.delete_published(safe_slug).map_err(cmd_err)?;` (deleting only the marker) — the
/// `load_collection_draft` assertion fails. (Flipping the `if store.is_published(...)` gate to
/// `if false` does NOT red this test: the store method removes the published path anyway —
/// marker removal is doubly covered, which is why the draft assertion is the discriminator.)
#[tokio::test]
async fn deleting_a_published_collection_destroys_the_local_record() {
    let (_dir, app) = guard_app_with_identity();
    let store = app.state::<DataStore>();
    make_draft(&store, "films");
    store.save_published("films", r#"{"parts":1}"#).unwrap();
    assert!(store.is_published("films"), "premise: published before the delete");

    delete_collection(
        "films".into(),
        app.state::<DataStore>(),
        app.state::<SharedIdentity>(),
        app.state::<SharedRelay>(),
    )
    .await
    .unwrap();

    assert!(
        store.load_collection_draft("films").unwrap().is_none(),
        "the local draft record must be destroyed"
    );
    assert!(!store.is_published("films"), "the published marker must be destroyed");
    assert!(!store.list_collection_slugs().unwrap().contains(&"films".to_string()));
}

/// AC 4: auto-publish must not resurrect a deleted collection. The owner named this trap: a
/// delete that clears the published event while the local record survives gets undone by the
/// next auto-publish tick. The watch's republish gate is `evaluate_rescan`, so the pin is:
/// after `delete_collection`, `evaluate_rescan` must read **Skipped("not published")** — no
/// republish can even be scheduled for the deleted slug, because there is neither marker nor
/// draft nor scan spec left to act on.
///
/// Mutation to redden: in `watch.rs::evaluate_rescan`, invert the early-return gate — change
/// `if !store.is_published(slug) { return Ok(RescanDecision::Skipped("not published".into())); }`
/// to `if store.is_published(slug) { return Ok(RescanDecision::Skipped("not published".into())); }`.
/// A deleted slug then walks past the gate; the scan spec this test sets up makes `rescan_listing`
/// return a listing, so the decision becomes `Changed` and the `assert_eq!(reason, "not published")`
/// panic fires. (No single delete-path edit reds this test — the marker is removed twice, once by
/// `unpublish_collection_inner`'s `delete_published` and once by the store's own
/// `delete_collection` — so the watch gate is the single attributable resurrection mechanism.)
#[tokio::test]
async fn delete_stops_the_watch_so_autopublish_cannot_resurrect() {
    let (dir, app) = guard_app_with_identity();
    let store = app.state::<DataStore>();
    make_draft(&store, "films");
    // A scan spec + fingerprint make the watch's republish path otherwise actionable — the
    // strongest possible setup for a resurrection.
    store
        .save_scan_spec("films", &crate::store::ScanSpec {
            root: dir.path().to_string_lossy().into_owned(),
            include: vec![],
            exclude: vec![],
            total_bytes: 1,
        })
        .unwrap();
    store.save_published("films", r#"{"parts":1}"#).unwrap();
    assert!(store.is_published("films"));

    delete_collection(
        "films".into(),
        app.state::<DataStore>(),
        app.state::<SharedIdentity>(),
        app.state::<SharedRelay>(),
    )
    .await
    .unwrap();

    // The auto-publish tick's exact gate: the watch cannot republish what is not published.
    match crate::watch::evaluate_rescan("films", &store).unwrap() {
        crate::watch::RescanDecision::Skipped(reason) => assert_eq!(
            reason, "not published",
            "the deleted collection must be out of the watch's scope entirely"
        ),
        other => panic!("a deleted collection must never be republishable, got {other:?}"),
    }
}


}
