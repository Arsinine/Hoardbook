use std::path::Path;
use hb_core::{
    DocType, SignedEnvelope,
    types::{Collection, DirectoryItem, ItemType},
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::DataStore,
    SharedIdentity, SharedRelay,
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
    let (listing, total_bytes) = scan_recursive(root, depth, 0, &opts.exclude).map_err(cmd_err)?;
    let item_count = count_items(&listing);
    let est_size = if total_bytes > 0 { Some(format_size(total_bytes)) } else { None };

    let collection = Collection {
        slug,
        path_alias: opts.path_alias,
        description: None,
        item_count,
        est_size,
        total_bytes,
        content_type: vec![],
        last_updated: chrono::Utc::now(),
        listing,
    };

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

/// Update the editable metadata fields (description, content_type) of a collection draft.
#[tauri::command]
pub async fn update_collection_meta(
    slug: String,
    description: Option<String>,
    content_type: Vec<String>,
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
    col.content_type = content_type;
    store.save_collection_draft(&col).map_err(cmd_err)
}

#[tauri::command]
pub async fn publish_collection(
    slug: String,
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let guard = identity.read().await;
    let kp = guard
        .as_ref()
        .ok_or("No identity loaded. Generate a keypair first.")?;

    // Re-derive the slug from what was stored rather than trusting the caller directly.
    // This prevents a crafted slug like "../identity/keypair" from escaping the
    // collections/ directory.
    let safe_slug = is_valid_slug(&slug)
        .then_some(slug.as_str())
        .ok_or("Invalid collection slug")?;

    let draft_path = store.collection_draft_path(safe_slug);
    if !draft_path.exists() {
        return Err(format!("No draft found for collection '{safe_slug}'"));
    }

    let bytes = std::fs::read(&draft_path).map_err(cmd_err)?;
    let collection: Collection = serde_json::from_slice(&bytes).map_err(cmd_err)?;

    let envelope = SignedEnvelope::create(kp, DocType::Collection, &collection).map_err(cmd_err)?;
    store.save_collection_signed(safe_slug, &envelope).map_err(cmd_err)?;
    relay.publish("collection", &envelope).await.map_err(cmd_err)
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
    exclude: &[String],
) -> anyhow::Result<(Vec<DirectoryItem>, u64)> {
    let mut items = vec![];
    let mut total_bytes: u64 = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_excluded(&name, exclude) {
            continue;
        }
        let meta = entry.metadata()?;
        let path = entry.path();
        if meta.is_dir() {
            let (children, sub_bytes) = if current_depth + 1 < max_depth {
                scan_recursive(&path, max_depth, current_depth + 1, exclude)?
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

fn is_excluded(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| {
        let pat = pat.trim_end_matches('/');
        if pat.starts_with('*') {
            name.ends_with(pat.trim_start_matches('*'))
        } else if pat.ends_with('*') {
            name.starts_with(pat.trim_end_matches('*'))
        } else {
            name == pat
        }
    })
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
}
