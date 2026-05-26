use std::path::Path;
use globset::{Glob, GlobSetBuilder};
use hb_core::{
    DocType, SignedEnvelope,
    types::{Collection, DirectoryItem, ItemType},
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    commands::profile::compute_content_types,
    error::{CmdResult, cmd_err},
    store::DataStore,
    SharedIdentity,
};

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
    pub depth: u32,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[tauri::command]
pub async fn scan_directory(
    opts: ScanOptions,
    store: State<'_, DataStore>,
) -> CmdResult<Collection> {
    let root = Path::new(&opts.path);
    if !root.is_dir() {
        return Err(format!("{} is not a directory", opts.path));
    }

    let depth = opts.depth.min(10);
    let slug = Collection::slug_from_alias(&opts.path_alias);
    let globs = build_glob_set(&opts.exclude);
    let (listing, total_bytes) = scan_recursive(root, depth, 0, &globs, "").map_err(cmd_err)?;
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
        last_updated: chrono::Utc::now(),
        listing,
    };

    // Preserve per-item notes from the existing draft (rescan scenario).
    if let Ok(Some(prev)) = store.load_collection_draft(&collection.slug) {
        let notes = collect_notes(&prev.listing, "");
        collection.listing = apply_notes(collection.listing, &notes, "");
    }

    store.save_collection_draft(&collection).map_err(cmd_err)?;

    // Persist the root path so the transfer server can find files on disk.
    let mut share = store
        .load_share_settings(&collection.slug)
        .map_err(cmd_err)?
        .unwrap_or_default();
    share.root_path = Some(opts.path.clone());
    store.save_share_settings(&collection.slug, &share).map_err(cmd_err)?;

    Ok(collection)
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
    // Signed (published) collections.
    let envelopes = store.list_collections().map_err(cmd_err)?;
    let mut entries: Vec<CollectionEntry> = envelopes
        .into_iter()
        .filter_map(|env| env.parse_payload::<Collection>().ok())
        .map(|c| CollectionEntry { collection: c, published: true })
        .collect();

    // Draft-only collections (scanned but not yet published).
    let draft_slugs = store.list_draft_only_slugs().map_err(cmd_err)?;
    for slug in draft_slugs {
        if let Ok(Some(col)) = store.load_collection_draft(&slug) {
            entries.push(CollectionEntry { collection: col, published: false });
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

/// Core publish logic extracted for testability.
/// Validates slug, enforces content_types, signs, and updates profile.
pub(crate) fn publish_collection_inner(
    slug: &str,
    store: &DataStore,
    kp: &hb_core::HoardbookKeypair,
) -> Result<(), String> {
    let safe_slug = is_valid_slug(slug)
        .then_some(slug)
        .ok_or("Invalid collection slug")?;

    let draft_path = store.collection_draft_path(safe_slug);
    if !draft_path.exists() {
        return Err(format!("No draft found for collection '{safe_slug}'"));
    }

    let bytes = std::fs::read(&draft_path).map_err(cmd_err)?;
    let collection: Collection = serde_json::from_slice(&bytes).map_err(cmd_err)?;

    if collection.content_types.is_empty() {
        return Err("At least one content type is required before publishing a collection.".into());
    }

    let envelope = SignedEnvelope::create(kp, DocType::Collection, &collection).map_err(cmd_err)?;
    store.save_collection_signed(safe_slug, &envelope).map_err(cmd_err)?;

    // Recompute and re-sign profile content_types if a signed profile exists.
    if let Some(mut profile) = store.load_profile_draft().map_err(cmd_err)? {
        profile.content_types = compute_content_types(store);
        store.save_profile_draft(&profile).map_err(cmd_err)?;
        if store.load_profile_signed().map_err(cmd_err)?.is_some() {
            let prof_env = SignedEnvelope::create(kp, DocType::Profile, &profile).map_err(cmd_err)?;
            store.save_profile_signed(&prof_env).map_err(cmd_err)?;
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn publish_collection(
    slug: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<()> {
    let guard = identity.read().await;
    let kp = guard
        .as_ref()
        .ok_or("No identity loaded. Generate a keypair first.")?;
    publish_collection_inner(&slug, &store, kp)
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

    // Prefer the published (signed) version; fall back to draft.
    let collection: Collection = if let Some(env) = store
        .list_collections()
        .map_err(cmd_err)?
        .into_iter()
        .find(|e| e.parse_payload::<Collection>().ok().map_or(false, |c| c.slug == safe_slug))
    {
        env.parse_payload().map_err(cmd_err)?
    } else {
        store
            .load_collection_draft(safe_slug)
            .map_err(cmd_err)?
            .ok_or_else(|| format!("Collection '{safe_slug}' not found"))?
    };

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

/// A valid slug contains only ASCII alphanumerics and hyphens.
/// This is enforced before constructing any file paths from slug values,
/// preventing path traversal attacks (e.g., "../identity/keypair").
pub(crate) fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ---------------------------------------------------------------------------
// Filesystem scanner
// ---------------------------------------------------------------------------

fn scan_recursive(
    dir: &Path,
    max_depth: u32,
    current_depth: u32,
    exclude: &globset::GlobSet,
    rel_prefix: &str,
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    let mut items = vec![];
    let mut total_bytes: u64 = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel_path = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{rel_prefix}/{name}")
        };
        if exclude.is_match(&rel_path) {
            continue;
        }
        let meta = entry.metadata()?;
        let path = entry.path();
        if meta.is_dir() {
            let (children, sub_bytes) = if current_depth + 1 < max_depth {
                scan_recursive(&path, max_depth, current_depth + 1, exclude, &rel_path)?
            } else {
                (vec![], 0)
            };
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
        } else if meta.is_file() {
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

fn count_items(items: &[DirectoryItem]) -> u64 {
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

    #[test]
    fn depth_limit_enforced() {
        let dir = tempfile::tempdir().unwrap();
        make_dir_tree(dir.path());

        // depth=3: root(0) → level1(1) → level2(2) → level3 NOT recursed (2+1 < 3 is false).
        let (items, _) = scan_recursive(dir.path(), 3, 0, &empty_globs(), "").unwrap();
        let json = serde_json::to_string(&items).unwrap();
        assert!(json.contains("top.txt"), "level1 files must be present");
        assert!(json.contains("mid.txt"), "level2 files must be present");
        assert!(!json.contains("deep.txt"), "level3 files must be absent at depth=3");

        // depth=10: all levels included.
        let (items_full, _) = scan_recursive(dir.path(), 10, 0, &empty_globs(), "").unwrap();
        let json_full = serde_json::to_string(&items_full).unwrap();
        assert!(json_full.contains("deep.txt"), "full depth must include level3");
    }

    #[test]
    fn exclude_glob_applied() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("movie.mkv"), b"x").unwrap();
        std::fs::write(dir.path().join("movie.nfo"), b"x").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();

        let globs = build_glob_set(&["*.nfo".to_string()]);
        let (items, _) = scan_recursive(dir.path(), 1, 0, &globs, "").unwrap();
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
        let (items, _) = scan_recursive(dir.path(), 10, 0, &globs, "").unwrap();
        let json = serde_json::to_string(&items).unwrap();
        assert!(!json.contains("ep1.nfo"), "nested *.nfo must be excluded by **/*.nfo glob");
        assert!(json.contains("ep1.mkv"), "mkv must remain");
    }

    #[test]
    fn item_count_accurate() {
        let dir = tempfile::tempdir().unwrap();
        make_dir_tree(dir.path()); // root.txt + level1/(top.txt + level2/(mid.txt + level3/(deep.txt)))
        let (items, _) = scan_recursive(dir.path(), 10, 0, &empty_globs(), "").unwrap();
        let total = count_items(&items);
        // Items: root.txt, level1(dir), top.txt, level2(dir), mid.txt, level3(dir), deep.txt = 7
        assert_eq!(total, 7, "expected 7 items (4 files + 3 dirs), got {total}");
    }

    #[test]
    fn regenerate_preserves_notes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("film.mkv"), b"x").unwrap();
        std::fs::write(dir.path().join("extra.txt"), b"x").unwrap();

        // First scan — simulate a note added to film.mkv.
        let (mut items, _) = scan_recursive(dir.path(), 1, 0, &empty_globs(), "").unwrap();
        for item in &mut items {
            if item.name == "film.mkv" {
                item.note = Some("Director's cut".into());
            }
        }

        // Second scan — fresh, no notes yet.
        let (new_items, _) = scan_recursive(dir.path(), 1, 0, &empty_globs(), "").unwrap();
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

        let (mut items, _) = scan_recursive(dir.path(), 10, 0, &empty_globs(), "").unwrap();
        // Add note only to the root readme.txt.
        for item in &mut items {
            if item.name == "readme.txt" {
                item.note = Some("root note".into());
            }
        }

        let (new_items, _) = scan_recursive(dir.path(), 10, 0, &empty_globs(), "").unwrap();
        let notes = collect_notes(&items, "");
        let merged = apply_notes(new_items, &notes, "");

        let root_readme = merged.iter().find(|i| i.name == "readme.txt").unwrap();
        assert_eq!(root_readme.note.as_deref(), Some("root note"));

        let subdir = merged.iter().find(|i| i.name == "subdir").unwrap();
        let sub_readme = subdir.children.iter().find(|i| i.name == "readme.txt").unwrap();
        assert!(sub_readme.note.is_none(), "subdirectory readme must not inherit root note");
    }

    // ── T16 acceptance tests ─────────────────────────────────────────────────

    fn make_published_collection(store: &DataStore, kp: &hb_core::HoardbookKeypair, slug: &str, content_types: Vec<String>) {
        let col = hb_core::Collection {
            slug: slug.to_string(),
            path_alias: slug.to_string(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types,
            tags: vec![],
            languages: vec![],
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        let env = hb_core::SignedEnvelope::create(kp, hb_core::DocType::Collection, &col).unwrap();
        store.save_collection_signed(slug, &env).unwrap();
    }

    fn make_collection_draft(store: &DataStore, slug: &str, content_types: Vec<String>) {
        let col = hb_core::Collection {
            slug: slug.to_string(),
            path_alias: slug.to_string(),
            description: None,
            item_count: 1,
            est_size: None,
            content_types,
            tags: vec![],
            languages: vec![],
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
    }

    #[test]
    fn publish_signs_with_current_key() {
        use hb_core::{HoardbookKeypair, SignedEnvelope, DocType};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let kp = HoardbookKeypair::generate();

        make_collection_draft(&store, "my-films", vec!["video".to_string()]);

        // Call the real publish path.
        publish_collection_inner("my-films", &store, &kp).unwrap();

        let signed_path = store.collection_signed_path("my-films");
        assert!(signed_path.exists(), "signed.json must be written to disk");

        let bytes = std::fs::read(&signed_path).unwrap();
        let loaded: SignedEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert!(loaded.verify().is_ok(), "envelope.verify() must return Ok(())");
        assert_eq!(loaded.doc_type, DocType::Collection);
    }

    #[test]
    fn publish_rejects_invalid_slug() {
        use hb_core::HoardbookKeypair;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let kp = HoardbookKeypair::generate();

        let err = publish_collection_inner("../evil", &store, &kp).unwrap_err();
        assert!(err.contains("Invalid collection slug"), "got: {err}");
    }

    #[test]
    fn publish_rejects_empty_content_types() {
        use hb_core::HoardbookKeypair;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let kp = HoardbookKeypair::generate();

        make_collection_draft(&store, "no-types", vec![]);

        let err = publish_collection_inner("no-types", &store, &kp).unwrap_err();
        assert!(err.contains("content type"), "got: {err}");
    }

    #[test]
    fn profile_content_types_updated_after_publish() {
        use hb_core::{HoardbookKeypair, SignedEnvelope, DocType, Profile};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let kp = HoardbookKeypair::generate();

        // Create and sign two existing collections via the helper.
        make_published_collection(&store, &kp, "films", vec!["video".to_string()]);

        // Create a profile draft + signed profile with empty content_types.
        let profile = Profile {
            display_name: "Test".to_string(),
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
            updated: chrono::Utc::now(),
        };
        store.save_profile_draft(&profile).unwrap();
        let prof_env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        store.save_profile_signed(&prof_env).unwrap();

        // Add a second collection and publish via the real command path.
        make_collection_draft(&store, "books", vec!["text".to_string()]);
        publish_collection_inner("books", &store, &kp).unwrap();

        // Profile must now be re-signed with merged content_types.
        let reloaded_env = store.load_profile_signed().unwrap().unwrap();
        assert!(reloaded_env.verify().is_ok(), "re-signed profile must verify");
        let reloaded: Profile = reloaded_env.parse_payload().unwrap();
        let mut expected = vec!["text".to_string(), "video".to_string()];
        expected.sort();
        let mut actual = reloaded.content_types.clone();
        actual.sort();
        assert_eq!(actual, expected, "profile content_types must be union of all collections");
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
