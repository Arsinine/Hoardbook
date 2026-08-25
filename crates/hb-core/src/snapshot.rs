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
//! **Where this digest DOES travel (security audit #25, QURATOR-123).** `collection_to_listing_json`
//! stamps it into listing meta, and `hb_net::truncate_listing` preserves top-level meta — so the
//! *public teaser event of a truncated Public collection carries it*, browse-key-encrypted but
//! decryptable by every share-code holder. It is therefore **not** a private digest, and it must
//! never cover content the truncation hides: for a truncated teaser the publish path re-stamps the
//! meta value with `teaser_fingerprint` (visible entries + elided count, below) so an observer with
//! a candidate tree cannot confirm-or-deny the hidden portion offline. The storm guard's persisted
//! sidecar (`snapshot_fingerprint.json`, never published) keeps using the full-tree
//! `snapshot_fingerprint`.
//!
//! **Layering note:** like `fingerprint` (the identity word+colour distinguisher), this is a
//! display/orchestration affordance, not a Nostr protocol primitive. It lives in `hb-core` so the
//! watch loop (`hb-app`) and any future tool share one derivation. It is a SHA-256 *content* hash,
//! unrelated to the human-comparable identity `Fingerprint` next door.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::DirectoryItem;

/// A deterministic content fingerprint of a collection's directory tree: the lowercase hex
/// SHA-256 of the tree's canonical JSON. `Serialize`/`Deserialize` so the watch loop can persist
/// the last-published value beside the listing and diff against it on the next re-scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFingerprint(pub String);

/// The one digest core both fingerprints share: canonicalize → serialize → SHA-256 over a
/// versioned frame (`tag ∥ elided count ∥ tree`). The `elided` count is folded into the hash so a
/// teaser digest commits to *how much* was hidden without ever seeing *what* was hidden — the
/// count is already public in the teaser's `total_items` meta, so this adds nothing an observer
/// doesn't have. `snapshot_fingerprint` is this core with `elided = 0`, so an untruncated listing's
/// teaser digest equals its full-tree digest (nothing is hidden; the publish-path stamp is a no-op
/// there and the byte-identical-untruncated-publish property survives).
///
/// The frame tag (`hb-snap-v1`) re-keys every digest at once. Owner ruling 2026-08-25 (QURATOR-123):
/// there are no users in the wild, so a digest-semantics change carries no migration burden.
fn digest_of(items: &[DirectoryItem], elided: u64) -> SnapshotFingerprint {
    let canonical = canonicalize(items);
    let json = serde_json::to_vec(&canonical).expect("a DirectoryItem tree always serializes");
    let mut hasher = Sha256::new();
    hasher.update(b"hb-snap-v1\x00");
    hasher.update(elided.to_string().as_bytes());
    hasher.update(b"\x00");
    hasher.update(&json);
    SnapshotFingerprint(hex::encode(hasher.finalize()))
}

/// Fingerprint a collection's directory tree — the **storm guard's** digest, over the WHOLE tree.
/// Pure and deterministic — the same *content* always hashes to the same value regardless of input
/// order, and any tree change (file/folder add, remove, rename, resize, format, or note edit)
/// changes it. Excludes nothing *within* the tree; excludes `last_updated` by construction (that
/// field lives on `Collection`, not on a `DirectoryItem`). Never publish this over a tree that has
/// a hidden portion — see [`teaser_fingerprint`] and the module docs (audit #25 / QURATOR-123).
///
/// The tree is **canonicalized** (recursively sorted by name, then type) before hashing, so the
/// storm guard does not fire on a mere reorder: `std::fs::read_dir` order is not guaranteed stable
/// across runs / filesystems, so hashing raw input order would yield spurious `Changed` verdicts on
/// an unchanged tree — the opposite storm (chorus: convergent Codex/Gemini/opencode finding).
pub fn snapshot_fingerprint(items: &[DirectoryItem]) -> SnapshotFingerprint {
    digest_of(items, 0)
}

