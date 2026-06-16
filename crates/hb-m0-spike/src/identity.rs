//! Leg 1 — the `nostr` crate: secp256k1 identity, npub (NIP-19), NIP-01 sign/verify.
//!
//! Proves: we can mint a Nostr-native identity, encode it as a bech32 `npub` that
//! round-trips (NIP-19), and produce a NIP-01 event whose Schnorr signature verifies.
//! This is the foundation M1's `hb-core` identity layer rests on.

use anyhow::Result;
use nostr_sdk::prelude::*;

/// Mint a fresh secp256k1 identity; return the keys and its bech32 `npub`.
pub fn generate_identity() -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let npub = keys.public_key().to_bech32()?;
    Ok((keys, npub))
}

/// Build and sign a minimal NIP-01 event (offline — no relay, no signer service).
pub fn signed_note(keys: &Keys, content: &str) -> Result<Event> {
    Ok(EventBuilder::new(Kind::TextNote, content).sign_with_keys(keys)?)
}

/// Human-readable proof line for the binary's report.
pub fn demo() -> Result<String> {
    let (keys, npub) = generate_identity()?;
    let event = signed_note(&keys, "hoardbook m0 · leg 1")?;
    event.verify()?;
    Ok(format!("{npub}  (NIP-01 event {} verified)", event.id.to_bech32()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npub_is_bech32_and_round_trips() {
        let (keys, npub) = generate_identity().unwrap();
        assert!(npub.starts_with("npub1"), "expected bech32 npub, got {npub}");
        // NIP-19 decode must recover the exact public key (bech32 checksum rejects typos).
        let recovered = PublicKey::from_bech32(&npub).unwrap();
        assert_eq!(recovered, keys.public_key());
    }

    #[test]
    fn nip01_event_signature_verifies() {
        let (keys, _) = generate_identity().unwrap();
        let event = signed_note(&keys, "the payload schema-version lives in signed content").unwrap();
        // verify() checks both the event id (canonical NIP-01 serialization) and the Schnorr sig.
        assert!(event.verify().is_ok());
        assert_eq!(event.pubkey, keys.public_key());
    }

    #[test]
    fn tampering_breaks_verification() {
        let (keys, _) = generate_identity().unwrap();
        let mut event = signed_note(&keys, "original").unwrap();
        // Mutate the signed content; the id no longer matches and the sig is over the old id.
        event.content = "tampered".to_string();
        assert!(event.verify().is_err(), "a tampered event must fail verification");
    }

    #[test]
    fn distinct_identities_are_distinct() {
        let (a, na) = generate_identity().unwrap();
        let (b, nb) = generate_identity().unwrap();
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(na, nb);
    }
}
