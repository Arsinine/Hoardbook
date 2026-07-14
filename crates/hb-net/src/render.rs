//! The partial-listing **render model** (M3): turn a set of fetched + decrypted listing payloads
//! into a directory tree annotated with **"K of N folders available"** when parts are missing.
//!
//! This is the lenient sibling of [`crate::split::restitch_listing`]. Restitch is all-or-nothing —
//! it *errors* on a missing part because the caller wanted the whole tree. The browse UI instead
//! wants to render whatever arrived and tell the user what's missing (spec §Data Model: "N of M
//! folders available on loss"; AB8 withhold). So `render_listing` treats a **missing** expected
//! part as graceful degradation (reported in `missing`), while a **foreign / duplicate /
//! out-of-range** part — something a hostile relay injected that the index never named — is a hard
//! rejection with a reason (it signals tampering, not loss). An index claiming more than
//! [`MAX_LISTING_PARTS`] is refused before any allocation (a read-side DoS cap the write-side
//! budget can't bound).
//!
//! M13 adds the v2 (depth-recursive, `parts_v: 2`) sibling of each of the above: `render_v2` slots
//! parts by sha256 (like [`crate::split::restitch_v2`]) rather than position, so an unreferenced
//! payload is silently ignored (the spec's stale-part rule) instead of a hard "foreign part"
//! rejection. Loss is still lenient — a slot that never arrived is `missing`, not an error — with
//! one refinement the tree shape demands: grafting a present part whose declared `mount` fails to
//! resolve is folded into `missing` too **only when something else is already known missing**
//! (an absent ancestor naturally strands its descendants); if nothing else is missing, an
//! unresolvable mount is a hostile/corrupt index, not loss, and stays a hard error.

use serde_json::{Map, Value};

use crate::error::NetError;
use crate::split::{graft, index_slots, parse_content_part_v2, parts_version, sha256_hex};

/// Read-side cap on the part count an index may claim. A hostile relay can serve an index
/// asserting an enormous `parts`, so we refuse it before collecting/allocating.
pub const MAX_LISTING_PARTS: usize = 4096;

/// A rendered listing tree, possibly partial.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedListing {
    /// Top-level metadata preserved from the listing (slug, content_types, …) — `entries` removed.
    pub meta: Map<String, Value>,
    /// The folder/file entries from the parts that actually arrived, in index order.
    pub entries: Vec<Value>,
    /// Total parts the index says exist (1 for an unsplit listing).
    pub parts_total: usize,
    /// Parts present (K). `parts_present == parts_total` ⇔ the tree is complete.
    pub parts_present: usize,
    /// Indices of the parts that did not arrive (empty when complete).
    pub missing: Vec<usize>,
}

