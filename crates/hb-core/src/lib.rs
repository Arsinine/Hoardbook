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
pub mod priv_listing;
pub mod ratelimit;
pub mod sharecode;
pub mod snapshot;
mod tag_util;
pub mod topic;
pub mod version;

// --- Crate-wide invariant guards (INVARIANT_AUDIT.md): the I-3 wire-format freeze and the
//     I-5 INV-2 no-browse-key-broadcast sweep. Test-only modules; no runtime surface. ---
#[cfg(test)]
mod inv2_sweep;
#[cfg(test)]
mod wire_freeze;

pub use error::HbError;
pub use types::{Collection, DirectoryItem, ItemType, Profile, SocialLink, Visibility};

pub use backup::{
    decrypt_backup, encrypt_backup, is_encrypted_backup, BackupMode, BACKUP_FORMAT_VER,
    MIN_PASSPHRASE_LEN,
};
pub use binding::{build_binding, verify_binding, Binding};
pub use count::{count_distinct_online, count_distinct_userbase, is_canary, CANARY_MARKER};
pub use fingerprint::{fingerprint, Fingerprint};
pub use identity::Identity;
pub use listing::{decrypt_listing, encrypt_listing, BrowseKey, ContentKey};
pub use priv_listing::{open_private_listing, seal_private_listing, OpenedPrivate, KIND_PRIV_LISTING};
pub use ratelimit::{RelayRateLimiter, RELAY_WRITE_BURST, RELAY_WRITE_REFILL_PER_SEC};
pub use sharecode::ShareCode;
pub use topic::{
    announce_cooldown_remaining, build_announce, build_public_join, member_sign_keys, mint_invite,
    new_topic, open_announce, open_channel_item, open_membership, open_post, parse_announce,
    public_join_identity, public_join_keys, redeem_invite, roster, seal_announce, seal_membership,
    seal_post, topic_id_for_name, topic_root, validate_public_name, Announcement, ChannelItem,
    Membership, NonceSet, Post, TopicKey, TopicMeta, ANNOUNCE_MIN_INTERVAL_SECS, KIND_TOPIC_ANNOUNCE,
    KIND_TOPIC_INVITE, KIND_TOPIC_MEMBER, KIND_TOPIC_POST, MAX_TOPIC_DEPTH, POST_TTL_SECS,
    TOPIC_ROOTS,
};
pub use snapshot::{snapshot_fingerprint, unchanged_since, SnapshotFingerprint};
pub use version::SCHEMA_V;
