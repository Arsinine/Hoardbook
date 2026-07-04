//! Oversize-listing split / restitch (N4; spec §Data Model split-listing protocol, HANDOVER
//! §C). A relay caps event size (strfry default 64 KiB), so a large collection listing is split
//! into an **index part** plus per-chunk **content parts**, each a separately-published
//! parameterized-replaceable event, and re-stitched into the full tree on fetch.
//!
//! The listing payload is a JSON object with an `entries` array (the top-level folders/files);
//! every other top-level field (slug, content_types, …) is metadata preserved verbatim. When the
//! whole payload fits, it is published as a single event under `d = <slug>`. When it doesn't, the
//! `entries` are chunked under a byte budget; the index records the part count so a fetcher
//! knows how many to collect and can report "N of M parts available" on loss.
//!
//! **v1** (shipped, live on relays — absence of `parts_v`) splits only the top-level **breadth** of
//! `entries`: a single dominant folder deeper than the budget can't be split further and becomes an
//! oversized part the relay rejects downstream. **v2** (M13, `parts_v: 2`) fixes this with a
//! depth-recursive packer ([`pack_tree`]): when a folder's own subtree won't fit, its children are
//! carved out into their own part(s) and grafted back onto the right tree node via a `mount` path
//! (child indices from the root) carried on each content part. The index also gains a per-part
//! `sha256` (per-part integrity was deferred at M2/M3 since each part is already a Schnorr-signed
//! event — see below; M13 needs the hash regardless, as it's now *how* a v2 part is matched to its
//! declared slot, not its claimed position): a stale, no-longer-referenced payload left over from a
//! prior publish of the same slug is silently ignored rather than corrupting the tree. `split: true`
//! is kept on **both** wire versions on purpose — it's the **poison pill** that makes a pre-M13
//! client recognise a v2 index as an index it can't parse (`parts` is now an array, so its
//! `as_u64` read fails) and fall into its own existing "index missing part count" error, instead of
//! silently mis-rendering a partial or wrong tree.
//!
//! Per-part `sha256` integrity for v1 (spec) was intentionally deferred at M2/M3: each part is
//! already a Schnorr-signed listing event (tamper-evident at the event layer), so the split
//! protocol only carried the index→parts linkage restitch needed. Recorded in HANDOVER.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::NetError;
use crate::render::MAX_LISTING_PARTS;

/// One published unit of a (possibly split) listing: the parameterized-replaceable `d`
/// identifier its event carries, and the plaintext JSON payload to encrypt + publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingPart {
    pub d_tag: String,
    pub json: String,
}

/// The `parts_v` discriminant for the depth-recursive split protocol (M13). **Wire-frozen the
/// moment a v2 listing exists in the wild** (INVARIANT_AUDIT.md I-3 style): every such event's
/// ciphertext carries this number, and a v1 listing already published carries no `parts_v` at all
/// (absence means v1 — see module docs). Bumping this number, rather than introducing a new one,
/// would change what every v2 event already on a relay means — never an edit.
pub const PARTS_V: u64 = 2;

/// Depth cap on a content part's `mount` path (child indices from the tree root) so a
/// hostile/malformed index can't force unbounded recursion while restitching.
pub const MAX_MOUNT_DEPTH: usize = 128;

/// Total bytes restitch will accumulate across matched (sha256-verified) part payloads before
/// refusing — bounds a hostile index that names many large duplicate-content parts.
pub const MAX_RESTITCHED_BYTES: usize = 64 * 1024 * 1024;

/// The `d`-tag of content part `i` for a given slug (deterministic so a fetcher can address them).
fn part_d_tag(slug: &str, i: usize) -> String {
    format!("{slug}#part{i}")
}

/// Normalise a JSON payload to its canonical (sorted-key) string form, so split→restitch is a
/// byte-exact round-trip regardless of input key order.
fn normalize(json: &str) -> Result<String, NetError> {
    let v: Value = serde_json::from_str(json).map_err(|e| NetError::Split(e.to_string()))?;
    serde_json::to_string(&v).map_err(|e| NetError::Split(e.to_string()))
}

// ── shared with `render.rs` (v2 index/part shape + tree grafting) ────────────────────────────────

/// Hex-encode a sha256 digest of `bytes` (lowercase). No `hex` crate dependency needed — this
/// crate already carries `sha2`, and a lowercase-hex fold is a one-liner.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// 64 lowercase hex characters — the exact shape a digest from [`sha256_hex`] takes.
pub(crate) fn is_lowercase_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Peek a split family's version discriminant without fully parsing content parts. `None` means no
/// index (`split == true`) is present at all — a lone unsplit part, or a stray-content-without-index
/// set the v1 path already reports clearly on its own — both are handled by the v1 branch
/// unchanged. `Some(1)` covers both v1's marker and its absence (the shipped default).
pub(crate) fn parts_version(parts: &[String]) -> Result<Option<u64>, NetError> {
    for json in parts {
        let v: Value = serde_json::from_str(json).map_err(|e| NetError::Split(e.to_string()))?;
        let obj = match v.as_object() {
            Some(obj) => obj,
            None => continue,
        };
        if obj.get("split") == Some(&Value::Bool(true)) {
            return match obj.get("parts_v") {
                None => Ok(Some(1)),
                Some(pv) => match pv.as_u64() {
                    Some(n) => Ok(Some(n)),
                    None => Err(NetError::Split(format!("unknown parts version {pv}"))),
                },
            };
        }
    }
    Ok(None)
}

