//! The in-memory session identity — the three keys of the v0.9 Nostr model.
//!
//! 1. the secp256k1 `Identity` (the irreplaceable `npub`; signs every event + DM),
//! 2. the account **browse-key** (the "club pass" carried in the `hbk` share code; the default
//!    collection key),
//! 3. the regenerable 32-byte **transport secret** — the manifest plane's node key (M18 W2).
//!
//! The third key was removed in v0.9.6 when file transfer moved to Mascara, and returns under
//! **INV-4′**: Hoardbook moves no *collection files*, but it does carry manifests, and a transport
//! plane needs a stable node identity. W2 restores the key material only — deriving a node key
//! from it belongs to W1, with the plane.
//!
//! Persisted as [`StoredIdentity`] (DPAPI-encrypted on Windows, 0600 file elsewhere).

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use hb_core::{Identity, ShareCode};
use nostr::prelude::ToBech32;
use nostr::PublicKey;
use tokio::sync::RwLock;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::store::StoredIdentity;

/// Schema version of the on-disk identity record.
pub const IDENTITY_VERSION: u8 = 1;

/// The in-memory session copy of the account browse-key (AR6 completion). Wraps the raw
/// `[u8; 32]` in a `ZeroizeOnDrop` newtype so every in-memory copy — the session-held one, and
/// each clone taken at a call site — is wiped on drop, mirroring `StoredIdentity`'s at-rest
/// zeroizing (audit I-11). Deliberately **not** `Copy`: a `Copy` key can't be zeroized on the
/// copy's own drop (there's no drop to hook), so call sites must `.clone()` explicitly — each
/// clone then zeroizes independently.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionBrowseKey([u8; 32]);

