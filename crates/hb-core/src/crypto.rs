//! Keypair generation, Hoardbook ID encoding, and Ed25519 signing.
//!
//! # Hoardbook ID format
//! `hb1_` prefix + base58(pubkey_bytes[32] || sha256d(pubkey_bytes)[..4])
//! ≈ 53 characters total. The 4-byte checksum (first 4 bytes of double-SHA256)
//! allows typo detection before any network connection is attempted.

use base64::Engine as _;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256, Sha512};

use crate::error::HbError;
use crate::jcs;

// ---------------------------------------------------------------------------
// ID encoding / decoding
// ---------------------------------------------------------------------------

/// Encodes a 32-byte Ed25519 public key as a Hoardbook ID string.
pub fn hb_id_encode(pubkey: &[u8; 32]) -> String {
    let mut payload = Vec::with_capacity(36);
    payload.extend_from_slice(pubkey);
    payload.extend_from_slice(&checksum(pubkey));
    format!("hb1_{}", bs58::encode(&payload).into_string())
}

/// Decodes a Hoardbook ID string back to 32 raw public-key bytes.
/// Returns `HbError::InvalidId` if the prefix or base58 is wrong,
/// and `HbError::InvalidChecksum` if the checksum doesn't match.
pub fn hb_id_decode(id: &str) -> Result<[u8; 32], HbError> {
    let encoded = id
        .strip_prefix("hb1_")
        .ok_or_else(|| HbError::InvalidId("missing hb1_ prefix".into()))?;

    let bytes = bs58::decode(encoded)
        .into_vec()
        .map_err(|_| HbError::InvalidId("invalid base58 encoding".into()))?;

    if bytes.len() != 36 {
        return Err(HbError::InvalidId(format!(
            "expected 36 decoded bytes, got {}",
            bytes.len()
        )));
    }

    let (key_bytes, got_checksum) = bytes.split_at(32);
    let expected = checksum(key_bytes);
    if got_checksum != expected {
        return Err(HbError::InvalidChecksum);
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);
    Ok(key)
}

fn checksum(data: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    [second[0], second[1], second[2], second[3]]
}

// ---------------------------------------------------------------------------
// HbId newtype
// ---------------------------------------------------------------------------

/// A validated Hoardbook ID string (`hb1_…`).
/// Guaranteed to decode successfully — checksum is verified at construction.
/// Implements `Deserialize` so Tauri IPC validation happens at the boundary.
#[derive(Debug, Clone)]
pub struct HbId(String);

impl HbId {
    /// Return the 32 raw public-key bytes (infallible — invariant is checked at construction).
    pub fn pubkey(&self) -> [u8; 32] {
        hb_id_decode(&self.0).expect("HbId invariant: string is always valid")
    }
}

impl std::ops::Deref for HbId {
    type Target = str;
    fn deref(&self) -> &str { &self.0 }
}

impl std::fmt::Display for HbId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<HbId> for String {
    fn from(id: HbId) -> String { id.0 }
}

impl serde::Serialize for HbId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for HbId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        hb_id_decode(&s).map_err(serde::de::Error::custom)?;
        Ok(HbId(s))
    }
}

// ---------------------------------------------------------------------------
// Keypair
// ---------------------------------------------------------------------------

/// A Hoardbook keypair wrapping an Ed25519 signing key.
pub struct HoardbookKeypair {
    signing_key: SigningKey,
}

impl HoardbookKeypair {
    /// Generate a fresh keypair using the OS CSPRNG.
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    /// Reconstruct from 32 raw private-key bytes (e.g. loaded from disk).
    pub fn from_bytes(private_key_bytes: &[u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(private_key_bytes),
        }
    }

    /// The 32-byte private key. Handle with care — never log or transmit.
    pub fn private_key_bytes(&self) -> &[u8; 32] {
        self.signing_key.as_bytes()
    }

    /// The 32-byte public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// The Hoardbook ID string (`hb1_...`) for this keypair.
    pub fn hb_id(&self) -> String {
        hb_id_encode(&self.public_key_bytes())
    }

    /// Sign `payload` (a `serde_json::Value`) using JCS canonicalization.
    /// Returns a hex-encoded 64-byte Ed25519 signature.
    pub fn sign(&self, payload: &serde_json::Value) -> String {
        let canonical = jcs::canonicalize(payload);
        let signature: Signature = self.signing_key.sign(&canonical);
        hex::encode(signature.to_bytes())
    }