/// Validate + extract a v2 index's slot table: `part_count` (≤ [`MAX_LISTING_PARTS`]), the `parts`
/// array's length agreeing with it, and each slot's `sha256` well-formed (64 lowercase hex) and
/// unique across slots. Shared by [`restitch_v2`] and `render.rs`'s v2 render — both slot payloads
/// by hash against the same index shape.
pub(crate) fn index_slots(index: &Map<String, Value>) -> Result<(Vec<String>, usize), NetError> {
    let part_count = index
        .get("part_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| NetError::Split("index missing part count".into()))? as usize;
    if part_count > MAX_LISTING_PARTS {
        return Err(NetError::Split(format!(
            "index claims {part_count} parts, exceeds the {MAX_LISTING_PARTS}-part cap"
        )));
    }
    let parts_arr = index
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| NetError::Split("index missing parts array".into()))?;
    if parts_arr.len() != part_count {
        return Err(NetError::Split(format!(
            "index parts array has {} entries, part_count says {part_count}",
            parts_arr.len()
        )));
    }

    let mut seen = std::collections::HashSet::with_capacity(part_count);
    let mut hashes = Vec::with_capacity(part_count);
    for elem in parts_arr {
        let obj = elem
            .as_object()
            .ok_or_else(|| NetError::Split("malformed parts entry in index".into()))?;
        let sha = obj
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| NetError::Split("parts entry missing sha256".into()))?;
        if !is_lowercase_sha256_hex(sha) {
            return Err(NetError::Split(format!("parts entry has malformed sha256: {sha}")));
        }
        if !seen.insert(sha.to_string()) {
            return Err(NetError::Split(format!("duplicate sha256 across index slots: {sha}")));
        }
        hashes.push(sha.to_string());
    }
    Ok((hashes, part_count))
}

/// Parse + validate one slotted v2 content-part payload: `parts_v == PARTS_V`, its internal `part`
/// marker equals its slot index (sanity against the sha256-keyed slotting), and a `mount` path
/// within [`MAX_MOUNT_DEPTH`]. Shared by [`restitch_v2`] and `render.rs`'s v2 render.
pub(crate) fn parse_content_part_v2(
    value: Value,
    slot: usize,
) -> Result<(Vec<usize>, Vec<Value>), NetError> {
    let obj = value
        .as_object()
        .ok_or_else(|| NetError::Split(format!("content part {slot} is not an object")))?;
    if obj.get("parts_v").and_then(Value::as_u64) != Some(PARTS_V) {
        return Err(NetError::Split(format!("content part {slot} missing/mismatched parts_v")));
    }
    let part_num = obj
        .get("part")
        .and_then(Value::as_u64)
        .ok_or_else(|| NetError::Split(format!("content part {slot} missing part marker")))?
        as usize;
    if part_num != slot {
        return Err(NetError::Split(format!(
            "internal part marker mismatch: slot {slot} claims part {part_num}"
        )));
    }
    let mount_arr = obj
        .get("mount")
        .and_then(Value::as_array)
        .ok_or_else(|| NetError::Split(format!("content part {slot} missing mount")))?;
    if mount_arr.len() > MAX_MOUNT_DEPTH {
        return Err(NetError::Split(format!(
            "mount path in part {slot} exceeds the {MAX_MOUNT_DEPTH}-depth cap"
        )));
    }
    let mut mount = Vec::with_capacity(mount_arr.len());
    for m in mount_arr {
        let idx = m
            .as_u64()
            .ok_or_else(|| NetError::Split(format!("invalid mount path in part {slot}")))?
            as usize;
        mount.push(idx);
    }
    let entries = obj
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| NetError::Split(format!("content part {slot} missing entries")))?;
    Ok((mount, entries))
}

/// Graft `entries` onto the node at `mount` (a child-index path from the tree root; `[]` = top
/// level) in the tree being reassembled. Every step must resolve through an object carrying a
/// `children` array — an out-of-range index, a node with no `children` (a file, not a folder), or a
/// non-object entry is `Err` (a malformed/hostile index; a well-formed family never fails to
/// resolve, since ancestors always land in earlier parts than their descendants by construction).
pub(crate) fn graft(
    root: &mut Vec<Value>,
    mount: &[usize],
    mut entries: Vec<Value>,
    part_idx: usize,
) -> Result<(), NetError> {
    if mount.is_empty() {
        root.append(&mut entries);
        return Ok(());
    }
    let node = root.get_mut(mount[0]).ok_or_else(|| bad_mount(part_idx))?;
    graft_into(node, &mount[1..], entries, part_idx)
}

fn graft_into(
    node: &mut Value,
    mount: &[usize],
    mut entries: Vec<Value>,
    part_idx: usize,
) -> Result<(), NetError> {
    let obj = node.as_object_mut().ok_or_else(|| bad_mount(part_idx))?;
    let kids =
        obj.get_mut("children").and_then(Value::as_array_mut).ok_or_else(|| bad_mount(part_idx))?;
    if mount.is_empty() {
        kids.append(&mut entries);
        return Ok(());
    }
    let next = kids.get_mut(mount[0]).ok_or_else(|| bad_mount(part_idx))?;
    graft_into(next, &mount[1..], entries, part_idx)
}

fn bad_mount(part_idx: usize) -> NetError {
    NetError::Split(format!("invalid mount path in part {part_idx}"))
}

// ── split (write side) ────────────────────────────────────────────────────────────────────────

