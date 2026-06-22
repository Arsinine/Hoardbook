//! Snapshot fingerprint — the **republish storm guard** (spec §Collection Manager → Snapshot
//! trigger; Decision #17). A pure content hash of a collection's directory tree, compared before
//! every auto-republish: a re-scan that hashes equal to the last published snapshot produces
//! **zero** relay writes (no event, no signature, no timing signal).
//!
//! The fingerprint is taken over the **directory tree only** (`&[DirectoryItem]`) — names, types,
//! sizes, formats, notes, and structure. It deliberately excludes the collection's `last_updated`
//! timestamp (which `chrono::Utc::now()` bumps on every scan), so a re-scan that finds the same
//! files is recognised as unchanged. A genuine file add / remove / rename / resize / note edit
//! changes the tree and therefore the fingerprint, triggering exactly one republish.
//!
//! **Layering note:** like `fingerprint` (the identity word+colour distinguisher), this is a
//! display/orchestration affordance, not a Nostr protocol primitive — it is never embedded in an
//! event. It lives in `hb-core` so the watch loop (`hb-app`) and any future tool share one
//! derivation. It is a SHA-256 *content* hash, unrelated to the human-comparable identity
//! `Fingerprint` next door.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::DirectoryItem;

/// A deterministic content fingerprint of a collection's directory tree: the lowercase hex
/// SHA-256 of the tree's canonical JSON. `Serialize`/`Deserialize` so the watch loop can persist
/// the last-published value beside the listing and diff against it on the next re-scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFingerprint(pub String);

/// Fingerprint a collection's directory tree. Pure and deterministic — the same *content* always
/// hashes to the same value regardless of input order, and any tree change (file/folder add, remove,
/// rename, resize, format, or note edit) changes it. Excludes nothing *within* the tree; excludes
/// `last_updated` by construction (that field lives on `Collection`, not on a `DirectoryItem`).
///
/// The tree is **canonicalized** (recursively sorted by name, then type) before hashing, so the
/// storm guard does not fire on a mere reorder: `std::fs::read_dir` order is not guaranteed stable
/// across runs / filesystems, so hashing raw input order would yield spurious `Changed` verdicts on
/// an unchanged tree — the opposite storm (chorus: convergent Codex/Gemini/opencode finding).
pub fn snapshot_fingerprint(items: &[DirectoryItem]) -> SnapshotFingerprint {
    let canonical = canonicalize(items);
    let json = serde_json::to_vec(&canonical).expect("a DirectoryItem tree always serializes");
    let digest = Sha256::digest(&json);
    SnapshotFingerprint(hex::encode(digest))
}

/// Return a copy of the tree with siblings sorted by `(name, item_type)` at every level — a stable
/// total order (siblings on a filesystem have unique names), so two semantically-identical trees
/// canonicalize to byte-identical JSON.
fn canonicalize(items: &[DirectoryItem]) -> Vec<DirectoryItem> {
    let mut out: Vec<DirectoryItem> = items
        .iter()
        .map(|it| DirectoryItem { children: canonicalize(&it.children), ..it.clone() })
        .collect();
    out.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| (a.item_type == crate::types::ItemType::Folder).cmp(&(b.item_type == crate::types::ItemType::Folder)))
    });
    out
}

