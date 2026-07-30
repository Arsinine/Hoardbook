//! **INV-4′ mechanisms 1 and 2** — the only thing Hoardbook's transport plane is allowed to carry.
//!
//! > **INV-4′ — Hoardbook moves no collection files.** A transport plane exists and is structurally
//! > limited to manifest payloads.
//!
//! The retired rule ("Hoardbook moves no bytes") was prose, and the shipped code already
//! contradicted it — M16's big-relay carrier moves manifest bytes today. M18 replaces the prose
//! with a line that is true and *enforced*. Four mechanisms enforce it; **any one alone is a
//! comment**. This module is mechanisms 1 and 2:
//!
//! 1. **Type-level.** [`ManifestPayload`] is the transport's only send type, and its only
//!    constructors are [`ManifestPayload::seal`] (from an existing [`ManifestEnvelope`]) and
//!    [`ManifestPayload::from_wire`] (bytes that *parse and verify* as one). There is deliberately
//!    no `new(bytes)`, no `From<Vec<u8>>`, no public field, and nothing anywhere that takes a
//!    `Path` or a `File`. **The plane cannot be handed a collection file because the signature has
//!    nowhere to put one** — the same shape as `seal_takes_no_browse_key_only_recipient_pubkeys`
//!    (INV-2, INVARIANT_AUDIT.md:18).
//! 2. **Byte ceiling.** [`MANIFEST_MAX_TRANSPORT_BYTES`] is checked on send *and* again on
//!    receive, and over-cap is a **rejection, not a truncation** — the error names export as the
//!    route out. Manifests are bounded (chunked `MANIFEST_V = 2` parts); collection files are not.
//!
//! (Mechanism 3 is the rewritten CI sweep; mechanism 4 is the red test at the bottom of this file —
//! handing the plane a non-manifest payload must *fail*, and the test asserts the failure rather
//! than merely exercising the happy path.)
//!
//! **Why a serialized envelope and not `&ManifestEnvelope` directly.** The receive side has bytes,
//! not a type — the whole risk lives in what it accepts. Making the send and receive sides meet at
//! the *same* newtype means the receiver's validation is not a courtesy the caller may skip: there
//! is no way to obtain a `ManifestPayload` from arbitrary bytes without it having parsed and passed
//! `verify_integrity`. Author verification stays with the caller ([`ManifestEnvelope::verify_author`]),
//! which needs the expected author's key and so cannot live at this layer.

use serde::{Deserialize, Serialize};

use crate::error::HbError;
use crate::manifest::ManifestEnvelope;

/// **The transport ceiling — 8 MiB, fixed.** Frozen at launch (`wire_freeze`): a peer that refuses
/// at 8 MiB and one that refuses at 16 would disagree about what is deliverable, so this is a
/// fixed-now-or-never value and is pinned as a wire constant.
///
/// **Derived, not picked** (owner ruling 2026-07-30). The ceiling is itself a size cap, so the
/// launch objection that motivated M18 applies to it recursively: set it too low and the day-1
/// embarrassment simply relocates one layer down. At ~70 bytes of entry JSON and NIP-44's measured
/// **~1.56× bucket-padded** expansion (never extrapolate linearly), 10k files ≈ 1 MB and 100k ≈
/// 11 MB. The owner's stated reasonable band is 1–5 MB, because the binding constraint is **human
/// attention, not disk** — "at 10,000 files we are already reaching the human limit of what is
/// considered browseable". That is why this needs no version negotiation: bandwidth gets cheaper,
/// but nobody's ability to scan 100,000 filenames improves, so a ceiling pinned to a human limit is
/// stable.
///
/// 8 MiB sits above that band with headroom for deep nesting and long filenames (~75k files) and
/// unambiguously below "file transfer". `MAX_LISTING_PARTS = 4096` would permit ~160 MB, and a
/// ceiling near *that* would make this mechanism decoration rather than enforcement — 100 MB on the
/// wire is indistinguishable from moving files.
///
/// **The cliff is principled, not a capacity failure:** past the browseable limit the full tree is
/// not the useful artifact anyway (search is), so declining to carry it is the honest answer.
/// Above the ceiling the route is export — say so, in the error and in the UI.
pub const MANIFEST_MAX_TRANSPORT_BYTES: usize = 8 * 1024 * 1024;

