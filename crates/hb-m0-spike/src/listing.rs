//! Leg 3 — NIP-44 collection-listing encryption under a *symmetric* browse-key
//! (THE CRUX). Spec: listings are "NIP-44 ciphertext under your browse-key"
//! (§Core Concepts, §Data Model kinds table), and the browse-key is "a 32-byte
//! symmetric secret" shared with share-code holders — *not* an ECDH keypair.
//!
//! The unknown M0 had to settle: NIP-44's headline API derives its key via ECDH
//! between two parties, which doesn't fit a shared symmetric secret. Resolution:
//! `nostr 0.43`'s `nip44::v2::ConversationKey::new([u8; 32])` accepts a raw 32-byte
//! key directly (alongside the ECDH `derive`), so we feed it a key derived from the
//! browse-key. The derivation runs through a **versioned HKDF** so a future cipher/KDF
//! change is a clean negotiated upgrade, honouring the spec's "crypto/KDF version byte
//! in the browse-key derivation" requirement (§Schema & crypto versioning) — the same
//! discriminant idea as `hb-core/crypto.rs`'s `hoardbook-ecdh-v1` salt.

use anyhow::Result;
use hkdf::Hkdf;
use nostr::nips::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey};
use sha2::Sha256;

/// Version discriminant baked into the browse-key -> conversation-key derivation.
/// Bumping it is a deliberate flag-day: v1 ciphertext won't decrypt under v2.
pub const BROWSE_KDF_VERSION: u8 = 1;
const HKDF_SALT: &[u8] = b"hoardbook/browse-key";

/// Derive the NIP-44 conversation key for a given KDF version.
fn conversation_key_versioned(browse_key: &[u8; 32], version: u8) -> ConversationKey {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), browse_key);
    let mut ck = [0u8; 32];
    // The version byte is the HKDF `info`, so each version is a domain-separated key.
    hk.expand(&[version], &mut ck)
        .expect("32 is a valid HKDF-SHA256 output length");
    ConversationKey::new(ck)
}

fn conversation_key(browse_key: &[u8; 32]) -> ConversationKey {
    conversation_key_versioned(browse_key, BROWSE_KDF_VERSION)
}

/// Encrypt a collection listing under the browse-key. Returns raw NIP-44 v2 bytes
/// (`[version][nonce][ciphertext][mac]`); the M1 event will base64 these into content.
pub fn encrypt_listing(browse_key: &[u8; 32], listing_json: &str) -> Result<Vec<u8>> {
    Ok(encrypt_to_bytes(&conversation_key(browse_key), listing_json.as_bytes())?)
}

/// Decrypt a listing. Fails (MAC mismatch) for anyone without the exact browse-key.
pub fn decrypt_listing(browse_key: &[u8; 32], ciphertext: &[u8]) -> Result<String> {
    let bytes = decrypt_to_bytes(&conversation_key(browse_key), ciphertext)?;
    Ok(String::from_utf8(bytes)?)
}

/// Human-readable proof line for the binary's report.
pub fn demo() -> Result<String> {
    let browse_key: [u8; 32] = rand::random();
    let listing = sample_listing();
    let ct = encrypt_listing(&browse_key, &listing)?;
    let round = decrypt_listing(&browse_key, &ct)?;
    anyhow::ensure!(round == listing, "round-trip mismatch");
    // A holder of a *different* browse-key gets nothing.
    let wrong: [u8; 32] = rand::random();
    anyhow::ensure!(decrypt_listing(&wrong, &ct).is_err(), "wrong key must fail");
    Ok(format!(
        "{}-byte listing -> {}-byte ciphertext -> round-trip OK; wrong browse-key rejected",
        listing.len(),
        ct.len()
    ))
}

fn sample_listing() -> String {
    r#"{"slug":"p2p-it-films","content_types":["video"],"files":[{"name":"sample.mkv","bytes":734003200}]}"#.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_under_the_browse_key() {
        let browse_key: [u8; 32] = rand::random();
        let listing = sample_listing();
        let ct = encrypt_listing(&browse_key, &listing).unwrap();
        assert_ne!(ct, listing.as_bytes(), "must actually be encrypted");
        assert_eq!(decrypt_listing(&browse_key, &ct).unwrap(), listing);
    }

    #[test]
    fn wrong_browse_key_cannot_decrypt() {
        let key_a: [u8; 32] = rand::random();
        let key_b: [u8; 32] = rand::random();
        let ct = encrypt_listing(&key_a, &sample_listing()).unwrap();
        // NIP-44's HMAC tag fails for the wrong conversation key — the open web sees ciphertext only.
        assert!(decrypt_listing(&key_b, &ct).is_err());
    }

    #[test]
    fn same_key_two_encryptions_differ_but_both_decrypt() {
        // NIP-44 uses a random nonce, so ciphertexts differ even for identical plaintext.
        let key: [u8; 32] = rand::random();
        let listing = sample_listing();
        let a = encrypt_listing(&key, &listing).unwrap();
        let b = encrypt_listing(&key, &listing).unwrap();
        assert_ne!(a, b, "nonce reuse would be a red flag");
        assert_eq!(decrypt_listing(&key, &a).unwrap(), listing);
        assert_eq!(decrypt_listing(&key, &b).unwrap(), listing);
    }

    #[test]
    fn kdf_version_bump_is_a_clean_break() {
        // The forward-compat seam: ciphertext made under v1 must not decrypt under a
        // future v2 derivation of the *same* browse-key.
        let key: [u8; 32] = rand::random();
        let v1 = conversation_key_versioned(&key, 1);
        let v2 = conversation_key_versioned(&key, 2);
        let ct = encrypt_to_bytes(&v1, b"listing").unwrap();
        assert!(decrypt_to_bytes(&v2, &ct).is_err(), "a KDF version bump must domain-separate");
        assert_eq!(decrypt_to_bytes(&v1, &ct).unwrap(), b"listing");
    }
}
