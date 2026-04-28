#![forbid(unsafe_code)]

pub mod crypto;
pub mod envelope;
pub mod error;
pub mod jcs;
pub mod types;

pub use crypto::{HoardbookKeypair, hb_id_decode, hb_id_encode};
pub use envelope::{DocType, SignedEnvelope};
pub use error::HbError;
pub use types::{
    ChannelMessage, ChatMessage, Collection, DirectoryItem, HeartbeatBody, ItemType, Profile,
    SocialLink, StoredKeypair, Succession,
};
