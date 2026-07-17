use std::path::Path;
use globset::{Glob, GlobSetBuilder};
use hb_core::types::{Collection, DirectoryItem, ItemType, Visibility};
use hb_core::{BrowseKey, Identity};
use hb_net::{publish_listing_capped, publish_listing_to};
use nostr::{Filter, Kind};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    commands::profile::{compute_content_types, teaser_from_profile},
    error::{CmdResult, cmd_err},
    net::{self, SharedRelay},
    store::DataStore,
    SharedIdentity,
};

/// NIP-44 listing split budget (NIP-44 caps plaintext, the relay caps the event ~64 KiB; ≤40 KB
/// keeps a single part well under both). Larger listings split per-folder (hb-net::split_listing).
const LISTING_MAX_BYTES: usize = 40_000;

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
    // Surface the scanned byte total to the freshly-added collection immediately (devtest #5) — read
    // it back from the sidecar scan_directory_inner just persisted, so size shows pre-reload too.
    let total_bytes = store
        .load_scan_spec(&collection.slug)
        .ok()
        .flatten()
        .map(|s| s.total_bytes)
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

/// Core scan + persist logic, extracted for testability (mirrors
/// `publish_collection_inner`). Walks the directory off the async runtime
/// thread under a deadline, then builds and persists the draft collection.
async fn scan_directory_inner(opts: ScanOptions, store: &DataStore) -> CmdResult<Collection> {
    let root = std::path::PathBuf::from(&opts.path);

    let slug = Collection::slug_from_alias(&opts.path_alias);
    if !is_valid_slug(&slug) {
        return Err(format!(
            "'{}' produces an invalid collection slug — use only letters, numbers, hyphens, or Unicode characters; avoid spaces and symbols",
            opts.path_alias
        ));
    }
    let globs = build_glob_set(&opts.exclude);
    let include = IncludeSet::new(opts.include.clone());

    // Walk the filesystem off the async runtime thread under a hard deadline.
    // We deliberately avoid `tokio::time::timeout` + `tokio::task::spawn_blocking`:
    // those require the executing runtime's tokio time/blocking drivers, and when
    // that requirement isn't met the command panics. Release builds set
    // `windows_subsystem = "windows"` (no console), so such a panic is silent — the
    // IPC response is never sent and the dialog hangs on "Scanning…" forever.
    // Tauri's own `spawn_blocking` plus a std `recv_timeout` deadline has no such
    // hidden runtime dependency.
    let scan = move || -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
        anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
        scan_selective(&root, &include, &globs)
    };
    let (listing, total_bytes) = match tauri::async_runtime::spawn_blocking(move || {
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
    let item_count = count_items(&listing);
    let est_size = if total_bytes > 0 { Some(format_size(total_bytes)) } else { None };

    let mut collection = Collection {
        slug,
        path_alias: opts.path_alias,
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

    store.save_collection_draft(&collection).map_err(cmd_err)?;

    // Persist the collection's on-disk root so the snapshot re-scan can find the tree again.
    let mut share = store
        .load_share_settings(&collection.slug)
        .map_err(cmd_err)?
        .unwrap_or_default();
    share.root_path = Some(opts.path.clone());
    store.save_share_settings(&collection.slug, &share).map_err(cmd_err)?;

    // M9: persist the exact scan parameters so the snapshot watch can faithfully re-scan this tree
    // (same root, same checked folders, same exclusions) when the source changes.
    store
        .save_scan_spec(
            &collection.slug,
            &crate::store::ScanSpec {
                root: opts.path.clone(),
                include: opts.include.clone(),
                exclude: opts.exclude.clone(),
                // Persisted here (never published) so get_collections can surface the aggregate
                // "Total Size" the home view reads from `total_bytes` (devtest 2026-06-25 #5).
                total_bytes,
            },
        )
        .map_err(cmd_err)?;

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
    let globs = build_glob_set(&spec.exclude);
    let include = IncludeSet::new(spec.include.clone());
    let scan = move || -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
        anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
        scan_selective(&root, &include, &globs)
    };
    let (mut listing, _bytes) =
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
    // devtest #11: a published collection contributes to the profile teaser's content_types union and
    // still has listing events on relays. Unpublish it first (NIP-09 delete + teaser content_types/tags
    // recompute, both best-effort offline) so removal drops it from the public teaser AND from relays,
    // then delete the local draft/marker.
    if store.is_published(safe_slug) {
        let id_clone = {
            let guard = identity.read().await;
            guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?.identity.clone()
        };
        unpublish_collection_inner(safe_slug, &store, &id_clone, &relay).await?;
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

    // Load the draft, update fields, and re-save.
    let mut col = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?;

    col.description = description;
    col.content_types = content_types;
    col.tags = tags;
    col.languages = languages;
    col.sorted = sorted;
    store.save_collection_draft(&col).map_err(cmd_err)
}

/// Map a `Collection` draft to the render-model listing JSON: the directory tree moves from
/// `listing` to `entries` (what `hb-net::render_listing` consumes), the rest stays as metadata.
/// Pure — unit-tested without a relay.
pub(crate) fn collection_to_listing_json(col: &Collection) -> Result<String, String> {
    let mut v = serde_json::to_value(col).map_err(cmd_err)?;
    if let serde_json::Value::Object(ref mut map) = v {
        if let Some(listing) = map.remove("listing") {
            map.insert("entries".into(), listing);
        }
        // M16 W3: stamp the full-tree snapshot fingerprint into the listing metadata so it rides —
        // through `truncate_listing` (the paywall teaser) and `split_listing` (the big-relay full
        // family, W2) — into `RenderedListing.meta`, where the browse-side staleness gate reads it.
        // An order-independent content hash of the whole tree: the teaser and the family carry the
        // *same* value, which is exactly what lets a browser confirm the family is the full version
        // of what the teaser previews.
        let fp = hb_core::snapshot_fingerprint(&col.listing);
        map.insert("snapshot_fingerprint".into(), serde_json::Value::String(fp.0));
    }
    serde_json::to_string(&v).map_err(cmd_err)
}

/// Validate + load a draft and produce its listing JSON, ready to publish. Pure (no relay) so the
/// validation paths are L1-testable.
pub(crate) fn prepare_listing(slug: &str, store: &DataStore) -> Result<String, String> {
    let safe_slug = is_valid_slug(slug).then_some(slug).ok_or("Invalid collection slug")?;
    let collection = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?;
    if collection.content_types.is_empty() {
        return Err("At least one content type is required before publishing a collection.".into());
    }
    collection_to_listing_json(&collection)
}

/// Current unix time in seconds (the seal/publish timestamp). A clock before 1970 reads as 0.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Collect the recipient pubkeys a **Private** collection must be sealed to: every `npub` in a
/// group marked `trusted`, parsed + deduped. Errs if there is no trusted audience (publishing a
/// Private collection to nobody is a mistake, not a silent no-op). An unparseable id (e.g. a legacy
/// non-Nostr contact) is skipped. Pure — unit-tested without a relay.
pub(crate) fn private_recipients(store: &DataStore) -> Result<Vec<nostr::PublicKey>, String> {
    use std::collections::BTreeSet;
    let groups = store.load_groups().map_err(cmd_err)?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<nostr::PublicKey> = Vec::new();
    for g in groups.iter().filter(|g| g.trusted) {
        for npub in &g.pubkeys {
            if seen.insert(npub.clone()) {
                if let Ok(pk) = hb_core::identity::parse_npub(npub) {
                    out.push(pk);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(
            "This collection is Private, but you have no trusted contacts. Mark a contact group as \
             trusted (and add members) before publishing a Private collection."
                .into(),
        );
    }
    Ok(out)
}

/// Publish a collection's listing. **Branches on visibility (M10):** a *Public* collection is
/// encrypted once under the account browse-key (M3); a *Private* collection is sealed per trusted
/// `npub` and gift-wrapped — the browse-key is **not** used and the public teaser is **not** touched
/// (no private holding leaks). Marks it published locally and (public only) keeps a published
/// teaser's content_types current.
pub(crate) async fn publish_collection_inner(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    browse_key: &BrowseKey,
    relay: &SharedRelay,
) -> Result<PublishSummary, String> {
    let listing_json = prepare_listing(slug, store)?;

    // Visibility gate: a Private collection takes the sealed, per-recipient path and never touches
    // the browse-key or the public teaser (and is never truncated — trusted recipients get it all).
    let visibility = store
        .load_collection_draft(slug)
        .map_err(cmd_err)?
        .map(|c| c.visibility)
        .unwrap_or(Visibility::Public);
    if visibility == Visibility::Private {
        publish_private_collection_inner(slug, store, identity, &listing_json, relay).await?;
        return Ok(PublishSummary::whole());
    }

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
    // just the teaser). A listing that fit whole, or an unset big relay, takes no big-relay write:
    // the shipped small-collection path is byte-identical. **Best-effort** — the teaser already went
    // out above, so a big-relay hiccup must not fail the whole publish; the browse side falls back
    // to the teaser (fingerprint-gated) and the owner can re-publish.
    let big_relay_url = store.load_settings().map_err(cmd_err)?.unwrap_or_default().big_relay_url;
    let big_relay_parts = match big_relay_target(published.truncated, &big_relay_url) {
        Some(big) => {
            let relays = [big.to_string()];
            match client.ensure_relays(&relays, net::RELAY_TIMEOUT).await {
                Ok(()) => match publish_listing_to(
                    &client, identity, slug, browse_key, &listing_json, LISTING_MAX_BYTES, &relays,
                )
                .await
                {
                    Ok(family) => family.parts,
                    Err(e) => {
                        tracing::warn!("big-relay publish for '{slug}' failed ({e}); the teaser stands");
                        0
                    }
                },
                Err(e) => {
                    tracing::warn!("big relay '{big}' unreachable ({e}); the teaser stands");
                    0
                }
            }
        }
        None => 0,
    };

    // Local published marker (the "published" badge + content_types union), now also recording the
    // truncation state + big-relay part count for reference.
    let marker = serde_json::json!({
        "parts": published.parts,
        "truncated": published.truncated,
        "shown_items": published.shown_items,
        "total_items": published.total_items,
        "big_relay_parts": big_relay_parts,
    })
    .to_string();
    store.save_published(slug, &marker).map_err(cmd_err)?;

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
                let _ = store.save_published("profile", &json);
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
) -> Result<(), String> {
    let recipients = private_recipients(store)?;
    let events = hb_core::seal_private_listing(identity, &recipients, listing_json, now_secs())
        .map_err(cmd_err)?;

    let client = net::client(identity, store, relay).await.map_err(cmd_err)?;
    hb_net::publish_private_listing(&client, &events).await.map_err(cmd_err)?;

    // Local published marker — records the *private* tier + the recipient count (the N× multiplier
    // INV-8 calls out), distinct from the public path's `parts`.
    let marker = serde_json::json!({ "private": true, "recipients": recipients.len() }).to_string();
    store.save_published(slug, &marker).map_err(cmd_err)?;

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
    let (id_clone, browse_key) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone())
    };
    publish_collection_inner(&slug, &store, &id_clone, browse_key.bytes(), &relay).await
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
/// (marker drop only): an honest limit, not a bug.
pub(crate) async fn unpublish_collection_inner(
    slug: &str,
    store: &DataStore,
    identity: &Identity,
    relay: &SharedRelay,
) -> Result<(), String> {
    let safe_slug = is_valid_slug(slug).then_some(slug).ok_or("Invalid collection slug")?;

    let visibility = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .map(|c| c.visibility)
        .unwrap_or(Visibility::Public);

    if visibility == Visibility::Public {
        if let Ok(client) = net::client(identity, store, relay).await {
            let filter =
                Filter::new().author(identity.public_key()).kind(Kind::from_u16(hb_core::event::KIND_LISTING));
            if let Ok(events) = client.fetch(filter, net::RELAY_TIMEOUT).await {
                for ev in events {
                    let belongs = ev
                        .tags
                        .identifier()
                        .map(|d| listing_dtag_belongs_to_slug(d, safe_slug))
                        .unwrap_or(false);
                    if belongs {
                        if let Ok(deletion) = hb_net::build_deletion(identity, &ev) {
                            let _ = client.publish(&deletion).await;
                        }
                    }
                }
            }
        }
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
    let id_clone = {
        let guard = identity.read().await;
        guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?.identity.clone()
    };
    unpublish_collection_inner(&slug, &store, &id_clone, &relay).await
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
    let mut col = store
        .load_collection_draft(safe_slug)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("No draft found for collection '{safe_slug}'"))?;
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

    let out = match format.as_str() {
        "markdown" => render_markdown(&collection.listing, 0),
        _ => render_text(&collection.listing, 0),
    };

    Ok(format!("{}\n\n{}", collection.path_alias, out))
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
            format!("{indent}{prefix}{}{size}{children}", item.name)
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
                format!("{indent}- **{}**{children}", item.name)
            } else {
                let mut meta = vec![];
                if let Some(fmt) = &item.format { meta.push(fmt.clone()); }
                if let Some(sz) = &item.size { meta.push(sz.clone()); }
                let meta_str = if meta.is_empty() { String::new() } else { format!(" `{}`", meta.join(", ")) };
                format!("{indent}- [ ] {}{meta_str}", item.name)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Slug validation
// ---------------------------------------------------------------------------

/// A valid slug contains only Unicode alphanumerics and hyphens.
/// Path-traversal characters (`/`, `.`, `\`, `:`, `%`, NUL, whitespace) are
/// not alphanumeric in any Unicode category and are therefore rejected here,
/// preventing path traversal attacks (e.g., "../identity/keypair").
pub(crate) fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.chars().all(|c| c.is_alphanumeric() || c == '-')
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

/// Selection-aware directory walk (replaces the depth-limited `scan_recursive`). Always lists
/// root-level loose files; fully recurses a subdir iff it (or an ancestor) is checked; for a dir
/// that is only an *ancestor* of a selection, traverses it but withholds its own loose files;
/// otherwise skips it. Validates every `include` entry against the root (F1) before any walk.
pub(crate) fn scan_selective(
    root: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    // F1: containment check on every checked path BEFORE walking anything.
    for c in &include.checked {
        contained_under_root(root, c).map_err(|e| anyhow::anyhow!(e))?;
    }
    scan_selective_walk(root, include, exclude, "")
}

fn scan_selective_walk(
    dir: &Path,
    include: &IncludeSet,
    exclude: &globset::GlobSet,
    rel_prefix: &str,
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    let is_root = rel_prefix.is_empty();
    // Loose files are listed at the root and inside any included directory; an ancestor-only
    // directory withholds them.
    let list_loose_files = is_root || include.is_included(rel_prefix);

    let mut items = vec![];
    let mut total_bytes: u64 = 0;
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
            let included = include.is_included(&rel_path);
            // Recurse into a directory that is selected (full subtree) OR only an ancestor of a
            // selection (to reach the checked descendant). Skip everything else entirely.
            if !included && !include.has_descendant_under(&rel_path) {
                continue;
            }
            let (children, sub_bytes) = scan_selective_walk(&path, include, exclude, &rel_path)?;
            total_bytes += sub_bytes;
            items.push(DirectoryItem {
                name,
                item_type: ItemType::Folder,
                size: None,
                format: None,
                year: None,
                tags: vec![],
                note: None,
                children,
            });
        } else if meta.is_file() && (list_loose_files || include.is_included(&rel_path)) {
            // devtest #10: a file is included when it lives in the root/an included directory (the
            // existing folder rule) OR when it is *itself* checked — so the user can pick individual
            // files inside a directory they did not select wholesale. `has_descendant_under` already
            // keeps this file's ancestor directories traversable above (they're withheld as loose
            // files but recursed to reach the checked file).
            total_bytes += meta.len();
            items.push(DirectoryItem {
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
        }
    }
    items.sort_by(|a, b| match (&a.item_type, &b.item_type) {
        (ItemType::Folder, ItemType::File) => std::cmp::Ordering::Less,
        (ItemType::File, ItemType::Folder) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok((items, total_bytes))
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

fn build_glob_set(patterns: &[String]) -> globset::GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if let Ok(glob) = Glob::new(pat) {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
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
        build_glob_set(&[])
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

        let globs = build_glob_set(&["*.nfo".to_string()]);
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

        let globs = build_glob_set(&["**/*.nfo".to_string()]);
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

        // The draft must be persisted so the empty collection shows up in the list.
        let draft = store.load_collection_draft(&collection.slug).unwrap();
        assert!(draft.is_some(), "empty-folder scan must still save a draft");
    }

    /// Regression (devtest 2026-06-25 #5): the home "Total Size" / "Disk size (auto)" aggregate
    /// reads `total_bytes`, but the published `Collection` deliberately omits exact bytes (hb-core
    /// privacy invariant). The scanned byte total must therefore be persisted in the per-slug
    /// `ScanSpec` sidecar so `get_collections` can surface it on `CollectionEntry` — before the fix
    /// it was computed at scan time and dropped, so the aggregate always read "—".
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
        let spec = store.load_scan_spec(&collection.slug).unwrap().expect("scan persists a spec");
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
        let json = collection_to_listing_json(&col).unwrap();
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

    // ── M16 W3: big-relay classifier (Layer 3 routing) ───────────────────────────
    // The routing decision is a pure function; the actual big-relay wire (family → big relay only,
    // no leak to public) is proven by hb-it Suite BIG1/BIG2 against a live strfry, same split as the
    // publish-path tests above.

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
        // The returned target is trimmed so `ensure_relays`/`publish_to` get a clean URL.
        assert_eq!(
            big_relay_target(true, "  ws://big.example:7777  "),
            Some("ws://big.example:7777"),
        );
    }

    #[test]
    fn prepare_listing_rejects_invalid_slug() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let err = prepare_listing("../evil", &store).unwrap_err();
        assert!(err.contains("Invalid collection slug"), "got: {err}");
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
        let relay = crate::net::new_shared();
        unpublish_collection_inner("films", &store, &identity, &relay).await.unwrap();

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
        let relay = crate::net::new_shared();
        unpublish_collection_inner("vault", &store, &identity, &relay).await.unwrap();

        assert!(!store.is_published("vault"), "the published marker must be gone");
    }

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
            picture: None,
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
        let relay = crate::net::new_shared();
        unpublish_collection_inner("films", &store, &identity, &relay).await.unwrap();
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
    fn private_recipients_requires_an_explicit_trusted_group() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // No groups → Err (a Private collection with no audience is a mistake).
        assert!(private_recipients(&store).is_err());
        // An UN-trusted group with members is still not an audience — trust is explicit.
        let a = hb_core::Identity::generate().npub();
        store
            .save_groups(&[crate::store::Group {
                name: "friends".into(),
                pubkeys: vec![a],
                modified_at: chrono::Utc::now(),
                trusted: false,
                color: None,
            }])
            .unwrap();
        assert!(private_recipients(&store).is_err(), "an untrusted group is not a recipient set");
    }

    #[test]
    fn private_recipients_collects_trusted_deduped_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let a = hb_core::Identity::generate();
        let b = hb_core::Identity::generate();
        store
            .save_groups(&[
                crate::store::Group {
                    name: "inner".into(),
                    // a, b, and a legacy non-Nostr id that must be skipped (not crash).
                    pubkeys: vec![a.npub(), b.npub(), "hb1_legacy_junk".into()],
                    modified_at: chrono::Utc::now(),
                    trusted: true,
                    color: None,
                },
                crate::store::Group {
                    name: "also".into(),
                    pubkeys: vec![a.npub()], // duplicate of `a` across groups → collapsed
                    modified_at: chrono::Utc::now(),
                    trusted: true,
                    color: None,
                },
            ])
            .unwrap();
        let recips = private_recipients(&store).unwrap();
        assert_eq!(recips.len(), 2, "two distinct valid npubs (junk skipped, dup collapsed)");
        assert!(recips.contains(&a.public_key()) && recips.contains(&b.public_key()));
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
}
