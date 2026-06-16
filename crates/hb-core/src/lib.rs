#![forbid(unsafe_code)]

// --- legacy v0.4.3 core: Ed25519 identity / JCS / SignedEnvelope.
//     Still consumed by hb-app, hb-relay, hb-it; deleted in M4 with those consumers. ---
pub mod crypto;
pub mod envelope;
pub mod error;
pub mod jcs;
pub mod types;

// --- v0.9 Nostr core (M1): secp256k1 identity, NIP-01 events, NIP-44 listings,
//     the npub→iroh binding, the hbk share code. The foundation everything migrates to. ---
pub mod binding;
pub mod event;
pub mod identity;
pub mod listing;
pub mod sharecode;
mod tag_util;
pub mod version;

pub use crypto::{HbId, HoardbookKeypair, hb_id_decode, hb_id_encode};
pub use envelope::{DocType, SignedEnvelope};
pub use error::HbError;
pub use types::{
    ChatMessage, Collection, DirectoryItem, HeartbeatBody, ItemType, Profile,
    SocialLink, StoredKeypair,
};

pub use binding::{build_binding, verify_binding, Binding};
pub use identity::Identity;
pub use listing::{decrypt_listing, encrypt_listing, BrowseKey};
pub use sharecode::ShareCode;
pub use version::SCHEMA_V;
