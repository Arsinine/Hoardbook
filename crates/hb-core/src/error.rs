use thiserror::Error;

#[derive(Error, Debug)]
pub enum HbError {
    #[error("invalid Hoardbook ID: {0}")]
    InvalidId(String),

    #[error("invalid Hoardbook ID prefix (expected 'hb1_')")]
    InvalidPrefix,

    #[error("invalid checksum in Hoardbook ID")]
    InvalidChecksum,

    #[error("invalid public key: {0}")]
    InvalidPublicKey(String),

    #[error("signature verification failed")]
    InvalidSignature,

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("hex decode error: {0}")]
    HexDecode(#[from] hex::FromHexError),

    #[error("message encryption failed")]
    EncryptionFailed,

    #[error("message decryption failed — wrong key or corrupted ciphertext")]
    DecryptionFailed,

    #[error("invalid encrypted message format")]
    InvalidEncryptedMessage,

    // --- v0.9 Nostr core (M1) ---
    #[error("nostr error: {0}")]
    Nostr(String),

    #[error("bech32 error: {0}")]
    Bech32(String),

    #[error("invalid share code: {0}")]
    InvalidShareCode(String),

    #[error("unsupported version byte: {0}")]
    UnsupportedVersion(u8),

    #[error("invalid event: {0}")]
    InvalidEvent(String),

    #[error("binding signed by unexpected identity")]
    WrongSigner,

    #[error("binding token expired")]
    BindingExpired,

    #[error("binding token not yet valid")]
    BindingNotYetValid,
}