/// Split a listing payload into publishable parts under a `max_bytes` budget on each part's JSON.
///
/// A payload that fits returns a single part under `d = slug` (unchanged fast path). An oversize
/// payload is packed depth-recursively ([`pack_tree`]) into an index part (`d = slug`, recording
/// `split: true` + `parts_v: 2` + `part_count: N` + a per-part `sha256` table + the preserved
/// metadata) plus N content parts (`d = slug#partI`), each carrying a `mount` path.
pub fn split_listing(
    slug: &str,
    listing_json: &str,
    max_bytes: usize,
) -> Result<Vec<ListingPart>, NetError> {
    let normalized = normalize(listing_json)?;
    if normalized.len() <= max_bytes {
        return Ok(vec![ListingPart { d_tag: slug.to_string(), json: normalized }]);
    }

    // Operate on the canonical form the fast path already produced (coherent with this
    // function's contract; guaranteed to re-parse since `normalize` validated it).
    let value: Value = serde_json::from_str(&normalized).map_err(|e| NetError::Split(e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| NetError::Split("listing payload is not a JSON object".into()))?;
    let entries = obj
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| NetError::Split("oversize listing has no `entries` array to split".into()))?;

    // Metadata = every top-level field except `entries`, preserved verbatim into the index.
    let mut meta: Map<String, Value> = obj.clone();
    meta.remove("entries");

    let chunks = pack_tree(entries, max_bytes)?;
    let n = chunks.len();
    if n > MAX_LISTING_PARTS {
        return Err(NetError::Split(format!(
            "this collection needs {n} parts to publish, exceeds the {MAX_LISTING_PARTS}-part cap"
        )));
    }

    // Content parts first — their position here IS their final part index (`pack_tree`'s DFS
    // emission order) — so the index below can record each one's real sha256.
    let mut content_parts = Vec::with_capacity(n);
    let mut slot_descriptors = Vec::with_capacity(n);
    for (i, chunk) in chunks.iter().enumerate() {
        let d_tag = part_d_tag(slug, i);
        let json = content_part_v2_json(&chunk.entries, &chunk.mount, i)?;
        slot_descriptors
            .push(serde_json::json!({ "part_d": d_tag, "sha256": sha256_hex(json.as_bytes()) }));
        content_parts.push(ListingPart { d_tag, json });
    }

    let mut index = meta;
    index.insert("split".into(), Value::Bool(true));
    index.insert("parts_v".into(), Value::from(PARTS_V));
    index.insert("part_count".into(), Value::from(n));
    index.insert("parts".into(), Value::Array(slot_descriptors));
    let index_json =
        serde_json::to_string(&Value::Object(index)).map_err(|e| NetError::Split(e.to_string()))?;
    if index_json.len() > max_bytes {
        return Err(NetError::Split(format!(
            "the listing index itself is {} bytes, over the {max_bytes}-byte per-listing limit even \
             with `entries` moved out — its metadata (or part count) is too large. Shorten its \
             fields or publish fewer parts.",
            index_json.len()
        )));
    }

    let mut parts = Vec::with_capacity(n + 1);
    parts.push(ListingPart { d_tag: slug.to_string(), json: index_json });
    parts.extend(content_parts);
    Ok(parts)
}

/// Build one v2 content part's JSON: `entries` + `mount` + `part` + `parts_v`. Used both for the
/// final emitted part (real `part` index) and, with `part = usize::MAX`, as a worst-case-width
/// measurement during packing (so a tight chunk can't exceed budget once the real, possibly
/// multi-digit, part index is substituted).
fn content_part_v2_json(entries: &[Value], mount: &[usize], part: usize) -> Result<String, NetError> {
    let mut m = Map::new();
    m.insert("entries".into(), Value::Array(entries.to_vec()));
    m.insert("mount".into(), Value::Array(mount.iter().map(|&i| Value::from(i)).collect()));
    m.insert("part".into(), Value::from(part));
    m.insert("parts_v".into(), Value::from(PARTS_V));
    serde_json::to_string(&Value::Object(m)).map_err(|e| NetError::Split(e.to_string()))
}

/// One tree-shaped unit [`pack_tree`] assigns to a graft point: `mount` = the child-index path
/// (from the tree root) where `entries` must be appended during restitch (`[]` = top level).
struct Chunk {
    mount: Vec<usize>,
    entries: Vec<Value>,
}

/// Pack a listing's `entries` tree into publishable chunks under `max_bytes`: depth-first,
/// order-preserving, greedy first-fit (M13 depth-recursive split — see module docs for the v2 wire
/// shape). Chunks come back in DFS emission order; the caller assigns each one's final part index
/// by its position in the returned `Vec` (part 0 = the top-level run, `mount == []`).
fn pack_tree(entries: &[Value], max_bytes: usize) -> Result<Vec<Chunk>, NetError> {
    let mut chunks = Vec::new();
    pack_siblings(entries, &[], max_bytes, &mut chunks)?;
    Ok(chunks)
}