/// The **only** payload Hoardbook's transport plane will carry: the serialized bytes of a
/// [`ManifestEnvelope`] that has passed `verify_integrity` and is within
/// [`MANIFEST_MAX_TRANSPORT_BYTES`].
///
/// Deliberately opaque — the inner `Vec<u8>` is private and there is no constructor that accepts
/// arbitrary bytes without validating them. See the module docs for why this is mechanism 1 of
/// INV-4′ rather than a convenience wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPayload(Vec<u8>);

impl ManifestPayload {
    /// **Send side.** Serialize an envelope and bound it. Errs with [`HbError::PayloadTooLarge`]
    /// when the result is over the ceiling — a rejection, never a truncation: a truncated manifest
    /// would fail its own `manifest_sha256` at the far end anyway, so silently shipping one would
    /// turn a clear "too big, export it" into an opaque corruption.
    pub fn seal(envelope: &ManifestEnvelope) -> Result<Self, HbError> {
        let bytes = serde_json::to_vec(envelope)?;
        Self::bound(bytes.len())?;
        Ok(Self(bytes))
    }

    /// **Receive side.** The second of the two ceiling checks, and the gate every inbound byte
    /// passes. Order is deliberate: **bound first, parse second** — a hostile 500 MB blob is
    /// refused on its declared size before anything tries to deserialize it.
    ///
    /// Note for the framing layer (W1): this is the *second* check, not the first. The wire framing
    /// must refuse a declared length over the ceiling **before reading it into memory** — by the
    /// time bytes reach here they are already allocated.
    pub fn from_wire(bytes: Vec<u8>) -> Result<Self, HbError> {
        Self::bound(bytes.len())?;
        // Mechanism 1 on the receive side: arbitrary bytes are not a payload. A collection file, a
        // zip, a JPEG — anything that is not a structurally valid, self-consistent envelope — is
        // refused here, so no caller can obtain a `ManifestPayload` that isn't one.
        let envelope: ManifestEnvelope = serde_json::from_slice(&bytes)?;
        envelope.verify_integrity()?;
        Ok(Self(bytes))
    }

    /// The wire bytes. Read-only: there is no `as_mut`, so a payload cannot be edited into
    /// something else after it has been validated.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Recover the envelope. Infallible in practice — both constructors have already parsed it —
    /// but returns `Result` rather than unwrapping, because a panic in the receive path is a
    /// remote-triggerable crash and this type exists to be fed hostile input.
    pub fn envelope(&self) -> Result<ManifestEnvelope, HbError> {
        Ok(serde_json::from_slice(&self.0)?)
    }

