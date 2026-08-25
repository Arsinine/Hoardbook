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

use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::Deserialize;

use crate::error::HbError;
use crate::manifest::ManifestEnvelope;

/// **The transport ceiling — 16 MiB, fixed.** Frozen at launch (`wire_freeze`): a peer that
/// refuses at 16 MiB and one that refuses at some other number would disagree about what is
/// deliverable, so this is a fixed-now-or-never value and is pinned as a wire constant. Raised
/// from the original 8 MiB (owner ruling 2026-07-30) to 16 MiB (owner ruling 2026-08-19) —
/// **the cost of raising it is real, not free**: a peer still on the old build refuses (with an
/// honest `PayloadTooLarge`, never a crash or corruption) a >8 MiB manifest an upgraded peer
/// sends, until it upgrades too.
///
/// **Derived, not picked.** The ceiling is itself a size cap, so the launch objection that
/// motivated M18 applies to it recursively: set it too low and the day-1 embarrassment simply
/// relocates one layer down. The original estimate (~70 bytes of entry JSON, NIP-44's measured
/// ~1.56× bucket-padded expansion) put 100k files at ≈11 MB — **measured reality runs denser**:
/// a real 108,045-file collection (a game library, `C:\Games`, 2026-08-19) sealed to
/// **17,629,824 bytes** ciphertext, ≈16.3 MB scaled to exactly 100,000 files. Never extrapolate
/// the padding factor linearly, and don't trust the back-of-envelope estimate over a measurement.
///
/// **The 2026-07-30 "human browseability" premise doesn't hold for every content type.** It's
/// right for a media library (a person scans filenames to decide what to watch/read/hear) but
/// wrong for software: a game install's files aren't independently meaningful browsable units —
/// the collection *as a whole* is proof-by-stake (proof the software is real and complete, not
/// an empty folder), never something scanned file-by-file. File count for this category tracks
/// disk contents, not human attention, so the original band (1–5 MB, ~10k files) undersized
/// exactly the content type most likely to be legitimately large. hb-app (a downstream crate,
/// not reachable from here) pairs this raise with a companion **100,000-item cap per
/// collection** (`MAX_COLLECTION_ITEMS`, enforced at scan time in `commands/collection.rs`) — a
/// file-COUNT guard against a different failure shape (many-tiny-files: pathological scan/UI/
/// part-count cost) than this byte ceiling guards against (large-content-per-file); the two
/// decouple exactly for a game library's shape (many small files) and are meant to be read
/// together.
///
/// `MAX_LISTING_PARTS = 4096` would permit ~268 MB at the 65,408-byte-per-part split budget, and
/// a ceiling that close to it would make this mechanism decoration rather than enforcement — a
/// wire payload indistinguishable in scale from moving files defeats the point of INV-4′ even if
/// every test asserting it stays green.
///
/// **The cliff is principled, not a capacity failure:** past the browseable limit the full tree
/// is not the useful artifact anyway (search is), so declining to carry it is the honest answer.
/// Above the ceiling the route is export — say so, in the error and in the UI.
pub const MANIFEST_MAX_TRANSPORT_BYTES: usize = 16 * 1024 * 1024;

/// **Companion cap to the byte ceiling: the most parts an inbound envelope may declare.**
///
/// The byte ceiling alone bounds the *frame*, not the *work*. A legal 16 MiB envelope can be almost
/// entirely `"",""...` — millions of empty `ciphertexts` entries with a `manifest_sha256` that
/// honestly matches them. Each costs ~3 bytes of JSON but a `String` is 24 bytes plus its
/// allocation, so peak memory is a multiple of the frame it arrived in. `verify_integrity` does not
/// bound the count (it only requires non-empty), so nothing else catches this.
///
/// 4096 is not a new number: it is `hb-net::MAX_LISTING_PARTS`, the cap the **producer** already
/// enforces, so no manifest Hoardbook can legitimately build is refused here. Duplicated as a
/// literal rather than imported because hb-core does not depend on hb-net; `parts_cap_matches_the_
/// producer_cap` in `wire_freeze` is what keeps the two honest.
///
/// **What this does and does not fix:** the cap is enforced *during* deserialization, not after —
/// `from_wire` parses through a [`WireEnvelope`] whose `ciphertexts` field is a [`BoundedParts`]
/// `SeqAccess` walk that stops retaining elements past this count. So the transient parse
/// allocation is bounded too, not just the payload a caller goes on to hold: a 16 MiB frame padded
/// to ~5.59M empty parts is drained and refused without ever materialising that many `String`s.
pub const MANIFEST_MAX_TRANSPORT_PARTS: usize = 4096;