/// The digest a **truncated teaser** may carry (security audit #25 / QURATOR-123): over the
/// VISIBLE (post-truncation) entries only, plus `elided` — the count of item nodes the truncation
/// dropped. It must never be computed over, or derivable from, the hidden entries: an observer
/// holding a candidate full tree must not be able to confirm-or-deny the hidden portion offline,
/// which is exactly what a full-tree digest in the public teaser allowed.
///
/// Same canonicalization as [`snapshot_fingerprint`] (readdir reorder is still a no-op). With
/// `elided == 0` (nothing hidden) this equals [`snapshot_fingerprint`] by construction.
pub fn teaser_fingerprint(items: &[DirectoryItem], elided: u64) -> SnapshotFingerprint {
    digest_of(items, elided)
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

    // ── QURATOR-123 / audit #25: the teaser digest must not confirm the hidden content ──────────

    /// The AC-2 discriminator: two collections that share a VISIBLE prefix but differ ONLY in their
    /// hidden entries must produce the SAME teaser digest. Under the old full-tree digest this was
    /// the confirmation oracle — an attacker hashed a candidate tree and compared it against the
    /// public teaser's meta value to confirm-or-deny its exact contents offline.
    #[test]
    fn teaser_digest_does_not_discriminate_hidden_entries() {
        let shared_visible = vec![file("shown-a.mkv", Some("1 GB")), file("shown-b.mkv", Some("2 GB"))];
        let hidden_one = (0..50)
            .map(|i| file(&format!("hidden-one-{i:03}.mkv"), Some("3 GB")))
            .collect::<Vec<_>>();
        let hidden_two = (0..50)
            .map(|i| file(&format!("hidden-two-{i:03}-DIFFERENT.mkv"), Some("9 GB")))
            .collect::<Vec<_>>();
        let elided = 50u64;
        // Same visible prefix, same elided count, entirely different hidden halves.
        let a = teaser_fingerprint(&shared_visible, elided);
        let b = teaser_fingerprint(&shared_visible, elided);
        assert_eq!(a, b, "two trees identical in their visible portion and elided count must share one teaser digest");
        // And the old oracle is gone: neither hidden half's FULL-tree digest matches the teaser's.
        let mut full_one = shared_visible.clone();
        full_one.extend(hidden_one.clone());
        let mut full_two = shared_visible.clone();
        full_two.extend(hidden_two);
        assert_ne!(
            snapshot_fingerprint(&full_one).0,
            a.0,
            "the teaser digest must not equal the full-tree digest of the tree it hides"
        );
        assert_ne!(snapshot_fingerprint(&full_two).0, a.0);
        // Sanity on the fixtures themselves: the two full trees really are different trees (the
        // assertion above is vacuous if the fixtures coincide).
        assert_ne!(snapshot_fingerprint(&full_one), snapshot_fingerprint(&full_two));
    }

    /// The teaser digest still detects changes IN THE VISIBLE PORTION and in the elided COUNT —
    /// it is a real digest of what the teaser ships, not a constant.
    #[test]
    fn teaser_digest_tracks_the_visible_portion_and_elided_count() {
        let a = teaser_fingerprint(&[file("shown.mkv", Some("1 GB"))], 40);
        assert_ne!(a, teaser_fingerprint(&[file("edited.mkv", Some("1 GB"))], 40), "a visible rename must change it");
        assert_ne!(a, teaser_fingerprint(&[file("shown.mkv", Some("2 GB"))], 40), "a visible resize must change it");
        assert_ne!(a, teaser_fingerprint(&[file("shown.mkv", Some("1 GB"))], 41), "a different elided count must change it");
        // Canonicalization holds for the teaser digest too, at any elided count.
        let l = teaser_fingerprint(&[file("a.mkv", None), file("b.mkv", None)], 7);
        let r = teaser_fingerprint(&[file("b.mkv", None), file("a.mkv", None)], 7);
        assert_eq!(l, r, "a readdir reorder of the visible portion is still the same teaser");
    }

    /// Nothing hidden ⇒ the teaser digest IS the full-tree digest: an untruncated listing's
    /// stamp is a no-op, so the publish path leaves such listings byte-identical.
    #[test]
    fn teaser_digest_with_nothing_hidden_equals_the_full_tree_digest() {
        let tree = vec![file("only.mkv", Some("1 GB"))];
        assert_eq!(teaser_fingerprint(&tree, 0), snapshot_fingerprint(&tree));
    }
}