fn pack_siblings(
    entries: &[Value],
    mount: &[usize],
    max_bytes: usize,
    chunks: &mut Vec<Chunk>,
) -> Result<(), NetError> {
    let mut current: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let entry = &entries[i];
        let mut candidate = current.clone();
        candidate.push(entry.clone());
        if content_part_v2_json(&candidate, mount, usize::MAX)?.len() <= max_bytes {
            current = candidate;
            i += 1;
            continue;
        }
        if !current.is_empty() {
            // Flush what fits, then retry entry `i` alone on a fresh part at the same mount.
            chunks.push(Chunk { mount: mount.to_vec(), entries: std::mem::take(&mut current) });
            continue;
        }
        // Doesn't fit even alone. A folder with children can shed weight: emit its SHELL (the
        // node with `children` emptied — the key stays present so restitch is byte-exact) into
        // this fresh part, then recurse into its children at `mount + [i]`.
        if let Some(obj) = entry.as_object() {
            if let Some(kids) = obj.get("children").and_then(Value::as_array) {
                if !kids.is_empty() {
                    let mut shell = obj.clone();
                    shell.insert("children".into(), Value::Array(Vec::new()));
                    let shell_entries = vec![Value::Object(shell)];
                    let shell_len = content_part_v2_json(&shell_entries, mount, usize::MAX)?.len();
                    if shell_len > max_bytes {
                        return Err(oversized_entry_error(shell_len, max_bytes));
                    }
                    chunks.push(Chunk { mount: mount.to_vec(), entries: shell_entries });
                    let mut child_mount = mount.to_vec();
                    child_mount.push(i);
                    pack_siblings(kids, &child_mount, max_bytes, chunks)?;
                    i += 1;
                    continue;
                }
            }
        }
        // A leaf file, a childless folder, or a non-object entry that still doesn't fit alone
        // cannot be split any further at any depth.
        let entry_len = content_part_v2_json(std::slice::from_ref(entry), mount, usize::MAX)?.len();
        return Err(oversized_entry_error(entry_len, max_bytes));
    }
    if !current.is_empty() {
        chunks.push(Chunk { mount: mount.to_vec(), entries: current });
    }
    Ok(())
}

fn oversized_entry_error(bytes: usize, max_bytes: usize) -> NetError {
    NetError::Split(format!(
        "this collection is too large to publish: a single item's metadata is {bytes} bytes, over \
         the {max_bytes}-byte per-listing limit and cannot be split further. Shorten its name/note \
         or exclude it."
    ))
}

// ── restitch (read side, strict) ──────────────────────────────────────────────────────────────

/// Re-stitch fetched part payloads into the full listing. A single unsplit part is returned
/// as-is; otherwise the index supplies the part count + metadata and the content parts supply
/// the `entries`. A missing **or duplicate** content part is reported as an error (the spec's
/// "N of M folders available on loss"), never a silent partial/corrupt tree.
///
/// `parts` is the set of decrypted JSON payloads (the `d` tag is not needed — v1 parts self-index
/// via their `part`/`parts` markers, v2 parts via the index's sha256 table). **The caller must pass
/// payloads from signature-verified+decrypted events** (e.g. via `parse_listing_event`): this
/// function trusts the JSON it is given and does not verify provenance.
pub fn restitch_listing(parts: &[String]) -> Result<String, NetError> {
    if parts.is_empty() {
        return Err(NetError::Split("no parts to restitch".into()));
    }
    match parts_version(parts)? {
        None | Some(1) => restitch_v1(parts),
        Some(2) => restitch_v2(parts),
        Some(v) => Err(NetError::Split(format!("unknown parts version {v}"))),
    }
}

/// v1 restitch (shipped, live on relays — absence of `parts_v`, or `parts_v: 1`): the index's
/// `parts` is a plain part COUNT and content parts self-index via `part`/`parts` markers only
/// (breadth-only split, no `mount`). Extracted verbatim from the pre-M13 `restitch_listing`.
fn restitch_v1(parts: &[String]) -> Result<String, NetError> {
    if parts.is_empty() {
        return Err(NetError::Split("no parts to restitch".into()));
    }

    // Locate the index (split == true). A lone part with no split marker is the whole listing.
    let mut index: Option<Map<String, Value>> = None;
    let mut content: Vec<(usize, usize, Vec<Value>)> = Vec::new();
    for json in parts {
        let v: Value = serde_json::from_str(json).map_err(|e| NetError::Split(e.to_string()))?;
        let obj = v.as_object().ok_or_else(|| NetError::Split("part is not an object".into()))?;
        if obj.get("split") == Some(&Value::Bool(true)) {
            index = Some(obj.clone());
        } else if let Some(entries) = obj.get("entries").and_then(Value::as_array) {
            let part = obj.get("part").and_then(Value::as_u64);
            let total = obj.get("parts").and_then(Value::as_u64);
            match (part, total) {
                (Some(p), Some(t)) => content.push((p as usize, t as usize, entries.clone())),
                // No part markers → an unsplit single listing; return it normalized.
                _ if parts.len() == 1 => return normalize(json),
                _ => return Err(NetError::Split("content part missing part/parts markers".into())),
            }
        } else if parts.len() == 1 {
            return normalize(json); // single unsplit part with no entries (e.g. empty listing)
        }
    }

    let index = index.ok_or_else(|| NetError::Split("no index part found".into()))?;
    let n = index
        .get("parts")
        .and_then(Value::as_u64)
        .ok_or_else(|| NetError::Split("index missing part count".into()))? as usize;

    if content.len() != n {
        return Err(NetError::Split(format!("got {} of {} parts", content.len(), n)));
    }
    // Parts must be exactly {0,1,…,n-1}, each present once — so a hostile relay can't slip in
    // duplicate part numbers (which would pass the count check while a real part is missing).
    content.sort_by_key(|(p, _, _)| *p);
    let mut entries: Vec<Value> = Vec::new();
    for (i, (p, total, chunk)) in content.into_iter().enumerate() {
        if p != i {
            return Err(NetError::Split(format!("missing or duplicate part: expected {i}, got {p}")));
        }
        if total != n {
            return Err(NetError::Split(format!("part claims {total} parts, index says {n}")));
        }
        entries.extend(chunk);
    }

    let mut full: Map<String, Value> = index;
    full.remove("split");
    full.remove("parts");
    full.insert("entries".into(), Value::Array(entries));
    serde_json::to_string(&Value::Object(full)).map_err(|e| NetError::Split(e.to_string()))
}

