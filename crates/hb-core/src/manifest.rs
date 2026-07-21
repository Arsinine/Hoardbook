//! The shared **manifest envelope** (M16 W1) — one signed, browse-key-encrypted artifact carrying a
//! collection's *full* listing, so the v0.11.4 paywall teaser gains a "get the rest" path.
//!
//! **The manifest is not a new format.** The envelope's *plaintext* is the canonical normalized full
//! listing JSON — the exact bytes `hb_net::truncate_listing` received before it cropped a 40 KB
//! teaser. This module adds only a thin, self-describing wrapper so the *same* `split_listing` parts
//! can serve **two carriers**: a chunked family on a higher-cap relay (M16 W2) and an opaque file
//! couriered peer-to-peer by Mascara (M16 W4) — the file holds the encrypted parts inline
//! (`ciphertexts`), so it is bounded by the number of parts, not by one NIP-44 event's plaintext cap.
//! The crypto is the existing browse-key path (`encrypt_listing`/`decrypt_listing`, NIP-44 v2 under a
//! versioned HKDF); the trust is the existing secp256k1 identity.
//!
//! **Why a raw BIP-340 signature (`author_sig`) rather than a wrapping Nostr event.** A relay listing
//! is trusted because it is an author-signed `KIND_LISTING` event. The envelope, however, must be
//! verifiable as a *standalone file* — with no relay, no event, no `kind` framing to rebuild. So the
//! author signs a canonical, domain-separated digest of its own listing directly
//! (`schnorr(npub) over manifest_v‖created_at‖slug‖fingerprint‖manifest_sha256`), exactly as every
//! `KIND_LISTING` is signed by the Hoardbook npub over its own content. This touches **no Mascara
//! identity** (MAS-INV-1 unimplicated) — it is the hoarder attesting to their own listing.
//!
//! **Author binding = pubkey pinning.** The signature verifies only under the *browsed* author's
//! x-only key, so a valid envelope for peer A cannot be replayed while browsing peer B; and the
//! declared `author_npub` is pinned to the expected author before the signature is even checked (the
//! `binding::verify_binding` author-pin pattern). `slug` + `snapshot_fingerprint` inside the signed
//! digest prevent replay under another collection or snapshot.
//!
//! **Browse-key symmetric, not per-recipient** (M16 decision): access = holding the full `hbk1…`
//! share code, same as the teaser. It is the only choice that lets *one* ciphertext serve both a
//! relay event set and a file — per-recipient sealing on a relay would be N copies (INV-8 / relay
//! citizenship). Private collections never truncate and keep their per-recipient CEK wrap
//! (`priv_listing`); they are untouched here.
//!
//! **Staleness is surfaced, never silent.** The teaser and the manifest both carry
//! `snapshot_fingerprint`; a mismatch means the manifest is an older version of the tree —
//! [`ManifestEnvelope::matches_fingerprint`] lets the caller say "full list as of an older version —
//! ask again" rather than serve stale data.

