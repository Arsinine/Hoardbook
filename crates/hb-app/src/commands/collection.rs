use std::path::Path;
use globset::{Glob, GlobSetBuilder};
use hb_core::types::{Collection, DirectoryItem, ItemType, Visibility};
use hb_core::{BrowseKey, Identity};
use hb_net::publish_listing;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    commands::profile::compute_content_types,
    error::{CmdResult, cmd_err},
    net,
    store::DataStore,
    SharedIdentity,
};

/// NIP-44 listing split budget (NIP-44 caps plaintext, the relay caps the event ~64 KiB; ≤40 KB
/// keeps a single part well under both). Larger listings split per-folder (hb-net::split_listing).
const LISTING_MAX_BYTES: usize = 40_000;

/// Collection with publication status, returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionEntry {
    #[serde(flatten)]
    pub collection: Collection,
    /// True if this collection has been signed and published.
    pub published: bool,
}

#[derive(Debug, Deserialize)]
pub struct ScanOptions {
    pub path: String,
    pub path_alias: String,
    /// Relative, "/"-separated directory paths the user checked in the folder-tree picker. Each
    /// checked folder (and everything under it) is walked in full; root-level loose files are always
    /// included. Replaces the former `depth` slider (M8, HANDOVER §A2.1).
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// An immediate child directory of a scanned path — drives one node of the folder-tree picker.
#[derive(Debug, Clone, Serialize)]
pub struct SubdirEntry {
    pub name: String,
    /// Absolute path on disk (handed back so the frontend can lazily expand this node).
    pub path: String,
    /// True if this directory itself contains at least one sub-directory (drives the ▶ expander).
    pub has_children: bool,
}

#[tauri::command]
pub async fn scan_directory(
    opts: ScanOptions,
    store: State<'_, DataStore>,
) -> CmdResult<Collection> {
    scan_directory_inner(opts, store.inner()).await
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
        // visibility below (alongside notes).
        visibility: Visibility::Public,
        last_updated: chrono::Utc::now(),
        listing,
    };

    // Preserve per-item notes AND the prior visibility from the existing draft (rescan scenario) —
    // a rescan must never silently flip a Private collection back to Public (that would re-publish
    // privately-marked data on the public path next publish).
    if let Ok(Some(prev)) = store.load_collection_draft(&collection.slug) {
        let notes = collect_notes(&prev.listing, "");
        collection.listing = apply_notes(collection.listing, &notes, "");
        collection.visibility = prev.visibility;
    }

    store.save_collection_draft(&collection).map_err(cmd_err)?;

    // Persist the root path so the transfer server can find files on disk.
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
) -> CmdResult<()> {
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;
    store.delete_collection(safe_slug).map_err(cmd_err)
}

