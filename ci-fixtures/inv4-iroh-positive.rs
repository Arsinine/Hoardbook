// Positive control fixture for the iroh-surface enumeration inside the INV-4′ sweep in
// .github/workflows/ci.yml (step "INV-4′ sweep — the plane carries manifests, never
// collection files"). NOT compiled, NOT under crates/, so no production probe ever scans
// it — the self-test step in ci.yml runs the SAME enumeration grep against it directly.
//
// It holds the use-aliasing evasion from the 2026-09-15 scan (QURATOR-256): a use-statement
// aliasing the crate, followed by aliased paths, never contains the literal iroh path prefix
// the retired enumeration pattern depended on, so an aliased module escaped the "every file
// that touches iroh must be listed" check — and with it every no-filesystem/no-path probe
// downstream. The enumeration must list this file. (Prose here deliberately avoids spelling
// that prefix: the enumeration lists files by content, comments included.)

use iroh as quic;

pub fn aliased_dial() {
    let _endpoint = quic::Endpoint::builder();
}
