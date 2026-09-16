// Positive control fixture for the serde-derive probe inside the INV-4′ sweep in
// .github/workflows/ci.yml (step "INV-4′ sweep — the plane carries manifests, never
// collection files"). NOT compiled, NOT under crates/, so the production probe never scans
// it — the self-test step in ci.yml runs the SAME awk against it and asserts it REDS.
//
// It holds the two evasions from the 2026-09-15 scan (QURATOR-269) — a cfg_attr-wrapped
// derive (`all()` is always true, so the derive is live) and a derive indented inside a
// nested module, both of which the retired awk skipped — plus the shapes the probe must
// keep catching, plus the ONE shape it must keep exempting.

// EVASION 1 (QURATOR-269): cfg_attr with an always-true predicate. The retired awk treated
// every #-line that was not a plain derive as "some other attribute" and dropped it.
#[cfg_attr(all(), derive(serde::Deserialize))]
pub struct EvadesViaCfgAttr { pub b: Vec<u8> }

// EVASION 2 (QURATOR-269): leading whitespace before the derive — any type nested in a
// module. The retired anchor demanded the derive at column 0 and then RESET the accumulator
// on the indented line.
mod inner {
    #[derive(Serialize)]
    pub struct EvadesViaIndent { pub b: Vec<u8> }
}

// EVASION 3: the multi-line form of EVASION 1 — parens balance only on the closing line,
// so a close-on-any-paren heuristic drops the derive clause mid-attribute.
#[cfg_attr(
    all(),
    derive(serde::Deserialize)
)]
pub struct EvadesViaMultilineCfgAttr { pub b: Vec<u8> }

// STILL CAUGHT: any Serialize derive anywhere in the file is banned outright.
#[derive(Serialize)]
pub struct PlainSerialize { pub b: Vec<u8> }

// STILL CAUGHT: a Deserialize on a pub type — an unvalidated public constructor.
#[derive(Deserialize)]
pub struct PubDeserialize { pub b: Vec<u8> }

// STILL EXEMPT: the private Deserialize-only WireEnvelope mirror is the bounded-parse gate
// from_wire parses through. The probe must NOT flag this one; if it does, the exemption is
// gone and every legitimate change to the real transport_payload.rs reds the build.
#[derive(Deserialize)]
struct WireEnvelope { b: Vec<u8> }