#[tauri::command]
pub async fn get_collections(store: State<'_, DataStore>) -> CmdResult<Vec<CollectionEntry>> {
    // Every collection is a local draft; `published` reflects whether a listing was published.
    let mut entries: Vec<CollectionEntry> = Vec::new();
    for slug in store.list_collection_slugs().map_err(cmd_err)? {
        if let Ok(Some(col)) = store.load_collection_draft(&slug) {
            let published = store.is_published(&slug);
            entries.push(CollectionEntry { collection: col, published });
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
) -> Result<(), String> {
    let listing_json = prepare_listing(slug, store)?;

    // Visibility gate: a Private collection takes the sealed, per-recipient path and never touches
    // the browse-key or the public teaser.
    let visibility = store
        .load_collection_draft(slug)
        .map_err(cmd_err)?
        .map(|c| c.visibility)
        .unwrap_or(Visibility::Public);
    if visibility == Visibility::Private {
        return publish_private_collection_inner(slug, store, identity, &listing_json).await;
    }

    let client = net::connect(identity, store).await.map_err(cmd_err)?;
    let published =
        publish_listing(&client, identity, slug, browse_key, &listing_json, LISTING_MAX_BYTES).await;
    client.disconnect().await;
    let published = published.map_err(cmd_err)?;

    // Local published marker (the "published" badge + content_types union).
    let marker = serde_json::json!({ "parts": published.parts }).to_string();
    store.save_published(slug, &marker).map_err(cmd_err)?;

    // M9: record the snapshot fingerprint of what we just published, so a later watch re-scan that
    // hashes equal is a no-op (the republish-storm guard) and a real change re-publishes exactly once.
    if let Ok(Some(col)) = store.load_collection_draft(slug) {
        let fp = hb_core::snapshot_fingerprint(&col.listing);
        let _ = store.save_snapshot_fingerprint(slug, &fp);
    }

    // Keep a published teaser's content_types current.
    if store.is_published("profile") {
        if let Some(mut profile) = store.load_profile_draft().map_err(cmd_err)? {
            profile.content_types = compute_content_types(store);
            store.save_profile_draft(&profile).map_err(cmd_err)?;
            let teaser = hb_core::event::Teaser {
                display_name: profile.display_name.clone(),
                bio: profile.bio.clone().unwrap_or_default(),
                tags: profile.tags.clone(),
                content_types: profile.content_types.clone(),
            };
            if let Ok(event) = hb_core::event::build_teaser(identity, &teaser) {
                if let Ok(client) = net::connect(identity, store).await {
                    let _ = client.publish(&event).await;
                    client.disconnect().await;
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = store.save_published("profile", &json);
                    }
                }
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
) -> Result<(), String> {
    let recipients = private_recipients(store)?;
    let events = hb_core::seal_private_listing(identity, &recipients, listing_json, now_secs())
        .map_err(cmd_err)?;

    let client = net::connect(identity, store).await.map_err(cmd_err)?;
    let res = hb_net::publish_private_listing(&client, &events).await;
    client.disconnect().await;
    res.map_err(cmd_err)?;

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
) -> CmdResult<()> {
    let (id_clone, browse_key) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key)
    };
    publish_collection_inner(&slug, &store, &id_clone, &browse_key).await
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
        } else if meta.is_file() && list_loose_files {
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

/// Enumerate the immediate child *directories* of `path` (sorted), each tagged with whether it has
/// sub-directories of its own (drives the picker's ▶ expander). Pure core behind `list_subdirs`.
pub(crate) fn list_subdirs_core(path: &str) -> anyhow::Result<Vec<SubdirEntry>> {
    let root = Path::new(path);
    anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
    let mut entries: Vec<SubdirEntry> = vec![];
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.metadata()?.is_dir() {
            continue;
        }
        let child_path = entry.path();
        entries.push(SubdirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            has_children: dir_has_subdir(&child_path),
            path: child_path.to_string_lossy().into_owned(),
        });
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(entries)
}

/// Cheap "does this directory contain at least one sub-directory?" probe (stops at the first hit).
/// An unreadable directory reports `false` rather than erroring — the expander simply won't show.
fn dir_has_subdir(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.filter_map(|e| e.ok())
        .any(|e| e.metadata().map(|m| m.is_dir()).unwrap_or(false))
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
    fn list_subdirs_returns_sorted_immediate_children_with_has_children() {
        let dir = tempfile::tempdir().unwrap();
        make_selective_tree(dir.path());

        let entries = list_subdirs_core(&dir.path().to_string_lossy()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        // immediate child DIRECTORIES only, sorted; loose files excluded.
        assert_eq!(names, vec!["a", "x"], "immediate child dirs, sorted, no files");
        let a = entries.iter().find(|e| e.name == "a").unwrap();
        assert!(a.has_children, "a has subdir b → expander shown");
        // a leaf dir reports no children.
        let leaf = list_subdirs_core(&dir.path().join("a").join("b").join("c").to_string_lossy()).unwrap();
        assert!(leaf.is_empty(), "c has no subdirs");
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
                },
                crate::store::Group {
                    name: "also".into(),
                    pubkeys: vec![a.npub()], // duplicate of `a` across groups → collapsed
                    modified_at: chrono::Utc::now(),
                    trusted: true,
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
