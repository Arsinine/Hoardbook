#![forbid(unsafe_code)]

// --- Shared domain types (collections / profiles) consumed across crates. ---
pub mod error;
pub mod types;

// --- v0.9 Nostr core: secp256k1 identity, NIP-01 events, NIP-44 listings,
//     the presence freshness binding, the hbk share code. (The legacy Ed25519
//     identity / JCS / signed-envelope core was removed with its hb-app consumer in M4; the
//     npub→iroh-node binding + xfer gate moved to the Mascara companion with file transfer in
//     v0.9.6 — Hoardbook moves no files.) ---
pub mod backup;
pub mod binding;
pub mod count;
pub mod event;
pub mod fingerprint;
pub mod identity;
pub mod listing;
pub mod sharecode;
pub mod snapshot;
mod tag_util;
pub mod version;

pub use error::HbError;
pub use types::{Collection, DirectoryItem, ItemType, Profile, SocialLink};

pub use backup::{
    decrypt_backup, encrypt_backup, is_encrypted_backup, BackupMode, BACKUP_FORMAT_VER,
    MIN_PASSPHRASE_LEN,
};
pub use binding::{build_binding, verify_binding, Binding};
pub use count::{count_distinct_online, count_distinct_userbase, is_canary, CANARY_MARKER};
pub use fingerprint::{fingerprint, Fingerprint};
pub use identity::Identity;
pub use listing::{decrypt_listing, encrypt_listing, BrowseKey};
pub use sharecode::ShareCode;
pub use snapshot::{snapshot_fingerprint, unchanged_since, SnapshotFingerprint};
pub use version::SCHEMA_V;