impl SessionBrowseKey {
    pub fn new(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The in-memory session copy of the transport secret (M18 W2) — the manifest plane's node key.
/// Same `ZeroizeOnDrop`, deliberately-not-`Copy` newtype as [`SessionBrowseKey`], for the same
/// reason (audit I-11): a `Copy` secret has no drop to hook, so call sites must `.clone()` and
/// each clone then wipes itself.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionTransportKey([u8; 32]);

impl SessionTransportKey {
    pub fn new(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The loaded session identity (all three keys live in memory for the session).
pub struct AppIdentity {
    /// secp256k1 / `npub` — signs events + DMs.
    pub identity: Identity,
    /// Account browse-key (the "club pass").
    pub browse_key: SessionBrowseKey,
    /// Regenerable 32-byte transport secret — the manifest plane's node key (M18 W2).
    pub transport_key: SessionTransportKey,
}

impl AppIdentity {
    /// Mint a fresh identity: a new npub + a fresh account browse-key + a fresh transport secret.
    pub fn generate() -> Self {
        Self {
            identity: Identity::generate(),
            browse_key: SessionBrowseKey::new(rand::random()),
            transport_key: SessionTransportKey::new(rand::random()),
        }
    }

    /// Import an existing Nostr secret key (`nsec` or hex): the pasted key becomes the `npub`, and
    /// a **fresh** account browse-key + transport secret are minted (both are regenerable and need
    /// not — must not — be carried in from elsewhere). Distinct from the whole-directory restore
    /// path. A malformed key is a reasoned `Err`, never a panic.
    pub fn from_nsec(nsec: &str) -> Result<Self> {
        let identity = Identity::from_secret(nsec)
            .map_err(|e| anyhow!(e.to_string()))
            .context("parsing the imported Nostr secret key")?;
        Ok(Self {
            identity,
            browse_key: SessionBrowseKey::new(rand::random()),
            transport_key: SessionTransportKey::new(rand::random()),
        })
    }

    /// Reconstruct from the on-disk record.
    ///
    /// A record with no transport secret is a pre-M18 (2-key) identity: mint one rather than
    /// failing, so an existing user is never dead-ended by a key they never had a chance to store.
    /// `DataStore::load_identity` normally fills it in and persists it first, so this branch is
    /// the fallback for records that did not come through that path (a restored backup body).
    pub fn from_stored(s: &StoredIdentity) -> Result<Self> {
        let identity = Identity::from_secret(&s.nsec)
            .map_err(|e| anyhow!(e.to_string()))
            .context("parsing stored nsec")?;
        let browse_key: [u8; 32] = hex::decode(&s.browse_key_hex)
            .context("decoding browse key")?
            .try_into()
            .map_err(|_| anyhow!("browse key must be exactly 32 bytes"))?;
        let transport_key: [u8; 32] = if s.transport_secret_hex.is_empty() {
            rand::random()
        } else {
            hex::decode(&s.transport_secret_hex)
                .context("decoding transport secret")?
                .try_into()
                .map_err(|_| anyhow!("transport secret must be exactly 32 bytes"))?
        };
        Ok(Self {
            identity,
            browse_key: SessionBrowseKey::new(browse_key),
            transport_key: SessionTransportKey::new(transport_key),
        })
    }

    /// Serialize to the on-disk record.
    pub fn to_stored(&self) -> Result<StoredIdentity> {
        let nsec = self
            .identity
            .keys()
            .secret_key()
            .to_bech32()
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(StoredIdentity {
            version: IDENTITY_VERSION,
            nsec,
            browse_key_hex: hex::encode(self.browse_key.bytes()),
            transport_secret_hex: hex::encode(self.transport_key.bytes()),
        })
    }

    /// The bech32 `npub` — the identity everywhere.
    pub fn npub(&self) -> String {
        self.identity.npub()
    }

    /// The raw secp256k1 public key.
    pub fn public_key(&self) -> PublicKey {
        self.identity.public_key()
    }

    /// The full `hbk…` share code (npub + account browse-key) — the "club pass".
    pub fn share_code(&self) -> Result<String> {
        ShareCode::Full { pubkey: self.identity.public_key(), browse_key: *self.browse_key.bytes() }
            .encode()
            .map_err(|e| anyhow!(e.to_string()))
    }
}

/// Managed state: the loaded identity, or `None` before generate/import.
pub type SharedIdentity = Arc<RwLock<Option<AppIdentity>>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_roundtrip_through_stored() {
        let id = AppIdentity::generate();
        let npub = id.npub();
        let browse = id.browse_key.clone();
        let transport = id.transport_key.clone();

        let stored = id.to_stored().unwrap();
        let back = AppIdentity::from_stored(&stored).unwrap();

        assert_eq!(back.npub(), npub, "npub survives the storage roundtrip");
        assert_eq!(back.browse_key.bytes(), browse.bytes(), "account browse-key survives");
        assert_eq!(back.transport_key.bytes(), transport.bytes(), "transport secret survives");
    }

    #[test]
    fn share_code_is_full_hbk_carrying_browse_key() {
        let id = AppIdentity::generate();
        let code = id.share_code().unwrap();
        assert!(code.starts_with("hbk1"), "share code must be a full hbk code, got {code}");
        let parsed = ShareCode::parse(&code).unwrap();
        assert_eq!(parsed.pubkey(), id.public_key());
        assert_eq!(parsed.browse_key(), Some(*id.browse_key.bytes()));
    }

    #[test]
    fn distinct_identities_have_distinct_keys() {
        let a = AppIdentity::generate();
        let b = AppIdentity::generate();
        assert_ne!(a.npub(), b.npub());
        assert_ne!(a.browse_key.bytes(), b.browse_key.bytes());
        assert_ne!(a.transport_key.bytes(), b.transport_key.bytes());
    }

    fn _assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}

    /// Type-level (mirrors the hb-core `DerivedKey` pattern / `StoredIdentity::stored_identity_
    /// zeroizes_secrets_on_drop`): assert the compile-time bound rather than inspecting freed
    /// memory. Every in-memory copy of the session browse-key — the session-held one and each
    /// clone taken at a call site — is wiped on drop.
    #[test]
    fn session_browse_key_zeroizes_on_drop() {
        _assert_zeroize_on_drop::<SessionBrowseKey>();
    }

    /// The transport secret is a secret: same compile-time bound as the browse-key (M18 W2).
    #[test]
    fn session_transport_key_zeroizes_on_drop() {
        _assert_zeroize_on_drop::<SessionTransportKey>();
    }

    /// **The 2-key → 3-key migration (M18 W2), in the direction that now matters.**
    ///
    /// Every shipped identity from v0.9.6 through v0.12.x is a 2-key record with no transport
    /// secret at all. It must load and gain one without user action — `transport_secret_hex` is
    /// `serde(default)`, and an empty value mints rather than erroring. `DataStore::load_identity`
    /// is where the minted key gets PERSISTED (see `store.rs`'s migration test); this pins the
    /// pure decode half, which is also the path a restored backup body takes.
    #[test]
    fn two_key_identity_loads_and_gains_a_transport_key() {
        let id = AppIdentity::generate();
        let s = id.to_stored().unwrap();
        // A literal v0.12.x keys.json — the exact shipped 2-key shape. A literal fixture (rather
        // than `to_value` + remove) keeps this faithful to a real on-disk file and makes it fail
        // if a future serde-rename of `nsec`/`browse_key_hex` breaks reads.
        let two_key_json = format!(
            r#"{{"version":{},"nsec":"{}","browse_key_hex":"{}"}}"#,
            s.version, s.nsec, s.browse_key_hex,
        );

        let parsed: StoredIdentity = serde_json::from_str(&two_key_json).unwrap();
        assert!(parsed.transport_secret_hex.is_empty(), "a 2-key record has no transport secret");

        let back = AppIdentity::from_stored(&parsed).unwrap();
        assert_eq!(back.npub(), id.npub(), "npub survives the 2-key→3-key migration");
        assert_eq!(back.browse_key.bytes(), id.browse_key.bytes(), "browse-key survives");
        assert_ne!(
            back.transport_key.bytes(),
            &[0u8; 32],
            "the migration mints a real transport key, not a zero placeholder"
        );
        // And it is now durable: re-saving emits it, so the next load reads it back rather than
        // minting a second one.
        let resaved = back.to_stored().unwrap();
        assert_eq!(
            resaved.transport_secret_hex.len(),
            64,
            "the re-saved record carries a 32-byte hex transport secret"
        );
    }

    /// M7 / v0.9.6 (**rewritten for M18 W2, not deleted — it documents a real historical shape**).
    ///
    /// The pre-v0.9.6 record carried `iroh_secret_hex`. M18 restores a transport key but under an
    /// impl-neutral name, so that field stays unknown and is still dropped: such a record loads,
    /// keeps its two irreplaceable-or-derived secrets, mints a **fresh** transport key rather than
    /// resurrecting the retired one, and must not re-emit the dead field on write.
    #[test]
    fn legacy_iroh_secret_field_is_still_dropped_and_never_re_emitted() {
        let id = AppIdentity::generate();
        let s = id.to_stored().unwrap();
        let retired_secret = "ab".repeat(32); // the historical 32-byte iroh secret, hex
        let legacy_json = format!(
            r#"{{"version":{},"nsec":"{}","browse_key_hex":"{}","iroh_secret_hex":"{}"}}"#,
            s.version, s.nsec, s.browse_key_hex, retired_secret,
        );

        let parsed: StoredIdentity = serde_json::from_str(&legacy_json).unwrap();
        let back = AppIdentity::from_stored(&parsed).unwrap();
        assert_eq!(back.npub(), id.npub(), "npub survives");
        assert_eq!(back.browse_key.bytes(), id.browse_key.bytes(), "browse-key survives");
        assert_ne!(
            hex::encode(back.transport_key.bytes()),
            retired_secret,
            "the retired iroh secret is NOT resurrected — a fresh transport key is minted"
        );

        let resaved = serde_json::to_string(&back.to_stored().unwrap()).unwrap();
        assert!(
            !resaved.contains("iroh_secret_hex"),
            "re-saved identity must not re-emit the retired iroh_secret_hex field"
        );
        assert!(
            resaved.contains("transport_secret_hex"),
            "the re-saved identity carries the M18 transport secret under its own name"
        );
    }
}