/// True iff two fingerprints are equal — i.e. the tree is unchanged since the last publish, so the
/// auto-republish must be a **no-op** (the storm + metadata-churn guard).
pub fn unchanged_since(prev: &SnapshotFingerprint, next: &SnapshotFingerprint) -> bool {
    prev == next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn file(name: &str, size: Option<&str>) -> DirectoryItem {
        DirectoryItem {
            name: name.into(),
            item_type: ItemType::File,
            size: size.map(|s| s.into()),
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        }
    }

    fn folder(name: &str, children: Vec<DirectoryItem>) -> DirectoryItem {
        DirectoryItem {
            name: name.into(),
            item_type: ItemType::Folder,
            size: None,
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children,
        }
    }

    #[test]
    fn identical_trees_fingerprint_equal_and_are_unchanged() {
        // The storm guard's core: re-scanning the same files yields the same fingerprint, so the
        // watch loop produces zero relay writes.
        let a = vec![folder("films", vec![file("Ran.mkv", Some("12 GB"))]), file("readme.txt", None)];
        let b = vec![folder("films", vec![file("Ran.mkv", Some("12 GB"))]), file("readme.txt", None)];
        assert_eq!(snapshot_fingerprint(&a), snapshot_fingerprint(&b));
        assert!(unchanged_since(&snapshot_fingerprint(&a), &snapshot_fingerprint(&b)));
    }

    #[test]
    fn added_file_changes_fingerprint() {
        let before = vec![file("a.mkv", None)];
        let after = vec![file("a.mkv", None), file("b.mkv", None)];
        assert_ne!(snapshot_fingerprint(&before), snapshot_fingerprint(&after));
        assert!(!unchanged_since(&snapshot_fingerprint(&before), &snapshot_fingerprint(&after)));
    }

    #[test]
    fn removed_file_changes_fingerprint() {
        let before = vec![file("a.mkv", None), file("b.mkv", None)];
        let after = vec![file("a.mkv", None)];
        assert_ne!(snapshot_fingerprint(&before), snapshot_fingerprint(&after));
    }

    #[test]
    fn renamed_file_changes_fingerprint() {
        let before = vec![file("a.mkv", None)];
        let after = vec![file("a-final.mkv", None)];
        assert_ne!(snapshot_fingerprint(&before), snapshot_fingerprint(&after));
    }

    #[test]
    fn resized_file_changes_fingerprint() {
        // A file growing/shrinking is a content change worth republishing.
        let before = vec![file("a.mkv", Some("1 GB"))];
        let after = vec![file("a.mkv", Some("2 GB"))];
        assert_ne!(snapshot_fingerprint(&before), snapshot_fingerprint(&after));
    }

    #[test]
    fn note_edit_changes_fingerprint() {
        // Notes are part of the published listing, so editing one is a meaningful change.
        let mut after = file("a.mkv", None);
        after.note = Some("Director's cut".into());
        assert_ne!(snapshot_fingerprint(&[file("a.mkv", None)]), snapshot_fingerprint(&[after]));
    }

    #[test]
    fn nested_change_changes_fingerprint() {
        // A change deep in the tree must propagate to the top-level fingerprint.
        let before = vec![folder("s1", vec![file("ep1.mkv", None)])];
        let after = vec![folder("s1", vec![file("ep1.mkv", None), file("ep2.mkv", None)])];
        assert_ne!(snapshot_fingerprint(&before), snapshot_fingerprint(&after));
    }

    #[test]
    fn reorder_does_not_change_fingerprint_canonicalized() {
        // Same content in a different sibling order is the SAME tree — the fingerprint canonicalizes,
        // so a readdir reorder doesn't fire a spurious republish (the convergent chorus fix). This is
        // the storm guard's correctness premise: equal content ⇒ equal fingerprint, order-independent.
        let a = vec![file("a.mkv", None), file("b.mkv", None)];
        let b = vec![file("b.mkv", None), file("a.mkv", None)];
        assert_eq!(snapshot_fingerprint(&a), snapshot_fingerprint(&b));
    }

    #[test]
    fn nested_reorder_also_canonicalizes() {
        // Canonicalization is recursive — a reorder deep in the tree is also a no-op.
        let a = vec![folder("s", vec![file("ep1.mkv", None), file("ep2.mkv", None)])];
        let b = vec![folder("s", vec![file("ep2.mkv", None), file("ep1.mkv", None)])];
        assert_eq!(snapshot_fingerprint(&a), snapshot_fingerprint(&b));
    }

    #[test]
    fn empty_tree_is_stable() {
        assert_eq!(snapshot_fingerprint(&[]), snapshot_fingerprint(&[]));
    }
}