/// The bounded-deserialization gate [`ManifestPayload::from_wire`] parses through instead of
/// straight into [`ManifestEnvelope`]. It mirrors the envelope's field list exactly (frozen by
/// `wire_freeze`) so only the *parse* changes — the bytes on the wire are byte-for-byte identical.
/// The one field that differs in *type* is `ciphertexts`: [`BoundedParts`] walks the array with a
/// `SeqAccess` that stops retaining elements past [`MANIFEST_MAX_TRANSPORT_PARTS`], so a hostile
/// frame padded to ~5.59M empty parts is drained and rejected without materialising that many
/// `String`s. Keep this list in lockstep with `ManifestEnvelope`'s fields.
#[derive(Deserialize)]
struct WireEnvelope {
    manifest_v: u8,
    slug: String,
    author_npub: String,
    crypto_v: u8,
    snapshot_fingerprint: String,
    created_at: u64,
    manifest_sha256: String,
    author_sig: String,
    ciphertexts: BoundedParts,
}

/// A `ciphertexts` array parsed with a bounded `SeqAccess`: retains at most
/// [`MANIFEST_MAX_TRANSPORT_PARTS`] elements, draining (and discarding) any beyond that, and
/// records whether the sequence overflowed in [`BoundedParts::over`]. Draining rather than bailing
/// keeps the parse transient bounded — the element being read is the only one ever held.
struct BoundedParts {
    parts: Vec<String>,
    over: bool,
}

impl<'de> Deserialize<'de> for BoundedParts {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PartsVisitor;

        impl<'de> Visitor<'de> for PartsVisitor {
            type Value = BoundedParts;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a JSON array of manifest ciphertext strings")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut parts =
                    Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MANIFEST_MAX_TRANSPORT_PARTS));
                let mut over = false;
                while let Some(s) = seq.next_element::<String>()? {
                    if parts.len() < MANIFEST_MAX_TRANSPORT_PARTS {
                        parts.push(s);
                    } else {
                        over = true;
                        // Drain but discard: never retain more than the cap.
                    }
                }
                Ok(BoundedParts { parts, over })
            }
        }

        deserializer.deserialize_seq(PartsVisitor)
    }
}

/// The **only** payload Hoardbook's transport plane will carry: the serialized bytes of a
/// [`ManifestEnvelope`] that has passed `verify_integrity` and is within
/// [`MANIFEST_MAX_TRANSPORT_BYTES`].
///
/// Deliberately opaque — the inner `Vec<u8>` is private and there is no constructor that accepts
/// arbitrary bytes without validating them. See the module docs for why this is mechanism 1 of
/// INV-4′ rather than a convenience wrapper.
///
/// **⚠ THIS TYPE MUST NEVER DERIVE `Serialize`/`Deserialize`, AND THE ORIGINAL SHIPPED VERSION DID.**
/// A derived `Deserialize` is a **public constructor**: a private tuple field does not stop
/// `serde_json::from_str::<ManifestPayload>("[137,80,78,71]")` from handing you a payload of PNG
/// bytes, which bypasses `seal`, `from_wire`, `verify_integrity` **and** the byte ceiling in one
/// step. Confirmed by execution, not inspection — an 8 MiB + 1 byte payload was constructed that
/// way. That single derive defeated mechanisms 1 and 2 simultaneously while every test stayed green,
/// because no test tried the bypass.
///
/// Nothing ever needed the derives: the plane writes [`Self::as_bytes`] onto the wire and reads
/// bytes back through [`Self::from_wire`], so the type is never itself a serde field. The lesson is
/// general — **on a newtype whose whole purpose is that its constructors validate, a serde derive is
/// an unvalidated constructor.** The CI sweep now greps for their return (mechanism 3).
#[derive(Clone, PartialEq, Eq)]
pub struct ManifestPayload(Vec<u8>);

/// Hand-written so a payload logs as its size, not its contents. A derived `Debug` prints every
/// byte, which for this type means **up to 8 MiB of remote-supplied data into a log line or a test
/// failure** — the derive turned one assertion message into 848 KB of decimal byte values.
impl std::fmt::Debug for ManifestPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ManifestPayload({} bytes)", self.0.len())
    }
}

