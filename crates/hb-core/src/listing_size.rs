//! v5 size rule (QURATOR-344): turn a listing's human-formatted size strings back into bytes and
//! sum them. `DirectoryItem::size` is a display string produced by hb-app's `format_size`
//! (`"{bytes} B"`, else `"{:.1} KB|MB|GB"`, 1024-based) — the listing carries NO raw byte counts.
//! The parse is the exact inverse of those units; everything ≥ 1 KB was already rounded to one
//! decimal on emit, so the result is an approximation of the true byte count, bounded by the last
//! displayed decimal place — **≤ ~5% per entry**. Good faith is assumed (spec §Honesty): this is
//! a cost signal for the size-gated read rule, not a security boundary.

use crate::types::{DirectoryItem, ItemType};

/// Parse one size string exactly as hb-app's `format_size` emits it — `"512 B"`, `"1.5 KB"`,
/// `"1.2 GB"` — back to an approximate byte count (truncated). Unparseable, unknown unit, or
/// missing/empty ⇒ 0: fail-closed for the read rule (an odd string reads as nothing, never as
/// something big).
pub fn parse_size_bytes(s: &str) -> u64 {
    let (num, unit) = match s.trim().rsplit_once(' ') {
        Some(parts) => parts,
        None => return 0,
    };
    let mult = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        _ => return 0,
    };
    match num.trim().parse::<f64>() {
        Ok(n) if n.is_finite() && n >= 0.0 => (n * mult) as u64,
        _ => 0,
    }
}

/// Sum of file-leaf sizes across a listing tree. **Folders contribute nothing**: only
/// `ItemType::File` items are summed; a `Folder`'s own `size` (if one was ever set) is ignored —
/// summing it would double-count everything under it. Absent/unparseable sizes count 0.
pub fn sum_listing_bytes(items: &[DirectoryItem]) -> u64 {
    items
        .iter()
        .map(|item| match item.item_type {
            ItemType::File => parse_size_bytes(item.size.as_deref().unwrap_or("")),
            ItemType::Folder => sum_listing_bytes(&item.children),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: Option<&str>) -> DirectoryItem {
        DirectoryItem {
            name: name.into(),
            item_type: ItemType::File,
            size: size.map(Into::into),
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
    fn parse_size_bytes_golden_units_and_garbage() {
        // Golden inputs across every unit `format_size` emits, plus the garbage ⇒ 0 rule.
        //
        // P-10 mutation: in parse_size_bytes, change `"KB" => 1024.0` to `"KB" => 1.0` — the
        // "1.5 KB" assert must RED (1536 → 1).
        assert_eq!(parse_size_bytes("0 B"), 0);
        assert_eq!(parse_size_bytes("1023 B"), 1023);
        assert_eq!(parse_size_bytes("512 B"), 512);
        assert_eq!(parse_size_bytes("1.5 KB"), 1536);
        assert_eq!(parse_size_bytes("2 MB"), 2_097_152);
        assert_eq!(parse_size_bytes("1.2 GB"), 1_288_490_188, "truncated from 1288490188.8");
        // Garbage ⇒ 0 (fail-closed): no unit, unknown unit, negative, non-numeric, no space.
        assert_eq!(parse_size_bytes(""), 0);
        assert_eq!(parse_size_bytes("nonsense"), 0);
        assert_eq!(parse_size_bytes("12 XB"), 0);
        assert_eq!(parse_size_bytes("-3 KB"), 0);
        assert_eq!(parse_size_bytes("1.5KB"), 0, "format_size always emits a space");
    }

    #[test]
    fn sum_listing_bytes_sums_file_leaves_recursively_and_ignores_folder_sizes() {
        // P-10 mutation: in sum_listing_bytes, change the `ItemType::Folder` arm to also add
        // `parse_size_bytes(item.size.as_deref().unwrap_or(""))` — the assert must RED via the
        // sized folder's own "9 GB".
        let mut sized_folder = folder("sized", vec![file("d.txt", Some("10 B"))]);
        sized_folder.size = Some("9 GB".into());
        let tree = vec![
            file("a.txt", Some("1.5 KB")),
            folder(
                "movies",
                vec![
                    file("b.mkv", Some("1.2 GB")),
                    folder("sub", vec![file("c.txt", Some("1023 B"))]),
                ],
            ),
            folder("empty", vec![]),
            sized_folder,
        ];
        assert_eq!(
            sum_listing_bytes(&tree),
            1536 + 1_288_490_188 + 1023 + 10,
            "nested file leaves summed; folder's own size ignored"
        );
    }
}