    // -------------------------------------------------------------------------
    // End-to-end message encryption
    //
    // The Ed25519 keypair is also used for Diffie-Hellman by converting both
    // keys to their Curve25519 Montgomery-form equivalents. The shared secret
    // is then run through HKDF-SHA256 to derive a 256-bit ChaCha20-Poly1305
    // key. The 24-byte nonce is randomly generated per message.
    //
    // Wire format of the `content` field when encrypted:
    //   base64( nonce[24] || xchacha20poly1305_ciphertext )
    // -------------------------------------------------------------------------

    /// Encrypt `plaintext` for `recipient_pubkey` (raw Ed25519 bytes from hb_id_decode).
    /// Returns `base64(nonce[24] || ciphertext)`.
    pub fn encrypt_for(
        &self,
        recipient_pubkey: &[u8; 32],
        plaintext: &str,
    ) -> Result<String, crate::error::HbError> {
        let recipient_x25519 = ed25519_pubkey_to_x25519(recipient_pubkey)?;
        let my_x25519 = self.to_x25519_secret();
        let shared = my_x25519.diffie_hellman(&recipient_x25519);
        let key = derive_key(shared.as_bytes());

        let mut nonce_bytes = [0u8; 24];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key));
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| crate::error::HbError::EncryptionFailed)?;

        let mut packed = Vec::with_capacity(24 + ciphertext.len());
        packed.extend_from_slice(&nonce_bytes);
        packed.extend_from_slice(&ciphertext);

        Ok(base64::engine::general_purpose::STANDARD.encode(&packed))
    }

    /// Decrypt a message received from `sender_pubkey` (raw Ed25519 bytes from hb_id_decode).
    /// `encrypted` must be the `base64(nonce[24] || ciphertext)` produced by `encrypt_for`.
    pub fn decrypt_from(
        &self,
        sender_pubkey: &[u8; 32],
        encrypted: &str,
    ) -> Result<String, crate::error::HbError> {
        let sender_x25519 = ed25519_pubkey_to_x25519(sender_pubkey)?;
        let my_x25519 = self.to_x25519_secret();
        let shared = my_x25519.diffie_hellman(&sender_x25519);
        let key = derive_key(shared.as_bytes());

        let packed = base64::engine::general_purpose::STANDARD
            .decode(encrypted)
            .map_err(|_| crate::error::HbError::InvalidEncryptedMessage)?;

        // Minimum: 24-byte nonce + 16-byte Poly1305 tag (empty plaintext)
        if packed.len() < 40 {
            return Err(crate::error::HbError::InvalidEncryptedMessage);
        }

        let (nonce_bytes, ciphertext) = packed.split_at(24);
        let nonce = XNonce::from_slice(nonce_bytes);

        let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key));
        let plaintext_bytes = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| crate::error::HbError::DecryptionFailed)?;

        String::from_utf8(plaintext_bytes).map_err(|_| crate::error::HbError::DecryptionFailed)
    }

    /// Derive the X25519 static secret from the Ed25519 signing key.
    /// Uses SHA-512(seed)[0..32] with X25519 clamping — the same scalar
    /// Ed25519 uses internally, just re-interpreted for Montgomery-form DH.
    fn to_x25519_secret(&self) -> x25519_dalek::StaticSecret {
        let seed = self.signing_key.to_bytes();
        let hash = Sha512::digest(seed);
        let mut scalar_bytes = [0u8; 32];
        scalar_bytes.copy_from_slice(&hash[..32]);
        scalar_bytes[0] &= 248;  // clear cofactor bits
        scalar_bytes[31] &= 127; // clear high bit
        scalar_bytes[31] |= 64;  // set second-highest bit
        x25519_dalek::StaticSecret::from(scalar_bytes)
    }
}

// ---------------------------------------------------------------------------
// Private helpers shared by encrypt_for / decrypt_from
// ---------------------------------------------------------------------------

/// Convert a raw Ed25519 public-key bytes to an X25519 public key by mapping
/// the Edwards point to its Montgomery (Curve25519) form.
fn ed25519_pubkey_to_x25519(
    pubkey_bytes: &[u8; 32],
) -> Result<x25519_dalek::PublicKey, crate::error::HbError> {
    let vk = VerifyingKey::from_bytes(pubkey_bytes)
        .map_err(|e| crate::error::HbError::InvalidPublicKey(e.to_string()))?;
    Ok(x25519_dalek::PublicKey::from(vk.to_montgomery().to_bytes()))
}