use nostr::prelude::PublicKey;
use secp256k1::schnorr::Signature;
use secp256k1::{Keypair, Message, XOnlyPublicKey, SECP256K1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HbError;
use crate::identity::{parse_npub, Identity};
use crate::listing::{decrypt_listing, encrypt_listing, BrowseKey};
use crate::version::{check_crypto, CRYPTO_V};

/// Manifest-envelope schema version. Frozen at launch (see `wire_freeze`): an unknown value is
/// *recognised and refused*, never mis-decoded — the same forward-compat contract `SCHEMA_V` /
/// `CRYPTO_V` uphold. **v2** (M16 W4 residual) carries a *chunked* body (`ciphertexts`, split at the
/// per-part budget) so a `.hbmanifest` can hold a listing larger than one NIP-44 event; **v1** was the
/// pre-release single-`ciphertext` shape, superseded before any producer shipped (export landed at v2).
pub const MANIFEST_V: u8 = 2;

/// The domain tag prefixed onto the `author_sig` pre-image. The BIP-340 message is the SHA-256 of
/// `SIG_DOMAIN ‖ manifest_v ‖ created_at(8 LE) ‖ len(slug)‖slug ‖ len(fp)‖fp ‖ len(sha)‖sha` — a
/// length-prefixed, domain-separated encoding so no two distinct field tuples share a pre-image.
/// **Frozen at launch** (a change invalidates every signature already produced); pinned in
/// `wire_freeze`.
pub(crate) const SIG_DOMAIN: &[u8] = b"hoardbook/manifest-envelope/v1";

/// A signed, browse-key-encrypted **full-listing manifest**. Serializes to the `.hbmanifest` file
/// (M16 W4) and is the plaintext the big-relay carrier chunks (M16 W2). Every field is public so a
/// carrier can read `slug` / `snapshot_fingerprint` / `manifest_sha256` without decrypting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEnvelope {
    /// Envelope schema version (`MANIFEST_V`); an unknown version is refused on verify.
    pub manifest_v: u8,
    /// The collection slug (the listing's `d`-tag identifier).
    pub slug: String,
    /// The hoarder's Hoardbook `npub` (bech32). Pinned to the browsed author on verify.
    pub author_npub: String,
    /// The listing crypto version (`CRYPTO_V`) the `ciphertext` was sealed under — reused verbatim.
    pub crypto_v: u8,
    /// `hb_core::snapshot_fingerprint` of the full tree; matched against the teaser's for staleness.
    pub snapshot_fingerprint: String,
    /// Unix seconds the manifest was built.
    pub created_at: u64,
    /// Lowercase-hex SHA-256 over the ordered `ciphertexts` (length-prefixed concat — see
    /// [`sha256_parts`]); binds every part + their order into the signature.
    pub manifest_sha256: String,
    /// Lowercase-hex BIP-340 Schnorr signature over the canonical signing digest (see `SIG_DOMAIN`).
    pub author_sig: String,
    /// The browse-key-encrypted listing body, one `encrypt_listing(browse_key, part)` per
    /// `hb-net::split_listing` part (index + content parts; a single element when the listing fits one
    /// NIP-44 event). Restitched by the reader (`render_listing`) after decrypting every part.
    pub ciphertexts: Vec<String>,
}

/// Lowercase-hex SHA-256 over the ordered `ciphertexts`, length-prefixed so the digest binds the
/// exact number of parts AND their order — reordering, dropping, or injecting a part changes the hash
/// (and so fails the signature that covers it). The part count is prefixed for the same reason.
fn sha256_parts(ciphertexts: &[String]) -> String {
    let mut h = Sha256::new();
    h.update((ciphertexts.len() as u64).to_le_bytes());
    for ct in ciphertexts {
        h.update((ct.len() as u64).to_le_bytes());
        h.update(ct.as_bytes());
    }
    hex::encode(h.finalize())
}

/// The 32-byte BIP-340 message the author signs: a length-prefixed, domain-separated hash of the
/// envelope's identifying fields, including `created_at` (fixed-width, so it can't be rewritten while
/// still verifying — a tampered build time would flip the signature). `crypto_v` is deliberately
/// *not* in the pre-image (it matches the frozen schema): it is bound transitively — the ciphertext
/// was sealed under a specific crypto version, and flipping the field to another *known* version only
/// makes `decrypt_listing` derive the wrong KDF key and fail its MAC (fails closed), while an
/// *unknown* version is refused by `check_crypto` before any decrypt.
fn signing_digest(
    manifest_v: u8,
    created_at: u64,
    slug: &str,
    fingerprint: &str,
    manifest_sha256: &str,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(SIG_DOMAIN);
    h.update([manifest_v]);
    h.update(created_at.to_le_bytes());
    for field in [slug, fingerprint, manifest_sha256] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    h.finalize().into()
}

/// Build a manifest envelope: seal each of `plaintext_parts` under `browse_key`, hash the ordered
/// ciphertexts, and sign the canonical digest with `identity`. `plaintext_parts` are the caller's
/// `hb-net::split_listing` output for the canonical full listing JSON (index + content parts, or a
/// single element when it fits one event); this module treats them opaquely. `snapshot_fingerprint`
/// is the tree's already-computed fingerprint. Signing is deterministic (BIP-340 no-aux-rand), so an
/// envelope re-serializes to stable bytes. Errs on an empty `plaintext_parts` (nothing to seal).
pub fn build_manifest_envelope(
    identity: &Identity,
    slug: &str,
    browse_key: &BrowseKey,
    snapshot_fingerprint: &str,
    created_at: u64,
    plaintext_parts: &[String],
) -> Result<ManifestEnvelope, HbError> {
    if plaintext_parts.is_empty() {
        return Err(HbError::InvalidEncryptedMessage);
    }
    let ciphertexts = plaintext_parts
        .iter()
        .map(|part| encrypt_listing(browse_key, part))
        .collect::<Result<Vec<String>, HbError>>()?;
    let manifest_sha256 = sha256_parts(&ciphertexts);
    let digest = signing_digest(MANIFEST_V, created_at, slug, snapshot_fingerprint, &manifest_sha256);
    let msg = Message::from_digest(digest);
    // nostr's `SecretKey` derefs to `secp256k1::SecretKey`; sign on the same global context nostr
    // itself uses. No-aux-rand ⇒ deterministic signature ⇒ canonical, reproducible envelope bytes.
    let sk: &secp256k1::SecretKey = identity.keys().secret_key();
    let keypair = Keypair::from_secret_key(SECP256K1, sk);
    let sig = SECP256K1.sign_schnorr_no_aux_rand(&msg, &keypair);
    Ok(ManifestEnvelope {
        manifest_v: MANIFEST_V,
        slug: slug.to_string(),
        author_npub: identity.npub(),
        crypto_v: CRYPTO_V,
        snapshot_fingerprint: snapshot_fingerprint.to_string(),
        created_at,
        manifest_sha256,
        author_sig: hex::encode(sig.serialize()),
        ciphertexts,
    })
}

