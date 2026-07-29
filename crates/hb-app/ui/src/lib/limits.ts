// Published-envelope metadata ceilings, mirrored from Rust.
//
// The source of truth is `crates/hb-core/src/types.rs` — the backend clamps to these values on
// save and on load, so these constants exist only to show the user the limit *before* the clamp
// fires. `limits.test.ts` parses the Rust file and fails if the two ever drift.
//
// Why there is a ceiling at all: a collection's metadata and its folder tree share one 40 KB
// publish budget, and `truncate_listing` measures the metadata first. Uncapped metadata starves
// the tree — far enough and the teaser publishes with no entries in it at all.

export const MAX_DESCRIPTION_CHARS = 255;
export const MAX_TAGS = 8;
export const MAX_TAG_CHARS = 32;