/// Derive a 256-bit symmetric key from an X25519 shared secret using HKDF-SHA256.
fn derive_key(shared_secret: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, shared_secret);
    let mut key = [0u8; 32];
    hk.expand(b"hoardbook-chat-v1", &mut key)
        .expect("32 bytes is always a valid HKDF output length");
    key
}

// ---------------------------------------------------------------------------
// Verification (stateless — needs only the public key)
// ---------------------------------------------------------------------------

/// Verify an Ed25519 signature over a JSON payload using JCS canonicalization.
///
/// `public_key_bytes` — raw 32-byte public key (from `hb_id_decode`)
/// `payload`          — the `serde_json::Value` that was signed
/// `signature_hex`    — hex-encoded 64-byte signature from the envelope
pub fn verify(
    public_key_bytes: &[u8; 32],
    payload: &serde_json::Value,
    signature_hex: &str,
) -> Result<(), HbError> {
    let verifying_key = VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|e| HbError::InvalidPublicKey(e.to_string()))?;

    let sig_bytes_vec = hex::decode(signature_hex)?;
    let sig_bytes: [u8; 64] = sig_bytes_vec
        .try_into()
        .map_err(|_| HbError::InvalidSignature)?;
    let signature = Signature::from_bytes(&sig_bytes);

    let canonical = jcs::canonicalize(payload);
    verifying_key
        .verify_strict(&canonical, &signature)
        .map_err(|_| HbError::InvalidSignature)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_decode_roundtrip() {
        let kp = HoardbookKeypair::generate();
        let id = kp.hb_id();
        assert!(id.starts_with("hb1_"));
        let decoded = hb_id_decode(&id).expect("decode failed");
        assert_eq!(decoded, kp.public_key_bytes());
    }

    #[test]
    fn id_checksum_rejects_tampering() {
        let kp = HoardbookKeypair::generate();
        let mut id = kp.hb_id();
        // Flip the last character
        let last = id.pop().unwrap();
        id.push(if last == 'A' { 'B' } else { 'A' });
        let result = hb_id_decode(&id);
        assert!(
            matches!(result, Err(HbError::InvalidChecksum) | Err(HbError::InvalidId(_))),
            "expected checksum or id error"
        );
    }

    #[test]
    fn sign_and_verify() {
        let kp = HoardbookKeypair::generate();
        let payload = json!({ "hello": "world", "num": 42 });
        let sig = kp.sign(&payload);
        let pubkey = kp.public_key_bytes();
        verify(&pubkey, &payload, &sig).expect("valid sig should verify");
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let kp = HoardbookKeypair::generate();
        let payload = json!({ "hello": "world" });
        let sig = kp.sign(&payload);
        let pubkey = kp.public_key_bytes();
        let tampered = json!({ "hello": "TAMPERED" });
        let result = verify(&pubkey, &tampered, &sig);
        assert!(matches!(result, Err(HbError::InvalidSignature)));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let kp1 = HoardbookKeypair::generate();
        let kp2 = HoardbookKeypair::generate();
        let payload = json!({ "data": 1 });
        let sig = kp1.sign(&payload);
        let result = verify(&kp2.public_key_bytes(), &payload, &sig);
        assert!(matches!(result, Err(HbError::InvalidSignature)));
    }

    // ---------- hb_id_decode rejection cases ----------

    #[test]
    fn hb_id_decode_rejects_wrong_prefix() {
        let kp = HoardbookKeypair::generate();
        let valid = kp.hb_id();
        // Strip the "hb1_" prefix
        let stripped = &valid[4..];
        assert!(matches!(hb_id_decode(stripped), Err(HbError::InvalidId(_))));
        assert!(matches!(hb_id_decode(""), Err(HbError::InvalidId(_))));
        assert!(matches!(hb_id_decode("HB1_something"), Err(HbError::InvalidId(_))));
    }

    #[test]
    fn hb_id_decode_rejects_invalid_base58() {
        // '0', 'O', 'I', 'l' are not valid base58 characters
        assert!(matches!(hb_id_decode("hb1_0OIl"), Err(HbError::InvalidId(_))));
    }

    #[test]
    fn hb_id_decode_rejects_wrong_length() {
        // Too short: valid prefix but only a few bytes of base58
        assert!(matches!(hb_id_decode("hb1_abc"), Err(HbError::InvalidId(_))));
        // Too long: extra bytes appended
        let kp = HoardbookKeypair::generate();
        let long = format!("{}AAAA", kp.hb_id());
        assert!(matches!(
            hb_id_decode(&long),
            Err(HbError::InvalidId(_)) | Err(HbError::InvalidChecksum)
        ));
    }

    #[test]
    fn hb_id_decode_catches_checksum_failure_in_valid_base58() {
        // Construct a 36-byte payload with a correct-looking structure but wrong checksum.
        // This specifically exercises the checksum path rather than the base58-decode path.
        let kp = HoardbookKeypair::generate();
        let pubkey = kp.public_key_bytes();

        // Build a payload with an intentionally wrong 4-byte checksum.
        let mut payload = Vec::with_capacity(36);
        payload.extend_from_slice(&pubkey);
        payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // wrong checksum
        let crafted = format!("hb1_{}", bs58::encode(&payload).into_string());

        assert!(
            matches!(hb_id_decode(&crafted), Err(HbError::InvalidChecksum)),
            "wrong checksum in valid base58 must produce InvalidChecksum, not a parse error"
        );
    }

    // ---------- nonce uniqueness ----------

    #[test]
    fn each_encryption_produces_unique_ciphertext() {
        // XChaCha20-Poly1305 is only secure if nonces are never reused.
        // A zero nonce or fixed nonce would make all ciphertexts identical for identical inputs.
        let alice = HoardbookKeypair::generate();
        let bob = HoardbookKeypair::generate();
        let ct1 = alice.encrypt_for(&bob.public_key_bytes(), "same message").unwrap();
        let ct2 = alice.encrypt_for(&bob.public_key_bytes(), "same message").unwrap();
        assert_ne!(ct1, ct2, "nonce must be random — reuse would break stream-cipher secrecy");
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let alice = HoardbookKeypair::generate();
        let bob = HoardbookKeypair::generate();

        let plaintext = "Hello Bob, this is a secret!";

        // Alice encrypts for Bob
        let ciphertext = alice
            .encrypt_for(&bob.public_key_bytes(), plaintext)
            .expect("encryption should succeed");

        // Bob decrypts from Alice
        let recovered = bob
            .decrypt_from(&alice.public_key_bytes(), &ciphertext)
            .expect("decryption should succeed");

        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let alice = HoardbookKeypair::generate();
        let bob = HoardbookKeypair::generate();
        let eve = HoardbookKeypair::generate();

        let ciphertext = alice
            .encrypt_for(&bob.public_key_bytes(), "secret")
            .unwrap();

        // Eve tries to decrypt using Alice's public key — wrong shared secret
        let result = eve.decrypt_from(&alice.public_key_bytes(), &ciphertext);
        assert!(result.is_err(), "decryption with wrong key must fail");
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext() {
        let alice = HoardbookKeypair::generate();
        let bob = HoardbookKeypair::generate();

        let ciphertext = alice
            .encrypt_for(&bob.public_key_bytes(), "secret message")
            .unwrap();

        // Flip a byte in the base64 to corrupt the ciphertext
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&ciphertext)
            .unwrap();
        bytes[30] ^= 0xFF;
        let tampered = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let result = bob.decrypt_from(&alice.public_key_bytes(), &tampered);
        assert!(result.is_err(), "tampered ciphertext must not decrypt");
    }

    #[test]
    fn encryption_is_directional() {
        // A message encrypted for Bob cannot be decrypted by Bob using Bob's own key as sender —
        // the shared secret is derived from (sender_priv × recipient_pub), so the roles matter.
        let alice = HoardbookKeypair::generate();
        let bob = HoardbookKeypair::generate();
        let ct = alice.encrypt_for(&bob.public_key_bytes(), "secret").unwrap();
        // Bob tries to decrypt, incorrectly treating himself as the sender
        let result = bob.decrypt_from(&bob.public_key_bytes(), &ct);
        assert!(result.is_err(), "ECDH shared secret must depend on both parties' distinct keys");
    }
}