impl ManifestEnvelope {
    /// Self-consistency, no author/key needed: the versions are recognised, the body is non-empty, and
    /// the declared `manifest_sha256` matches the ordered ciphertexts (a tampered, reordered, dropped,
    /// or injected part flips the hash and is refused here, before any signature or decrypt).
    pub fn verify_integrity(&self) -> Result<(), HbError> {
        if self.manifest_v == 0 || self.manifest_v > MANIFEST_V {
            return Err(HbError::UnsupportedVersion(self.manifest_v));
        }
        check_crypto(self.crypto_v)?;
        if self.ciphertexts.is_empty() {
            return Err(HbError::InvalidEncryptedMessage);
        }
        if sha256_parts(&self.ciphertexts) != self.manifest_sha256 {
            return Err(HbError::InvalidEncryptedMessage);
        }
        Ok(())
    }

    /// Full author verification: integrity, then the declared author pinned to `expected_author`
    /// (the browsed peer), then the BIP-340 signature under that author's x-only key. Order matters
    /// — pin the author *before* trusting the signature, so an envelope for peer A cannot verify
    /// while browsing peer B (M16 headline failure mode #3).
    pub fn verify_author(&self, expected_author: &PublicKey) -> Result<(), HbError> {
        self.verify_integrity()?;
        let declared = parse_npub(&self.author_npub)?;
        if &declared != expected_author {
            return Err(HbError::WrongSigner);
        }
        let digest = signing_digest(
            self.manifest_v,
            self.created_at,
            &self.slug,
            &self.snapshot_fingerprint,
            &self.manifest_sha256,
        );
        let msg = Message::from_digest(digest);
        let sig_bytes = hex::decode(&self.author_sig)?;
        let sig = Signature::from_slice(&sig_bytes).map_err(|_| HbError::InvalidSignature)?;
        let xonly: XOnlyPublicKey =
            expected_author.xonly().map_err(|e| HbError::InvalidPublicKey(e.to_string()))?;
        SECP256K1.verify_schnorr(&sig, &msg, &xonly).map_err(|_| HbError::InvalidSignature)
    }

    /// Decrypt the manifest body into its listing **parts** (the `hb-net::split_listing` output:
    /// index + content parts, or a single element). The caller restitches them with
    /// `hb-net::render_listing`. Needs only the browse-key; callers must
    /// [`verify_author`](Self::verify_author) first — [`open`](Self::open) does both in the ratified
    /// order.
    pub fn decrypt(&self, browse_key: &BrowseKey) -> Result<Vec<String>, HbError> {
        self.ciphertexts
            .iter()
            .map(|ct| decrypt_listing(browse_key, self.crypto_v, ct))
            .collect()
    }

    /// Verify against the browsed author, then decrypt into listing parts — the one-call import path.
    pub fn open(
        &self,
        browse_key: &BrowseKey,
        expected_author: &PublicKey,
    ) -> Result<Vec<String>, HbError> {
        self.verify_author(expected_author)?;
        self.decrypt(browse_key)
    }

    /// True iff this manifest describes the same snapshot as `newest_fingerprint` (the newest
    /// teaser's). A mismatch ⇒ an older version of the tree; the caller surfaces "ask again" rather
    /// than serving stale data silently.
    pub fn matches_fingerprint(&self, newest_fingerprint: &str) -> bool {
        self.snapshot_fingerprint == newest_fingerprint
    }

