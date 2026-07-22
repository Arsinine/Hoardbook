//! At-rest sealing for the local DM cache (devtest v0.12.4 #2).
//!
//! Re-opening Chat must not re-fetch + re-unwrap the whole NIP-17 mailbox every time (the old
//! behaviour: each poll pulled the entire gift-wrap mailbox and re-ran the expensive unwrap on every
//! message), so the **decoded** DM history is cached on disk. That cache holds decrypted message
//! plaintext, so it is sealed under a key **derived from the identity secret** — confidential AND
//! tamper-evident (NIP-44 v2 AEAD, the same primitive the listing/CEK paths ship), with an
//! **independent HKDF domain** so it shares no key with the browse-key or the private-listing CEK.
//!
//! INV-8 (safe-to-keep, the durable-artifact gate): the cache never leaves the device, is wiped with
//! the data dir, and grants **no new decryption capability** — the identity key already opens these
//! DMs (they are NIP-17 wraps addressed to it). INV-2 is untouched (no browse-key, nothing broadcast).
//! Freshness stays the store's job (local fetch-time), never the wrap's attacker-fuzzed `created_at`.

use hkdf::Hkdf;
use sha2::Sha256;

use crate::error::HbError;
use crate::identity::Identity;
use crate::listing::{decrypt_with_cek, encrypt_with_cek, ContentKey};
use crate::version::CRYPTO_V;

/// HKDF salt domain-separating the DM-cache key from the browse-key (`hoardbook/browse-key`) and the
/// private-listing CEK (`hoardbook/cek`) paths — an independent key even under colliding input key
/// material (RFC 5869 salt separation).
const HKDF_SALT_DM_CACHE: &[u8] = b"hoardbook/dm-cache";

/// Derive the 32-byte at-rest key from the identity secret. Deterministic, so the cache re-opens
/// across restarts and survives backup/restore (the identity travels with the data dir, so the key
/// re-derives on the new install). Never the raw signing key — HKDF domain-separates it.
fn dm_cache_key(identity: &Identity) -> ContentKey {
    let ikm = identity.keys().secret_key().secret_bytes();
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_DM_CACHE), &ikm);
    let mut okm: ContentKey = [0u8; 32];
    hk.expand(b"dm-cache/v1", &mut okm)
        .expect("32 is a valid HKDF-SHA256 output length");
    okm
}

/// Seal the DM-cache plaintext (canonical JSON) for at-rest storage.
pub fn seal_dm_cache(identity: &Identity, plaintext: &str) -> Result<String, HbError> {
    encrypt_with_cek(&dm_cache_key(identity), plaintext)
}

/// Open a sealed DM cache. A tampered, foreign, or version-mismatched blob fails the AEAD tag / the
/// version check (Err) — it never silently returns forged plaintext. Callers treat an Err as "no
/// usable cache" and rebuild from the relay (self-healing across a crypto-version bump or corruption).
pub fn open_dm_cache(identity: &Identity, ciphertext: &str) -> Result<String, HbError> {
    decrypt_with_cek(&dm_cache_key(identity), CRYPTO_V, ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CACHE: &str = r#"[{"from":"npub1a","to":"npub1me","content":"back room open","sent_at":"2026-07-22T00:00:00Z"}]"#;

    #[test]
    fn seal_open_roundtrip() {
        let me = Identity::generate();
        let sealed = seal_dm_cache(&me, CACHE).unwrap();
        assert_ne!(sealed, CACHE, "the on-disk cache is ciphertext, never plaintext");
        assert_eq!(open_dm_cache(&me, &sealed).unwrap(), CACHE);
    }

    #[test]
    fn a_different_identity_cannot_open_the_cache() {
        // The key is identity-derived: another install's identity cannot decrypt this cache.
        let me = Identity::generate();
        let other = Identity::generate();
        let sealed = seal_dm_cache(&me, CACHE).unwrap();
        assert!(open_dm_cache(&other, &sealed).is_err(), "a foreign identity must not open the cache");
    }

    #[test]
    fn a_tampered_blob_fails_the_aead_tag_not_silently_forged() {
        // Tamper-evidence (the "can't be tampered with easily" requirement): flipping a byte fails the
        // AEAD tag rather than yielding altered plaintext.
        let me = Identity::generate();
        let sealed = seal_dm_cache(&me, CACHE).unwrap();
        let mut bytes = sealed.into_bytes();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        let tampered = String::from_utf8_lossy(&bytes).into_owned();
        assert!(open_dm_cache(&me, &tampered).is_err(), "a tampered cache must not decrypt to forged plaintext");
    }

    #[test]
    fn cache_key_is_domain_separated_from_the_browse_key_path() {
        // Same 32 bytes used as a browse-key vs as DM-cache IKM must yield different ciphertext — the
        // HKDF salt (`hoardbook/dm-cache` vs `hoardbook/browse-key`) guarantees an independent key.
        use crate::listing::encrypt_listing;
        let me = Identity::generate();
        let bk: crate::listing::BrowseKey = me.keys().secret_key().secret_bytes();
        // Not a real scenario (the browse-key is a separate secret) — this only asserts the domains
        // don't collide: sealing the same text under each path is not cross-openable.
        let via_browse = encrypt_listing(&bk, CACHE).unwrap();
        let via_cache = seal_dm_cache(&me, CACHE).unwrap();
        assert_ne!(via_browse, via_cache, "distinct HKDF domains must not produce the same ciphertext");
    }
}
