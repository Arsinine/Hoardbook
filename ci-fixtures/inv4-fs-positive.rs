//! POSITIVE CONTROL — deliberately dangerous, never compiled, never imported.
//!
//! This is the exact evasion the 2026-08-23 security scan described (QURATOR-111 finding #5):
//! a `use std::fs;` import followed by bare `fs::…` calls contains none of the fully-qualified
//! tokens (`std::fs::`, `File::open`, …) the retired INV-4′ no-fs probe required, so the sweep
//! reported CLEAN while the transport module held real filesystem access. The self-test step in
//! .github/workflows/ci.yml runs the sweep's pattern against this file and asserts it REDS.
//!
//! The doc comments below name std::fs and `tokio::fs::File::open` on purpose: a module that
//! documents what it refuses to do names those things, and the sweep's code_only filter must
//! keep exempting them. The self-test's negative half asserts exactly that.

use std::fs;
use std::path::PathBuf;

fn leak() -> std::io::Result<()> {
    // The aliased call that must trip the probe.
    let manifest = fs::read_to_string("collection/manifest.hbmanifest")?;
    fs::write("/tmp/exfiltrated-manifest", manifest)?;
    Ok(())
}

fn leak_more() {
    let p = PathBuf::from("some/collection/file");
    let _ = File::open(&p);
}