    fn bound(len: usize) -> Result<(), HbError> {
        if len > MANIFEST_MAX_TRANSPORT_BYTES {
            return Err(HbError::PayloadTooLarge { declared: len, max: MANIFEST_MAX_TRANSPORT_BYTES });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::manifest::build_manifest_envelope;

    fn envelope() -> ManifestEnvelope {
        let id = Identity::generate();
        let browse_key = [3u8; 32];
        build_manifest_envelope(&id, "slug", &browse_key, "fp-abc", 1_700_000_000, &[
            r#"{"part":0,"entries":[]}"#.to_string(),
        ])
        .unwrap()
    }

    #[test]
    fn a_real_envelope_round_trips_through_the_payload() {
        let env = envelope();
        let payload = ManifestPayload::seal(&env).unwrap();
        let back = ManifestPayload::from_wire(payload.as_bytes().to_vec()).unwrap();
        assert_eq!(back.envelope().unwrap(), env, "the envelope survives the transport payload");
    }

    /// **INV-4′ mechanism 4 — the red test.** The point of the plane is what it *refuses*. Each of
    /// these is a shape a collection file (or an attacker) would actually take; none of them can
    /// become a `ManifestPayload`, so none of them can be handed to the transport.
    #[test]
    fn non_manifest_payloads_are_refused() {
        // A binary file — the exact thing INV-4′ exists to keep off the plane. (PNG magic.)
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xde, 0xad, 0xbe, 0xef];
        assert!(ManifestPayload::from_wire(png).is_err(), "a binary file is not a manifest");

        // Valid JSON that simply isn't an envelope.
        assert!(
            ManifestPayload::from_wire(br#"{"hello":"world"}"#.to_vec()).is_err(),
            "arbitrary JSON is not a manifest"
        );

        // Empty, and not-quite-JSON.
        assert!(ManifestPayload::from_wire(Vec::new()).is_err(), "nothing is not a manifest");
        assert!(ManifestPayload::from_wire(b"{".to_vec()).is_err(), "truncated JSON is refused");

        // The subtle one: structurally an envelope, but self-inconsistent. `from_wire` runs
        // `verify_integrity`, so a tampered body cannot ride the plane even though it deserializes.
        let mut env = envelope();
        env.ciphertexts.push("injected-part".into());
        let tampered = serde_json::to_vec(&env).unwrap();
        assert!(
            ManifestPayload::from_wire(tampered).is_err(),
            "an envelope whose parts no longer match its manifest_sha256 is refused"
        );
    }

    /// The ceiling rejects rather than truncates, on **both** sides, and the error carries the
    /// numbers a caller needs to say "export it instead".
    #[test]
    fn the_ceiling_rejects_over_cap_on_send_and_on_receive() {
        // Receive side: over-cap bytes are refused on size, before any parse — note this blob is
        // not even valid JSON, so a size check that ran second would report the wrong reason.
        let over = vec![b'x'; MANIFEST_MAX_TRANSPORT_BYTES + 1];
        match ManifestPayload::from_wire(over) {
            Err(HbError::PayloadTooLarge { declared, max }) => {
                assert_eq!(declared, MANIFEST_MAX_TRANSPORT_BYTES + 1);
                assert_eq!(max, MANIFEST_MAX_TRANSPORT_BYTES);
            }
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }

        // Send side: an envelope that serializes over the ceiling is refused, not shipped short.
        let mut env = envelope();
        let filler = "y".repeat(MANIFEST_MAX_TRANSPORT_BYTES / 4);
        env.ciphertexts = vec![filler.clone(), filler.clone(), filler.clone(), filler.clone(), filler];
        assert!(
            matches!(ManifestPayload::seal(&env), Err(HbError::PayloadTooLarge { .. })),
            "an over-cap envelope is rejected on send, never truncated"
        );

        // And the rejection names the route out — the cliff is principled, not a capacity failure.
        let msg = HbError::PayloadTooLarge { declared: 9_000_000, max: MANIFEST_MAX_TRANSPORT_BYTES }
            .to_string();
        assert!(
            msg.to_lowercase().contains("export"),
            "the rejection must point at export, got: {msg}"
        );
    }

    /// **Exactly at the ceiling is allowed; one byte over is not.** An off-by-one here is the
    /// difference between a documented cliff and an arbitrary one, and `wire_freeze` makes it
    /// unfixable without a fork — so pin the boundary itself, not just "big is refused".
    #[test]
    fn the_boundary_is_inclusive_at_exactly_the_ceiling() {
        assert!(
            ManifestPayload::bound(MANIFEST_MAX_TRANSPORT_BYTES).is_ok(),
            "exactly at the ceiling is deliverable"
        );
        assert!(
            ManifestPayload::bound(MANIFEST_MAX_TRANSPORT_BYTES + 1).is_err(),
            "one byte over is refused"
        );
    }
}
