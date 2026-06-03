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
}