/// Parse inbound bytes into a [`ManifestEnvelope`], bounding the part count *during*
/// deserialization via [`WireEnvelope`]/[`BoundedParts`] so the transient parse allocation can't
/// outgrow the frame by the `String` expansion factor. Only the *parse* differs from
/// `serde_json::from_slice::<ManifestEnvelope>` — the wire shape is untouched.
fn parse_envelope(bytes: &[u8]) -> Result<ManifestEnvelope, HbError> {
    let wire: WireEnvelope = serde_json::from_slice(bytes)?;
    let BoundedParts { parts, over } = wire.ciphertexts;
    if over {
        return Err(HbError::InvalidManifest(format!(
            "manifest declares more than {MANIFEST_MAX_TRANSPORT_PARTS} parts — no manifest this \
             app can build has that many"
        )));
    }
    Ok(ManifestEnvelope {
        manifest_v: wire.manifest_v,
        slug: wire.slug,
        author_npub: wire.author_npub,
        crypto_v: wire.crypto_v,
        snapshot_fingerprint: wire.snapshot_fingerprint,
        created_at: wire.created_at,
        manifest_sha256: wire.manifest_sha256,
        author_sig: wire.author_sig,
        ciphertexts: parts,
    })
}

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
        // refused here, so no caller can obtain a `ManifestPayload` that isn't one. The envelope is
        // parsed through [`WireEnvelope`], which bounds the part count *during* deserialization
        // rather than after a full `Vec<String>` has been materialised.
        let envelope = parse_envelope(&bytes)?;
        envelope.verify_integrity()?;
        Ok(Self(bytes))
    }

    /// The slug the envelope inside claims to describe.
    ///
    /// Exposed so the transport can bind the payload to the **ticket** that authorized it: without
    /// this check `ManifestSource::payload(slug)` is a naming convention, and a source that answers
    /// with the wrong collection is served and accepted as self-consistent. A ticket names one
    /// collection; the bytes must agree.
    pub fn declared_slug(&self) -> Result<String, HbError> {
        Ok(self.envelope()?.slug)
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

    /// **The part cap bounds the work an in-budget frame can demand.** A manifest well under 8 MiB
    /// can still declare a colossal number of parts — each `String` costs far more in memory than in
    /// JSON — and `verify_integrity` only requires the vector to be non-empty. The envelope here is
    /// **honestly self-consistent**: built by the real constructor, so its `manifest_sha256` genuinely
    /// matches its parts. It is refused on the count alone, which is the point.
    #[test]
    fn an_envelope_declaring_more_parts_than_the_producer_can_build_is_refused() {
        let id = Identity::generate();
        let parts: Vec<String> =
            (0..=MANIFEST_MAX_TRANSPORT_PARTS).map(|i| format!(r#"{{"p":{i}}}"#)).collect();
        let env =
            build_manifest_envelope(&id, "slug", &[3u8; 32], "fp", 1_700_000_000, &parts).unwrap();
        let bytes = serde_json::to_vec(&env).unwrap();
        assert!(
            bytes.len() < MANIFEST_MAX_TRANSPORT_BYTES,
            "the fixture must be UNDER the byte ceiling, or it would be refused for the wrong \
             reason — got {} bytes",
            bytes.len()
        );
        match ManifestPayload::from_wire(bytes) {
            Err(HbError::InvalidManifest(msg)) => {
                assert!(msg.contains("parts"), "the refusal names the part count, got: {msg}");
            }
            other => panic!("expected InvalidManifest, got {other:?}"),
        }
    }

    /// The bounded `SeqAccess` retains at most the cap and flags overflow, instead of materialising
    /// the whole array and checking its length afterwards — that "check after the expensive parse"
    /// shape is exactly CWE-770. This pins the mechanism directly: a sequence over the cap must come
    /// back with only the cap retained and `over` set.
    #[test]
    fn bounded_parts_deserialize_retains_only_the_cap() {
        let n = MANIFEST_MAX_TRANSPORT_PARTS + 3;
        let json = format!("[{}]", "\"\",".repeat(n).trim_end_matches(','));
        let parts: BoundedParts = serde_json::from_str(&json).unwrap();
        assert!(parts.over, "a sequence over the cap must set the overflow flag");
        assert_eq!(
            parts.parts.len(),
            MANIFEST_MAX_TRANSPORT_PARTS,
            "only the cap is retained — the rest must be drained, not held"
        );
    }

    /// End-to-end: a frame well under the byte ceiling but padded to hundreds of thousands of empty
    /// `ciphertexts` entries is refused on the part count alone (never reaching `verify_integrity`),
    /// and the rejection happens during deserialization rather than after a full `Vec<String>`.
    #[test]
    fn a_frame_padded_with_many_empty_parts_is_refused() {
        let n = MANIFEST_MAX_TRANSPORT_PARTS * 100; // ~410k parts — a fraction of the ~5.59M max, so the fixture stays fast
        let mut parts_json = String::with_capacity(n * 3);
        for _ in 0..n {
            parts_json.push_str("\"\",");
        }
        parts_json.pop(); // drop the trailing comma
        let json = format!(
            r#"{{"manifest_v":1,"slug":"s","author_npub":"n","crypto_v":1,"snapshot_fingerprint":"f","created_at":0,"manifest_sha256":"0","author_sig":"0","ciphertexts":[{}]}}"#,
            parts_json
        );
        let bytes = json.into_bytes();
        assert!(
            bytes.len() < MANIFEST_MAX_TRANSPORT_BYTES,
            "the fixture must be UNDER the byte ceiling, or it would be refused for the wrong reason"
        );
        match ManifestPayload::from_wire(bytes) {
            Err(HbError::InvalidManifest(msg)) => {
                assert!(msg.contains("parts"), "the refusal names the part count, got: {msg}");
            }
            other => panic!("expected InvalidManifest, got {other:?}"),
        }
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

