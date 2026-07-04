//! The in-memory session identity — the two keys of the v0.9 Nostr model.
//!
//! 1. the secp256k1 `Identity` (the irreplaceable `npub`; signs every event + DM),
//! 2. the account **browse-key** (the "club pass" carried in the `hbk` share code; the default
//!    collection key).
//!
//! (The former third key — the Ed25519 iroh transport key — moved to the Mascara companion with
//! file transfer; Hoardbook moves no files, so it has no transport to key.)
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

/// The loaded session identity (both keys live in memory for the session).
pub struct AppIdentity {
    /// secp256k1 / `npub` — signs events + DMs.
    pub identity: Identity,
    /// Account browse-key (the "club pass").
    pub browse_key: SessionBrowseKey,
}

impl AppIdentity {
    /// Mint a fresh identity: a new npub + a fresh account browse-key.
    pub fn generate() -> Self {
        Self {
            identity: Identity::generate(),
            browse_key: SessionBrowseKey::new(rand::random()),
        }
    }

    /// Import an existing Nostr secret key (`nsec` or hex): the pasted key becomes the `npub`,
    /// and a **fresh** account browse-key is minted (the browse-key is regenerable and need not —
    /// must not — be carried in from elsewhere). Distinct from the whole-directory restore path.
    /// A malformed key is a reasoned `Err`, never a panic.
    pub fn from_nsec(nsec: &str) -> Result<Self> {
        let identity = Identity::from_secret(nsec)
            .map_err(|e| anyhow!(e.to_string()))
            .context("parsing the imported Nostr secret key")?;
        Ok(Self { identity, browse_key: SessionBrowseKey::new(rand::random()) })
    }

    /// Reconstruct from the on-disk record.
    pub fn from_stored(s: &StoredIdentity) -> Result<Self> {
        let identity = Identity::from_secret(&s.nsec)
            .map_err(|e| anyhow!(e.to_string()))
            .context("parsing stored nsec")?;
        let browse_key: [u8; 32] = hex::decode(&s.browse_key_hex)
            .context("decoding browse key")?
            .try_into()
            .map_err(|_| anyhow!("browse key must be exactly 32 bytes"))?;
        Ok(Self { identity, browse_key: SessionBrowseKey::new(browse_key) })
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

        let stored = id.to_stored().unwrap();
        let back = AppIdentity::from_stored(&stored).unwrap();

        assert_eq!(back.npub(), npub, "npub survives the storage roundtrip");
        assert_eq!(back.browse_key.bytes(), browse.bytes(), "account browse-key survives");
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

    /// M7 / v0.9.6: an existing **pre-cut 3-key** `keys.json` carried a third `iroh_secret_hex`
    /// (the now-removed iroh transport key). Dropping that field from `StoredIdentity` must not
    /// brick an existing identity — serde ignores the now-unknown field (`store.rs` has no
    /// `deny_unknown_fields`), so a legacy record loads as a 2-key identity. And re-saving must
    /// NOT re-emit the dropped field (a write-side regression would silently round-trip it).
    #[test]
    fn legacy_three_key_identity_loads_and_resaves_without_iroh_secret() {
        // Build a record with real keys, then write a **literal pre-M7 keys.json** — the exact
        // historical 3-key shape (`version` · `nsec` · `browse_key_hex` · `iroh_secret_hex`). Using a
        // literal fixture (not `to_value(stored)` + inject) keeps the test faithful to a real on-disk
        // file and makes it also fail if a future serde-rename of `nsec`/`browse_key_hex` breaks reads.
        let id = AppIdentity::generate();
        let s = id.to_stored().unwrap();
        let legacy_json = format!(
            r#"{{"version":{},"nsec":"{}","browse_key_hex":"{}","iroh_secret_hex":"{}"}}"#,
            s.version,
            s.nsec,
            s.browse_key_hex,
            "ab".repeat(32), // a retired 32-byte iroh secret, hex
        );

        // Read side: the legacy 3-key record deserializes — serde drops the now-unknown field
        // (StoredIdentity has no `deny_unknown_fields`) — and round-trips its two surviving secrets.
        let parsed: StoredIdentity = serde_json::from_str(&legacy_json).unwrap();
        let back = AppIdentity::from_stored(&parsed).unwrap();
        assert_eq!(back.npub(), id.npub(), "npub survives the 3-key→2-key migration");
        assert_eq!(back.browse_key.bytes(), id.browse_key.bytes(), "browse-key survives the migration");

        // Write side: re-serializing the loaded identity must not carry the dropped field back (a
        // future re-add of the field — or a stray `deny_unknown_fields` — would surface here).
        let resaved = serde_json::to_string(&back.to_stored().unwrap()).unwrap();
        assert!(
            !resaved.contains("iroh_secret_hex"),
            "re-saved identity must not re-emit the retired iroh_secret_hex field"
        );
    }
}