    /// Canonical serialization — the exact `.hbmanifest` bytes. Deterministic (serde emits struct
    /// fields in declaration order), so identical input round-trips to identical bytes.
    pub fn to_json(&self) -> Result<String, HbError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parse an envelope from its canonical JSON. Verify it before trusting any field.
    pub fn from_json(s: &str) -> Result<Self, HbError> {
        Ok(serde_json::from_str(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A canonical full-listing plaintext (opaque to this module) and a realistic 64-hex fingerprint.
    const PLAINTEXT: &str =
        r#"{"slug":"criterion","content_types":["video"],"entries":[{"name":"Ran.mkv"}]}"#;
    const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn built() -> (Identity, BrowseKey, ManifestEnvelope) {
        let id = Identity::generate();
        let bk: BrowseKey = rand::random();
        // A listing that fits one event → a single-part manifest (the common small-collection case).
        let env = build_manifest_envelope(&id, "criterion", &bk, FP, 1_700_000_000, &[PLAINTEXT.into()])
            .unwrap();
        (id, bk, env)
    }

    #[test]
    fn build_then_open_roundtrips_to_the_parts() {
        let (id, bk, env) = built();
        assert_eq!(env.ciphertexts.len(), 1);
        assert_eq!(env.open(&bk, &id.public_key()).unwrap(), vec![PLAINTEXT.to_string()]);
    }

    #[test]
    fn build_then_open_roundtrips_a_multi_part_body() {
        // A chunked manifest: several split parts (an index + content parts, opaque here) round-trip in
        // order through build → open, and the sha binds the whole ordered set.
        let id = Identity::generate();
        let bk: BrowseKey = rand::random();
        let parts: Vec<String> = (0..4).map(|i| format!(r#"{{"part":{i},"entries":[]}}"#)).collect();
        let env = build_manifest_envelope(&id, "vault", &bk, FP, 1, &parts).unwrap();
        assert_eq!(env.ciphertexts.len(), 4);
        assert_eq!(env.open(&bk, &id.public_key()).unwrap(), parts);
        assert_eq!(env.manifest_sha256, sha256_parts(&env.ciphertexts));
    }

    #[test]
    fn reordering_parts_fails_integrity() {
        // The sha binds part ORDER, so a swapped pair is refused before decrypt (and so fails the sig).
        let id = Identity::generate();
        let bk: BrowseKey = rand::random();
        let parts: Vec<String> = (0..3).map(|i| format!(r#"{{"part":{i},"entries":[]}}"#)).collect();
        let mut env = build_manifest_envelope(&id, "vault", &bk, FP, 1, &parts).unwrap();
        env.ciphertexts.swap(0, 2);
        assert!(matches!(env.verify_integrity(), Err(HbError::InvalidEncryptedMessage)));
        assert!(env.verify_author(&id.public_key()).is_err());
    }

    #[test]
    fn empty_body_is_refused() {
        let id = Identity::generate();
        let bk: BrowseKey = rand::random();
        assert!(build_manifest_envelope(&id, "vault", &bk, FP, 1, &[]).is_err());
        let (_id, _bk, mut env) = built();
        env.ciphertexts.clear();
        env.manifest_sha256 = sha256_parts(&env.ciphertexts);
        assert!(matches!(env.verify_integrity(), Err(HbError::InvalidEncryptedMessage)));
    }

    #[test]
    fn fields_reflect_the_build_inputs() {
        let (id, _bk, env) = built();
        assert_eq!(env.manifest_v, MANIFEST_V);
        assert_eq!(env.crypto_v, CRYPTO_V);
        assert_eq!(env.slug, "criterion");
        assert_eq!(env.author_npub, id.npub());
        assert_eq!(env.snapshot_fingerprint, FP);
        assert_eq!(env.created_at, 1_700_000_000);
        // manifest_sha256 is the hash over the ordered ciphertexts, and author_sig is 64 bytes of hex.
        assert_eq!(env.manifest_sha256, sha256_parts(&env.ciphertexts));
        assert_eq!(env.author_sig.len(), 128);
        env.verify_author(&id.public_key()).unwrap();
    }

    #[test]
    fn tampered_ciphertext_flips_sha_and_is_refused() {
        // Mutate a ciphertext without updating the hash: caught by the integrity check.
        let (id, _bk, mut env) = built();
        env.ciphertexts[0].push_str("AA");
        assert!(matches!(env.verify_integrity(), Err(HbError::InvalidEncryptedMessage)));
        assert!(env.verify_author(&id.public_key()).is_err());
    }

    #[test]
    fn tampered_ciphertext_with_recomputed_sha_fails_the_signature() {
        // Swap in a *different valid* ciphertext AND recompute its hash so integrity passes — the
        // signature (which covers the old hash) must still reject it. Proves the sig binds the body.
        let (id, bk, mut env) = built();
        let other_ct = encrypt_listing(&bk, r#"{"slug":"criterion","entries":[]}"#).unwrap();
        env.ciphertexts = vec![other_ct];
        env.manifest_sha256 = sha256_parts(&env.ciphertexts);
        env.verify_integrity().unwrap(); // self-consistent now …
        assert!(matches!(env.verify_author(&id.public_key()), Err(HbError::InvalidSignature))); // … but unsigned
    }

    #[test]
    fn tampered_slug_flips_the_signature() {
        let (id, _bk, mut env) = built();
        env.slug = "criterion-leaked".into();
        assert!(matches!(env.verify_author(&id.public_key()), Err(HbError::InvalidSignature)));
    }

    #[test]
    fn tampered_fingerprint_flips_the_signature() {
        let (id, _bk, mut env) = built();
        env.snapshot_fingerprint = FP.replace('9', "8");
        assert!(matches!(env.verify_author(&id.public_key()), Err(HbError::InvalidSignature)));
    }

    #[test]
    fn tampered_created_at_flips_the_signature() {
        // created_at is inside the signed digest, so rewriting the declared build time no longer
        // verifies (Codex review, W1): a stale manifest cannot be re-stamped as fresh.
        let (id, _bk, mut env) = built();
        env.created_at += 1;
        assert!(matches!(env.verify_author(&id.public_key()), Err(HbError::InvalidSignature)));
    }

    #[test]
    fn unknown_manifest_version_is_refused_not_misparsed() {
        let (id, _bk, mut env) = built();
        env.manifest_v = MANIFEST_V + 1;
        assert!(matches!(env.verify_integrity(), Err(HbError::UnsupportedVersion(v)) if v == MANIFEST_V + 1));
        assert!(matches!(env.verify_author(&id.public_key()), Err(HbError::UnsupportedVersion(_))));
    }

    #[test]
    fn unknown_crypto_version_is_refused() {
        let (id, _bk, mut env) = built();
        env.crypto_v = CRYPTO_V + 1;
        assert!(matches!(env.verify_integrity(), Err(HbError::UnsupportedVersion(v)) if v == CRYPTO_V + 1));
        assert!(env.verify_author(&id.public_key()).is_err());
    }

    #[test]
    fn wrong_author_is_rejected_before_the_signature() {
        // A validly-built envelope authored by A, verified while browsing B: the author-pin rejects
        // it (WrongSigner) before the signature is even consulted.
        let (_id, _bk, env) = built();
        let other = Identity::generate();
        assert!(matches!(env.verify_author(&other.public_key()), Err(HbError::WrongSigner)));
    }

    #[test]
    fn relabeling_the_author_still_fails_the_signature() {
        // An attacker rewrites author_npub to B's and browses as B: the author-pin now passes, but
        // the signature was A's over A's digest → it will not verify under B's key.
        let (_id, _bk, mut env) = built();
        let other = Identity::generate();
        env.author_npub = other.npub();
        assert!(matches!(env.verify_author(&other.public_key()), Err(HbError::InvalidSignature)));
    }

    #[test]
    fn stale_fingerprint_is_detectable() {
        let (_id, _bk, env) = built();
        assert!(env.matches_fingerprint(FP));
        assert!(!env.matches_fingerprint("00ff00ff"));
    }

    #[test]
    fn wrong_browse_key_fails_to_decrypt() {
        // The signature verifies (author is right) but the wrong browse-key cannot open the body.
        let (id, _bk, env) = built();
        let wrong: BrowseKey = rand::random();
        assert!(matches!(env.decrypt(&wrong), Err(HbError::DecryptionFailed)));
        assert!(env.open(&wrong, &id.public_key()).is_err());
    }

    #[test]
    fn json_roundtrips_and_opens() {
        let (id, bk, env) = built();
        let json = env.to_json().unwrap();
        let back = ManifestEnvelope::from_json(&json).unwrap();
        assert_eq!(back, env);
        assert_eq!(back.open(&bk, &id.public_key()).unwrap(), vec![PLAINTEXT.to_string()]);
    }

    #[test]
    fn serialization_is_canonical_stable_bytes() {
        // The same envelope serializes to identical bytes every time (deterministic sig + struct
        // field order), and a serialize→parse→serialize round-trip is byte-stable.
        let (_id, _bk, env) = built();
        assert_eq!(env.to_json().unwrap(), env.to_json().unwrap());
        let once = env.to_json().unwrap();
        let twice = ManifestEnvelope::from_json(&once).unwrap().to_json().unwrap();
        assert_eq!(once, twice);
    }
}