/// v2 restitch (M13 depth-recursive split, `parts_v: 2`): the index names each content part's
/// **sha256** — parts are matched to their declared slot by hash, not position, so a stale payload
/// left over from a prior publish of the same slug (spec: "no longer referenced") is silently
/// ignored rather than corrupting the tree. Grafting an out-of-range/hostile `mount` is always a
/// hard error here — restitch wants the whole tree or nothing (the lenient sibling for partial
/// trees is `render.rs`'s v2 render).
fn restitch_v2(parts: &[String]) -> Result<String, NetError> {
    let mut index: Option<Map<String, Value>> = None;
    let mut candidates: Vec<(&str, Value)> = Vec::new();
    for json in parts {
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

    let mut filled: Vec<Option<Value>> = vec![None; part_count];
    let mut matched_bytes: usize = 0;
    for (raw, value) in &candidates {
        let slot = match slot_hashes.iter().position(|s| s == &sha256_hex(raw.as_bytes())) {
            Some(slot) => slot,
            None => continue, // sha256 matches no declared slot → stale/unreferenced, ignored
        };
        if filled[slot].is_some() {
            return Err(NetError::Split(format!("duplicate part {slot}")));
        }
        matched_bytes += raw.len();
        if matched_bytes > MAX_RESTITCHED_BYTES {
            return Err(NetError::Split(format!(
                "restitched payload exceeds the {MAX_RESTITCHED_BYTES}-byte cap"
            )));
        }
        filled[slot] = Some(value.clone());
    }

    let present = filled.iter().filter(|s| s.is_some()).count();
    if present != part_count {
        return Err(NetError::Split(format!("got {present} of {part_count} parts")));
    }

    let mut root: Vec<Value> = Vec::new();
    for (i, slot) in filled.into_iter().enumerate() {
        let (mount, entries) = parse_content_part_v2(slot.expect("checked complete above"), i)?;
        graft(&mut root, &mount, entries, i)?;
    }

    let mut full = index;
    for key in ["split", "parts_v", "part_count", "parts"] {
        full.remove(key);
    }
    full.insert("entries".into(), Value::Array(root));
    serde_json::to_string(&Value::Object(full)).map_err(|e| NetError::Split(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A listing with `n` folder entries, each padded so the whole payload is comfortably large.
    fn listing(n: usize) -> String {
        let entries: Vec<Value> = (0..n)
            .map(|i| serde_json::json!({ "name": format!("folder-{i:03}"), "size": 1_000_000 + i }))
            .collect();
        serde_json::json!({
            "slug": "criterion",
            "content_types": ["video"],
            "entries": entries,
        })
        .to_string()
    }

    #[test]
    fn small_listing_is_single_part() {
        let parts = split_listing("criterion", &listing(2), 65_536).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].d_tag, "criterion");
        assert!(!parts[0].json.contains("\"split\""), "a fitting listing carries no split marker");
    }

    #[test]
    fn oversize_listing_splits_per_folder() {
        // A tight budget forces multiple content parts. (M13: bumped from the v1 256B budget — the
        // v2 index carries a per-part sha256 slot table, so it needs more headroom than a bare part
        // count did; per the module's own tiny-budget note, adjust the TEST budget, never the
        // protocol.)
        let parts = split_listing("criterion", &listing(40), 1000).unwrap();
        assert!(parts.len() > 2, "expected an index + several content parts, got {}", parts.len());
        // The index leads; content parts stay within budget.
        assert_eq!(parts[0].d_tag, "criterion");
        assert!(parts[0].json.contains("\"split\":true"));
        for p in &parts[1..] {
            assert!(p.json.len() <= 1000, "content part {} exceeds budget: {} bytes", p.d_tag, p.json.len());
        }
    }

    fn payloads(parts: &[ListingPart]) -> Vec<String> {
        parts.iter().map(|p| p.json.clone()).collect()
    }

    #[test]
    fn split_listing_restitches_to_full_tree() {
        let original = listing(40);
        let parts = split_listing("criterion", &original, 1000).unwrap();
        let restitched = restitch_listing(&payloads(&parts)).unwrap();
        assert_eq!(restitched, normalize(&original).unwrap(), "restitch must reproduce the full tree");
    }

    #[test]
    fn restitch_single_unsplit_part() {
        let original = listing(2);
        let parts = split_listing("criterion", &original, 65_536).unwrap();
        assert_eq!(restitch_listing(&payloads(&parts)).unwrap(), normalize(&original).unwrap());
    }

    #[test]
    fn restitch_detects_missing_part() {
        let parts = split_listing("criterion", &listing(40), 1000).unwrap();
        // Drop one content part → restitch refuses with an "N of M" report.
        let mut fetched = payloads(&parts);
        fetched.pop();
        match restitch_listing(&fetched) {
            Err(NetError::Split(m)) => assert!(m.contains(" of "), "expected 'K of N', got: {m}"),
            other => panic!("expected a missing-part error, got {other:?}"),
        }
    }

    #[test]
    fn restitch_rejects_duplicate_part() {
        // A hostile relay returns the right *count* of parts but a duplicate part number and a
        // missing one → must be refused, not silently restitched into a corrupt tree.
        let parts = split_listing("criterion", &listing(40), 1000).unwrap();
        let mut fetched = payloads(&parts);
        let last = fetched.len() - 1;
        fetched[last] = fetched[1].clone(); // replace the last content part with a copy of part 0
        match restitch_listing(&fetched) {
            Err(NetError::Split(m)) => assert!(m.contains("duplicate"), "expected duplicate error, got: {m}"),
            other => panic!("expected a duplicate-part error, got {other:?}"),
        }
    }

    /// One top-level folder whose own subtree exceeds any small budget — the "single deep folder"
    /// shape (e.g. 113k items under one root) the breadth-only v1 chunker cannot split (devtest #3).
    fn single_huge_entry() -> String {
        let children: Vec<Value> = (0..200)
            .map(|i| serde_json::json!({ "name": format!("file-{i:05}.bin"), "size": 1234 }))
            .collect();
        serde_json::json!({
            "slug": "hoard",
            "content_types": ["video"],
            "entries": [ { "name": "Movies", "children": children } ],
        })
        .to_string()
    }

    #[test]
    fn deep_single_root_folder_splits_under_budget() {
        // The real hoard shape: everything under one folder. Every published part MUST fit the
        // budget, and the parts MUST restitch to the original tree (M13: depth-recursive split).
        // Budget bumped from the v1-era 512B: 200 children at this budget need several file-chunk
        // parts, and the v2 index's per-part sha256 slot table needs headroom the old bare part
        // count didn't (per the module's tiny-budget note — adjust the TEST budget, never the
        // protocol).
        let parts = split_listing("hoard", &single_huge_entry(), 3000).unwrap();
        for p in &parts[1..] {
            assert!(
                p.json.len() <= 3000,
                "part {} is oversized: {} bytes — deep tree not recursively split",
                p.d_tag,
                p.json.len()
            );
        }
        let payloads: Vec<String> = parts.iter().map(|p| p.json.clone()).collect();
        assert_eq!(restitch_listing(&payloads).unwrap(), normalize(&single_huge_entry()).unwrap());
    }

    #[test]
    fn parts_v_discriminant_is_frozen() {
        // Wire-frozen (INVARIANT_AUDIT.md I-3 style): every v2 listing already published carries
        // this number inside a signed event's ciphertext. Renumbering it silently reinterprets
        // every such event already on a relay.
        assert_eq!(PARTS_V, 2, "PARTS_V is a wire discriminant — never renumber, only introduce a new one");
    }

    #[test]
    fn v2_index_shape_is_the_old_reader_poison_pill() {
        let parts = split_listing("criterion", &listing(40), 1000).unwrap();
        let index: Value = serde_json::from_str(&parts[0].json).unwrap();
        assert_eq!(index["split"], Value::Bool(true));
        assert_eq!(index["parts_v"], Value::from(PARTS_V));
        let n = parts.len() - 1;
        assert_eq!(index["part_count"], Value::from(n));
        let declared = index["parts"].as_array().unwrap();
        assert_eq!(declared.len(), n);
        for (i, slot) in declared.iter().enumerate() {
            assert_eq!(slot["part_d"], Value::String(format!("criterion#part{i}")));
            let expected_sha = sha256_hex(parts[i + 1].json.as_bytes());
            assert_eq!(slot["sha256"], Value::String(expected_sha), "index sha256 must match the real part payload");
        }
        // THE POISON PILL: an old client's `index.get("parts").and_then(Value::as_u64)` — now an
        // array, not a number — must fail, so it falls into its existing clean "index missing part
        // count" error instead of silently mis-rendering.
        assert!(index.get("parts").and_then(Value::as_u64).is_none());
    }

    /// Minimal seeded xorshift32 PRNG — no new dependency, just enough determinism for the
    /// round-trip property test below to be reproducible across runs.
    struct XorShift32(u32);
    impl XorShift32 {
        fn new(seed: u32) -> Self {
            XorShift32(if seed == 0 { 1 } else { seed })
        }
        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            x
        }
        fn next_range(&mut self, n: u32) -> u32 {
            self.next_u32() % n.max(1)
        }
    }

    fn random_node(rng: &mut XorShift32, depth: usize, id: &mut u32) -> Value {
        *id += 1;
        let name_len = 3 + rng.next_range(20) as usize;
        let name: String = (0..name_len).map(|_| (b'a' + rng.next_range(26) as u8) as char).collect();
        let mut obj = Map::new();
        obj.insert("name".into(), Value::String(format!("{name}-{id}")));
        if rng.next_range(2) == 0 {
            obj.insert("size".into(), Value::from(rng.next_u32()));
        }
        if rng.next_range(2) == 0 {
            obj.insert("note".into(), Value::String("a note field".into()));
        }
        let is_folder = depth < 8 && rng.next_range(3) != 0;
        let child_count = if is_folder { rng.next_range(4) as usize } else { 0 };
        let children: Vec<Value> = (0..child_count).map(|_| random_node(rng, depth + 1, id)).collect();
        obj.insert("children".into(), Value::Array(children));
        Value::Object(obj)
    }

    fn random_tree(seed: u32) -> String {
        let mut rng = XorShift32::new(seed);
        let mut id = 0u32;
        let n = 1 + rng.next_range(6) as usize;
        let entries: Vec<Value> = (0..n).map(|_| random_node(&mut rng, 0, &mut id)).collect();
        serde_json::json!({ "slug": "rand", "content_types": ["video"], "entries": entries }).to_string()
    }

    #[test]
    fn deep_round_trip_random_trees_is_identity() {
        const MAX_BYTES: usize = 40_000;
        // NIP-44's 65_408-byte plaintext cap (M2 gotcha) vs our LISTING_MAX_BYTES of 40_000 — every
        // published part must clear both, with headroom.
        const NIP44_PLAINTEXT_CAP: usize = 65_408;
        for seed in 1..=50u32 {
            let tree = random_tree(seed);
            let parts = split_listing("rand", &tree, MAX_BYTES)
                .unwrap_or_else(|e| panic!("seed {seed}: split failed: {e}"));
            for p in &parts {
                assert!(
                    p.json.len() <= MAX_BYTES,
                    "seed {seed}: part {} is {} bytes > {MAX_BYTES}",
                    p.d_tag,
                    p.json.len()
                );
                assert!(
                    p.json.len() <= NIP44_PLAINTEXT_CAP,
                    "seed {seed}: part {} is {} bytes > the NIP-44 plaintext cap",
                    p.d_tag,
                    p.json.len()
                );
            }
            let payloads: Vec<String> = parts.iter().map(|p| p.json.clone()).collect();
            let restitched = restitch_listing(&payloads)
                .unwrap_or_else(|e| panic!("seed {seed}: restitch failed: {e}"));
            assert_eq!(restitched, normalize(&tree).unwrap(), "seed {seed}: restitch must reproduce the tree");
        }
    }

    #[test]
    fn split_is_deterministic_byte_identical_parts() {
        let original = single_huge_entry();
        let a = split_listing("hoard", &original, 3000).unwrap();
        let b = split_listing("hoard", &original, 3000).unwrap();
        assert_eq!(a.len(), b.len(), "same input+budget must produce the same part count");
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.d_tag, y.d_tag);
            assert_eq!(x.json, y.json, "same input+budget must produce byte-identical parts");
        }
    }

    #[test]
    fn unsplittable_leaf_errors_clearly() {
        // A single file two levels deep whose own metadata alone exceeds the budget — no
        // shell/recurse can help (it's a leaf), so this must surface the same clear "too large"
        // error a top-level oversize leaf would, not an opaque relay rejection downstream.
        let huge_note = "x".repeat(2000);
        let tree = serde_json::json!({
            "slug": "hoard2",
            "entries": [
                { "name": "Movies", "children": [
                    { "name": "Sub", "children": [
                        { "name": "huge.bin", "note": huge_note, "children": [] }
                    ] }
                ] }
            ],
        })
        .to_string();
        match split_listing("hoard2", &tree, 512) {
            Err(NetError::Split(m)) => assert!(m.contains("too large"), "expected oversize error, got: {m}"),
            other => panic!("expected a clear oversize error, got {other:?}"),
        }
    }

    #[test]
    fn part_count_over_cap_and_index_over_budget_error_at_write_time() {
        // (a) Forcing more chunks than MAX_LISTING_PARTS must be refused before publishing that
        // many events — a write-side DoS/cost guard.
        let entries: Vec<Value> = (0..(MAX_LISTING_PARTS + 200))
            .map(|i| serde_json::json!({ "name": format!("f{i}") }))
            .collect();
        let tree = serde_json::json!({ "slug": "manyparts", "entries": entries }).to_string();
        match split_listing("manyparts", &tree, 90) {
            Err(NetError::Split(m)) => assert!(m.contains("cap"), "got: {m}"),
            other => panic!("expected a part-count-cap rejection, got {other:?}"),
        }

        // (b) Metadata alone (no entries) too large to fit the index even after moving entries out.
        let padding = "y".repeat(2000);
        let tree2 = serde_json::json!({
            "slug": "bigmeta",
            "padding": padding,
            "entries": [ {"name": "a"}, {"name": "b"} ],
        })
        .to_string();
        match split_listing("bigmeta", &tree2, 100) {
            Err(NetError::Split(m)) => assert!(m.contains("index"), "got: {m}"),
            other => panic!("expected an index-over-budget rejection, got {other:?}"),
        }
    }

    #[test]
    fn v1_payloads_still_restitch() {
        // Hand-written literal v1 fixtures (no `parts_v` key at all) — protects pre-M13 relay
        // listings: a real listing published before this milestone must still restitch under the
        // new dispatcher.
        let index = r#"{"slug":"legacy","content_types":["video"],"split":true,"parts":2}"#;
        let part0 = r#"{"part":0,"parts":2,"entries":[{"name":"a"}]}"#;
        let part1 = r#"{"part":1,"parts":2,"entries":[{"name":"b"}]}"#;
        let restitched =
            restitch_listing(&[index.to_string(), part0.to_string(), part1.to_string()]).unwrap();
        let expected = normalize(
            &serde_json::json!({
                "slug": "legacy", "content_types": ["video"],
                "entries": [{"name":"a"}, {"name":"b"}]
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(restitched, expected);
    }

    #[test]
    fn unknown_parts_version_refused() {
        let index = r#"{"slug":"future","split":true,"parts_v":3,"part_count":0,"parts":[]}"#;
        match restitch_listing(&[index.to_string()]) {
            Err(NetError::Split(m)) => assert!(m.contains("unknown parts version"), "got: {m}"),
            other => panic!("expected an unknown-version rejection, got {other:?}"),
        }
    }

    #[test]
    fn stale_unreferenced_part_is_ignored() {
        let original = single_huge_entry();
        let parts = split_listing("hoard", &original, 3000).unwrap();
        let mut fetched = payloads(&parts);
        // A stray payload whose hash matches no index slot — e.g. a superseded part from a prior
        // publish of the same slug. The spec's stale rule: ignored, not an error.
        fetched
            .push(serde_json::json!({ "entries": [], "mount": [], "part": 999, "parts_v": 2 }).to_string());
        let restitched = restitch_listing(&fetched).unwrap();
        assert_eq!(
            restitched,
            normalize(&original).unwrap(),
            "a stale unreferenced part must not affect the tree"
        );
    }

    /// Build a hand-crafted v2 family from raw content-part JSON strings (each already carrying its
    /// own `part`/`mount`/`parts_v`/`entries`), computing the index's sha256 slots from their actual
    /// bytes. Lets the hostile tests below construct a precise, minimal fixture and corrupt one
    /// part's mount/marker directly, instead of reverse-engineering `pack_tree`'s real output.
    fn v2_family(slug: &str, content_parts: &[String]) -> Vec<String> {
        let slots: Vec<Value> = content_parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                serde_json::json!({ "part_d": format!("{slug}#part{i}"), "sha256": sha256_hex(p.as_bytes()) })
            })
            .collect();
        let index = serde_json::json!({
            "slug": slug, "split": true, "parts_v": 2, "part_count": content_parts.len(), "parts": slots,
        })
        .to_string();
        let mut out = vec![index];
        out.extend(content_parts.iter().cloned());
        out
    }

    fn content_part(mount: &[usize], part: usize, entries: Value) -> String {
        serde_json::json!({ "entries": entries, "mount": mount, "part": part, "parts_v": 2 }).to_string()
    }

    #[test]
    fn restitch_rejects_mount_out_of_range() {
        let a = content_part(&[], 0, serde_json::json!([{"name":"A","children":[]}]));
        let b = content_part(&[9], 1, serde_json::json!([{"name":"orphan"}])); // root has only index 0
        let family = v2_family("hand", &[a, b]);
        match restitch_listing(&family) {
            Err(NetError::Split(m)) => assert!(m.contains("invalid mount path"), "got: {m}"),
            other => panic!("expected a mount-out-of-range rejection, got {other:?}"),
        }
    }

    #[test]
    fn restitch_rejects_mount_through_a_file_node() {
        let a = content_part(&[], 0, serde_json::json!([{"name":"leaf.txt"}])); // no `children`: a file
        let b = content_part(&[0], 1, serde_json::json!([{"name":"orphan"}])); // tries to descend into it
        let family = v2_family("hand", &[a, b]);
        match restitch_listing(&family) {
            Err(NetError::Split(m)) => assert!(m.contains("invalid mount path"), "got: {m}"),
            other => panic!("expected a mount-through-a-file rejection, got {other:?}"),
        }
    }

    #[test]
    fn restitch_rejects_mount_deeper_than_cap() {
        let deep_mount: Vec<usize> = (0..(MAX_MOUNT_DEPTH + 1)).collect();
        let a = content_part(&deep_mount, 0, serde_json::json!([{"name":"x"}]));
        let family = v2_family("hand", &[a]);
        match restitch_listing(&family) {
            Err(NetError::Split(m)) => assert!(m.contains("depth"), "got: {m}"),
            other => panic!("expected a mount-depth-cap rejection, got {other:?}"),
        }
    }

    #[test]
    fn restitch_rejects_total_size_over_cap() {
        let padding = "z".repeat(MAX_RESTITCHED_BYTES + 1024);
        let a = content_part(&[], 0, serde_json::json!([{"name":"x","note":padding}]));
        let family = v2_family("hand", &[a]);
        match restitch_listing(&family) {
            Err(NetError::Split(m)) => assert!(m.contains("cap"), "got: {m}"),
            other => panic!("expected a restitched-size-cap rejection, got {other:?}"),
        }
    }

    #[test]
    fn restitch_rejects_internal_part_marker_mismatch() {
        // Slot 0's payload internally claims to be part 1 — the index's slot position and the
        // payload's own `part` marker must agree (sanity against the sha256-keyed slotting).
        let a = content_part(&[], 1, serde_json::json!([{"name":"x"}])); // wrong: claims part 1, is slot 0
        let family = v2_family("hand", &[a]);
        match restitch_listing(&family) {
            Err(NetError::Split(m)) => assert!(m.contains("mismatch"), "got: {m}"),
            other => panic!("expected an internal-part-mismatch rejection, got {other:?}"),
        }
    }

    #[test]
    fn sibling_chunks_at_same_mount_graft_in_part_order() {
        // Two content parts both mounted at the root ([]) — legal (a sibling run split across
        // parts); they must graft in ASCENDING part order, not fetch/arrival order.
        let a = content_part(&[], 0, serde_json::json!([{"name":"first"}]));
        let b = content_part(&[], 1, serde_json::json!([{"name":"second"}]));
        let family = v2_family("hand", &[a.clone(), b.clone()]);
        // Feed the payloads to restitch in REVERSE arrival order.
        let reversed = vec![family[0].clone(), b, a];
        let restitched = restitch_listing(&reversed).unwrap();
        let v: Value = serde_json::from_str(&restitched).unwrap();
        let names: Vec<&str> =
            v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["first", "second"], "siblings at the same mount must graft in part order");
    }
}