impl RenderedListing {
    /// Whether every part is present. (The human-facing "K of N folders available" *string* is the
    /// UI's concern — see `ui/src/lib/browse-view.ts::availabilityBadge`; this data struct exposes
    /// the counts, not display text.)
    pub fn complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Render fetched + decrypted listing payloads into a (possibly partial) tree.
///
/// **The caller must pass payloads from signature-verified + decrypted events** (e.g. via
/// `hb_core::event::parse_listing_event`) — like restitch, this trusts the JSON it is given for
/// *provenance* and only validates *shape*.
pub fn render_listing(payloads: &[String]) -> Result<RenderedListing, NetError> {
    if payloads.is_empty() {
        return Err(NetError::Split("no listing payloads to render".into()));
    }
    // devtest #7: a plain unsplit listing (metadata + `entries`, no split/part/parts_v markers) is the
    // authoritative whole listing for its d-tag — a truncated paywall teaser, or an unsplit collection.
    // When it arrives ALONGSIDE stray `#partN` payloads (orphans a relay still serves after a
    // previously-split collection was republished as one event), those parts are stale: render the
    // single and ignore the rest, rather than routing to the split reader and finding no valid index.
    if payloads.len() > 1 {
        if let Some(single) = payloads.iter().find(|p| is_plain_unsplit(p)) {
            return render_v1(std::slice::from_ref(single));
        }
    }
    match parts_version(payloads)? {
        None | Some(1) => render_v1(payloads),
        Some(2) => render_v2(payloads),
        Some(v) => Err(NetError::Split(format!("unknown parts version {v}"))),
    }
}

/// True iff `json` is a plain, unsplit listing payload: a JSON object carrying `entries` and NONE of
/// the split/part markers (`split`, `part`, `parts_v`). A content part always carries `part`/`parts_v`
/// and an index carries `split`, so only a whole-listing event matches — making it the authoritative
/// render source when stale orphan parts share its family (devtest #7).
fn is_plain_unsplit(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    let Some(obj) = v.as_object() else {
        return false;
    };
    obj.contains_key("entries")
        && !obj.contains_key("split")
        && !obj.contains_key("part")
        && !obj.contains_key("parts_v")
}

/// v1 render (shipped, live on relays — absence of `parts_v`, or `parts_v: 1`): extracted verbatim
/// from the pre-M13 `render_listing`, plus one guard — a payload carrying a `parts_v` key (a v2
/// straggler mixed into what is otherwise a v1 family) is skipped outright rather than mis-parsed
/// into `plain_single`, which would wrongly reject an otherwise-valid v1 family.
fn render_v1(payloads: &[String]) -> Result<RenderedListing, NetError> {
    // Parse all payloads; locate the index (split == true) and the content parts.
    let mut index: Option<Map<String, Value>> = None;
    let mut content: Vec<(usize, usize, Vec<Value>)> = Vec::new();
    let mut plain_single: Option<Map<String, Value>> = None;
    for json in payloads {
        let v: Value = serde_json::from_str(json).map_err(|e| NetError::Split(e.to_string()))?;
        let obj = v.as_object().ok_or_else(|| NetError::Split("part is not an object".into()))?;
        if obj.contains_key("parts_v") {
            continue; // a v2 part in a v1 family — not this reader's business, ignore it
        }
        if obj.get("split") == Some(&Value::Bool(true)) {
            index = Some(obj.clone());
        } else if let (Some(p), Some(t)) =
            (obj.get("part").and_then(Value::as_u64), obj.get("parts").and_then(Value::as_u64))
        {
            let entries =
                obj.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
            content.push((p as usize, t as usize, entries));
        } else {
            // No split marker and no part/parts → a whole unsplit listing.
            plain_single = Some(obj.clone());
        }
    }

    // Unsplit single listing: only valid when it's the *only* payload (otherwise we have stray
    // content parts with no index, which is a tampered/incoherent set).
    if let Some(mut obj) = plain_single {
        if payloads.len() != 1 {
            return Err(NetError::Split(
                "content/plain parts present without a split index".into(),
            ));
        }
        let entries = obj.remove("entries").and_then(|v| match v {
            Value::Array(a) => Some(a),
            _ => None,
        });
        return Ok(RenderedListing {
            meta: obj,
            entries: entries.unwrap_or_default(),
            parts_total: 1,
            parts_present: 1,
            missing: Vec::new(),
        });
    }

    let index = index.ok_or_else(|| NetError::Split("no index part found".into()))?;
    let n = index
        .get("parts")
        .and_then(Value::as_u64)
        .ok_or_else(|| NetError::Split("index missing part count".into()))? as usize;
    if n > MAX_LISTING_PARTS {
        return Err(NetError::Split(format!(
            "index claims {n} parts, exceeds the {MAX_LISTING_PARTS}-part cap"
        )));
    }

    // Slot the content parts by index, rejecting anything the index never named.
    let mut slots: Vec<Option<Vec<Value>>> = vec![None; n];
    for (p, total, entries) in content {
        if total != n {
            return Err(NetError::Split(format!("part claims {total} parts, index says {n}")));
        }
        if p >= n {
            return Err(NetError::Split(format!("foreign part {p}: index names only 0..{n}")));
        }
        if slots[p].is_some() {
            return Err(NetError::Split(format!("duplicate part {p}")));
        }
        slots[p] = Some(entries);
    }

    let mut entries = Vec::new();
    let mut missing = Vec::new();
    for (i, slot) in slots.into_iter().enumerate() {
        match slot {
            Some(chunk) => entries.extend(chunk),
            None => missing.push(i),
        }
    }
    let parts_present = n - missing.len();

    let mut meta = index;
    meta.remove("split");
    meta.remove("parts");
    Ok(RenderedListing { meta, entries, parts_total: n, parts_present, missing })
}

/// v2 render (M13 depth-recursive split, `parts_v: 2`) — see module docs for the leniency rules.
fn render_v2(payloads: &[String]) -> Result<RenderedListing, NetError> {
    let mut index: Option<Map<String, Value>> = None;
    let mut candidates: Vec<(&str, Value)> = Vec::new();
    for json in payloads {
        let v: Value = serde_json::from_str(json).map_err(|e| NetError::Split(e.to_string()))?;
        let obj = v.as_object().ok_or_else(|| NetError::Split("part is not an object".into()))?;
        if obj.get("split") == Some(&Value::Bool(true)) {
            index = Some(obj.clone());
        } else {
            candidates.push((json.as_str(), v));
        }
    }
    let index = index.ok_or_else(|| NetError::Split("no index part found".into()))?;
    let (slot_hashes, part_count) = index_slots(&index)?;

    // Slot leniently: an unmatched hash is ignored (stale/unreferenced); a matched-but-already-
    // filled slot is still a hard error (a genuine duplicate is tampering, not loss).
    let mut filled: Vec<Option<Value>> = vec![None; part_count];
    for (raw, value) in &candidates {
        let slot = match slot_hashes.iter().position(|s| s == &sha256_hex(raw.as_bytes())) {
            Some(slot) => slot,
            None => continue,
        };
        if filled[slot].is_some() {
            return Err(NetError::Split(format!("duplicate part {slot}")));
        }
        filled[slot] = Some(value.clone());
    }

    let mut missing: Vec<usize> = Vec::new();
    let mut parsed: Vec<Option<(Vec<usize>, Vec<Value>)>> = Vec::with_capacity(part_count);
    for (i, slot) in filled.into_iter().enumerate() {
        match slot {
            None => {
                missing.push(i);
                parsed.push(None);
            }
            Some(v) => parsed.push(Some(parse_content_part_v2(v, i)?)),
        }
    }

    let mut root: Vec<Value> = Vec::new();
    for (i, part) in parsed.into_iter().enumerate() {
        let Some((mount, entries)) = part else { continue };
        if graft(&mut root, &mount, entries, i).is_err() {
            // A present part whose mount can't resolve: if nothing else is missing, this can't be
            // explained by loss — it's a hostile/corrupt index. Otherwise fold it into `missing`
            // too (an absent ancestor naturally strands its descendants — keeps K of N honest).
            if missing.is_empty() {
                return Err(NetError::Split(format!("invalid mount path in part {i}")));
            }
            missing.push(i);
        }
    }
    missing.sort_unstable();

    let parts_present = part_count - missing.len();
    let mut meta = index;
    meta.remove("split");
    meta.remove("parts_v");
    meta.remove("part_count");
    meta.remove("parts");
    Ok(RenderedListing { meta, entries: root, parts_total: part_count, parts_present, missing })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::split_listing;

    fn listing(n: usize) -> String {
        let entries: Vec<Value> = (0..n)
            .map(|i| serde_json::json!({ "name": format!("folder-{i:03}"), "size": 1000 + i }))
            .collect();
        serde_json::json!({ "slug": "criterion", "content_types": ["video"], "entries": entries })
            .to_string()
    }

    fn payloads(parts: &[crate::split::ListingPart]) -> Vec<String> {
        parts.iter().map(|p| p.json.clone()).collect()
    }

    // NOTE (M13): the split budgets below were bumped from the v1-era 256/200 — the v2 index
    // carries a per-part sha256 slot table, so at those budgets the index itself no longer fits
    // the budget at all. Per the tiny-budget rule: adjust the TEST budget, never the protocol.
    // 500 still forces listing(30) into ≥3 content parts (the withheld-middle case needs that).

    #[test]
    fn full_listing_renders_complete_tree() {
        let parts = split_listing("criterion", &listing(30), 500).unwrap();
        let r = render_listing(&payloads(&parts)).unwrap();
        assert!(r.complete(), "all parts present → complete");
        assert_eq!(r.entries.len(), 30, "every folder present");
        assert!(r.missing.is_empty() && r.parts_present == r.parts_total);
        assert_eq!(r.meta.get("slug").unwrap(), "criterion", "metadata preserved");
    }

    #[test]
    fn missing_parts_render_k_of_n_available() {
        let parts = split_listing("criterion", &listing(30), 500).unwrap();
        let mut p = payloads(&parts);
        p.pop(); // withhold the last content part
        let r = render_listing(&p).unwrap();
        assert!(!r.complete(), "a withheld part means incomplete, not an error");
        assert_eq!(r.parts_present, r.parts_total - 1);
        assert_eq!(r.missing.len(), 1, "the one withheld part is reported in `missing`");
        assert!(!r.entries.is_empty(), "the parts that did arrive still render");
    }

    #[test]
    fn withheld_folder_is_marked_unavailable_not_dropped() {
        // Drop a *middle* part — its index must appear in `missing` (so the UI can name it),
        // and the surrounding parts must still render.
        let parts = split_listing("criterion", &listing(30), 500).unwrap();
        let content_count = parts.len() - 1; // minus the index
        assert!(content_count >= 3, "need several parts for a meaningful middle drop");
        let mut p = payloads(&parts);
        p.remove(2); // remove a content part (index 0 is the listing index)
        let r = render_listing(&p).unwrap();
        assert_eq!(r.missing.len(), 1, "exactly one folder marked unavailable");
        assert_eq!(r.parts_present, r.parts_total - 1);
    }

    #[test]
    fn empty_index_is_clean_empty_not_error() {
        // A listing with no entries renders as a complete, empty tree — not an error.
        let parts = split_listing("empty", r#"{"slug":"empty","entries":[]}"#, 65_536).unwrap();
        let r = render_listing(&payloads(&parts)).unwrap();
        assert!(r.complete());
        assert!(r.entries.is_empty());
    }

    #[test]
    fn duplicate_part_rejected() {
        let parts = split_listing("criterion", &listing(30), 500).unwrap();
        let mut p = payloads(&parts);
        let last = p.len() - 1;
        p[last] = p[1].clone(); // duplicate part 0, drop the real last
        match render_listing(&p) {
            Err(NetError::Split(m)) => assert!(m.contains("duplicate"), "got: {m}"),
            other => panic!("expected a duplicate-part rejection, got {other:?}"),
        }
    }

    #[test]
    fn index_exceeding_max_parts_rejected_before_alloc() {
        // A forged index claiming a huge part count is refused before any per-part allocation.
        // This forged index is v1-shaped (`parts` a bare number, no `parts_v`) — it still hits the
        // v1 branch unchanged.
        let forged = serde_json::json!({
            "slug": "dos", "split": true, "parts": MAX_LISTING_PARTS + 1
        })
        .to_string();
        match render_listing(&[forged]) {
            Err(NetError::Split(m)) => assert!(m.contains("cap"), "got: {m}"),
            other => panic!("expected a parts-cap rejection, got {other:?}"),
        }
    }

    /// REWRITE of the old `foreign_part_not_in_index_rejected_with_reason` (M13): v2 slots by
    /// sha256, not position, so a payload matching no index slot is the spec's "stale part no
    /// longer referenced" — ignored, not a hard rejection. v1's positional range-check is preserved
    /// unchanged in `v1_foreign_part_rejected_with_reason` below.
    #[test]
    fn unreferenced_part_ignored_never_grafted() {
        let parts = split_listing("criterion", &listing(30), 500).unwrap();
        let mut p = payloads(&parts);
        p.push(serde_json::json!({ "entries": [], "mount": [], "part": 999, "parts_v": 2 }).to_string());
        let r = render_listing(&p).unwrap();
        assert!(r.complete(), "an unreferenced (stale) part must be ignored, not counted against completeness");
        assert_eq!(r.entries.len(), 30, "the real tree renders in full, unaffected by the stray part");
    }

    #[test]
    fn v1_foreign_part_rejected_with_reason() {
        // v1 coverage preserved: a hand-written v1 family (no `parts_v`) with a hostile
        // relay-injected content part outside the index's declared range must still be rejected —
        // the v2 hash-based slotting doesn't apply here, so the old positional-range check fires.
        let index = serde_json::json!({ "slug": "legacy", "split": true, "parts": 2 }).to_string();
        let part0 = serde_json::json!({ "part": 0, "parts": 2, "entries": [{"name":"a"}] }).to_string();
        let part1 = serde_json::json!({ "part": 1, "parts": 2, "entries": [{"name":"b"}] }).to_string();
        let foreign = serde_json::json!({ "part": 7, "parts": 2, "entries": [] }).to_string();
        match render_listing(&[index, part0, part1, foreign]) {
            Err(NetError::Split(m)) => assert!(m.contains("foreign part"), "got: {m}"),
            other => panic!("expected a foreign-part rejection, got {other:?}"),
        }
    }

    /// REWRITE of the old `part_with_wrong_total_rejected` (M13): the v2 equivalent is subsumed by
    /// hash-mismatch-ignore (a part claiming the wrong total won't hash-match any slot either way,
    /// so it's silently ignored, not distinguishably rejected). This hand-written v1 literal
    /// preserves the pre-M13 behaviour: a relay-injected part claiming a mismatched `parts` total
    /// must still be rejected.
    #[test]
    fn v1_part_with_wrong_total_rejected() {
        let index = serde_json::json!({ "slug": "legacy", "split": true, "parts": 2 }).to_string();
        let part0 = serde_json::json!({ "part": 0, "parts": 2, "entries": [{"name":"a"}] }).to_string();
        let part1 = serde_json::json!({ "part": 1, "parts": 2, "entries": [{"name":"b"}] }).to_string();
        let wrong_total = serde_json::json!({ "part": 0, "parts": 99, "entries": [] }).to_string();
        match render_listing(&[index, part0, part1, wrong_total]) {
            Err(NetError::Split(m)) => assert!(m.contains("part claims"), "got: {m}"),
            other => panic!("expected a wrong-total rejection, got {other:?}"),
        }
    }

    #[test]
    fn plain_single_wins_over_orphan_split_parts() {
        // devtest #7: a collection republished as ONE (truncated or whole) event supersedes its old
        // `d=slug` index but leaves stale `#partN` orphans on the relay. Those orphans carry `parts_v`,
        // so a naive render would route to v2 and skip the family for want of an index. The plain
        // unsplit listing must win — the orphans are ignored.
        let single = serde_json::json!({
            "slug": "vault", "content_types": ["video"], "truncated": true, "total_items": 900,
            "entries": [{"name": "a.mkv", "item_type": "File", "children": []}]
        })
        .to_string();
        let orphan0 = serde_json::json!({ "entries": [{"name":"stale0"}], "mount": [], "part": 0, "parts_v": 2 }).to_string();
        let orphan1 = serde_json::json!({ "entries": [{"name":"stale1"}], "mount": [], "part": 1, "parts_v": 2 }).to_string();

        let r = render_listing(&[single, orphan0, orphan1]).unwrap();
        assert_eq!(r.entries.len(), 1, "only the authoritative single listing's entries render");
        assert_eq!(r.entries[0]["name"], serde_json::json!("a.mkv"), "the orphan parts are ignored");
        assert_eq!(r.meta.get("truncated"), Some(&Value::Bool(true)), "the paywall markers survive in meta");
        assert_eq!(r.meta.get("total_items").and_then(Value::as_u64), Some(900));
    }

    #[test]
    fn unknown_parts_version_render_refused() {
        let index = serde_json::json!({
            "slug": "future", "split": true, "parts_v": 5, "part_count": 0, "parts": []
        })
        .to_string();
        match render_listing(&[index]) {
            Err(NetError::Split(m)) => assert!(m.contains("unknown parts version"), "got: {m}"),
            other => panic!("expected an unknown-version rejection, got {other:?}"),
        }
    }

    #[test]
    fn v1_family_skips_v2_stragglers() {
        // A v1 family (real v1 index+parts) plus a stray v2-shaped payload (carries `parts_v`, no
        // `split` marker) must render the v1 family cleanly — the straggler is skipped outright,
        // not mis-parsed into `plain_single` (which would wrongly error "content/plain parts
        // present without a split index" for what is otherwise a valid multi-part v1 set).
        let index = serde_json::json!({ "slug": "legacy", "split": true, "parts": 2 }).to_string();
        let part0 = serde_json::json!({ "part": 0, "parts": 2, "entries": [{"name":"a"}] }).to_string();
        let part1 = serde_json::json!({ "part": 1, "parts": 2, "entries": [{"name":"b"}] }).to_string();
        let straggler =
            serde_json::json!({ "entries": [], "mount": [], "part": 0, "parts_v": 2 }).to_string();
        let r = render_listing(&[index, part0, part1, straggler]).unwrap();
        assert!(r.complete());
        assert_eq!(r.entries.len(), 2);
    }

    /// One folder with several children — the shape whose split forces a shell part (`mount: []`)
    /// and then several child parts (`mount: [0]`), so dropping the shell exercises the "ancestor
    /// rule": every part grafted under it must also read as unavailable, not silently vanish from
    /// the count.
    fn deep_listing(n: usize) -> String {
        let children: Vec<Value> =
            (0..n).map(|i| serde_json::json!({ "name": format!("file-{i:03}") })).collect();
        serde_json::json!({
            "slug": "deep", "content_types": ["video"],
            "entries": [ { "name": "Movies", "children": children } ],
        })
        .to_string()
    }

    #[test]
    fn missing_deep_part_renders_k_of_n_and_ancestor_rule() {
        // 800: big enough for the 3-slot v2 index, small enough that the 40 children still split
        // into a shell part plus ≥2 child parts under `mount: [0]`.
        let parts = split_listing("deep", &deep_listing(40), 800).unwrap();
        assert!(parts.len() > 2, "need a real split to exercise the ancestor rule");
        let mut p = payloads(&parts);
        // Content part 0 (index 1 in `parts`/`p`) is the shell — drop it. Everything mounted under
        // it (the child parts) can no longer graft: they must ALSO read as missing (never silently
        // drop from `entries` with no accounting), keeping `parts_present`/`missing` honest.
        p.remove(1);
        let r = render_listing(&p).unwrap();
        assert!(!r.complete(), "a withheld ancestor part must render incomplete");
        assert_eq!(r.parts_present, r.parts_total - r.missing.len(), "K of N accounting must stay consistent");
        assert!(
            r.missing.len() >= 2,
            "the shell AND every part grafted under it must count as missing, got {:?}",
            r.missing
        );
    }
}
