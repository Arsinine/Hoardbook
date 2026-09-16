// Positive control fixture for the manifest-cache single-writer sweep in
// .github/workflows/ci.yml (step "manifest-cache single-writer sweep — Carrier 4's
// public-only fence has exactly one writer"). NOT compiled, NOT under crates/hb-app/src,
// so the production sweep never scans it — the self-test step in ci.yml runs the SAME
// pattern-and-filter pipeline against it and asserts it REDS.
//
// It holds the import-then-bare-call evasions from the 2026-09-15 scan (QURATOR-233): the
// retired pattern probed files other than manifest_cache.rs for the QUALIFIED call only, and
// a `use` line imports the function without ever spelling `manifest_cache::put(` — the
// sweep's own comment conceded imports were the remaining form and then failed to scan for
// them. Both import forms below must be flagged; the plain read-oriented module import must
// NOT be (flagging it would red the build on today's manifest_source.rs).

use crate::manifest_cache;
use crate::manifest_cache::put;
use crate::manifest_cache as mc;

pub fn evade_import_then_bare_call(key: &str) {
    put(key);
}

pub fn evade_module_alias(key: &str) {
    mc::put(key);
}
