use thiserror::Error;

#[derive(Error, Debug)]
pub enum HbError {
    #[error("invalid Hoardbook ID: {0}")]
    InvalidId(String),

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

    // --- v1.0 portable backup crypto (M5): Argon2id → XChaCha20-Poly1305 ---
    /// The archive is not a Hoardbook backup (bad magic / too short / truncated header).
    #[error("not a Hoardbook backup archive: {0}")]
    InvalidBackup(String),

    /// The archive declares a `format_ver` this build does not speak. A v1 decoder speaks
    /// only v1 — a bumped/unknown version is a clean reject, never a misparse.
    #[error("unsupported backup format version: {0}")]
    UnsupportedBackupVersion(u8),

    /// An encrypted (`mode=1`) archive was handed to decrypt without a passphrase.
    #[error("this backup is passphrase-encrypted — a passphrase is required to restore it")]
    PassphraseRequired,

    /// The passphrase is below the minimum length (measured on the NFKC-normalized form).
    #[error("passphrase too short — use at least {min} characters")]
    PassphraseTooShort { min: usize },

    /// A KDF parameter in the (not-yet-authenticated) header is outside the accepted range.
    /// Rejected *before* Argon2id runs so a hostile archive can't OOM / thread-exhaust restore.
    #[error("backup KDF parameter out of range: {0}")]
    BackupParamsOutOfRange(String),

    // --- M18: the manifest transport plane (INV-4′) ---
    /// The payload is over [`crate::MANIFEST_MAX_TRANSPORT_BYTES`] — INV-4′ mechanism 2. **The
    /// message names export deliberately**: past the browseable limit the full tree isn't the
    /// useful artifact anyway, so the plane declining to carry it is the honest answer rather than
    /// a capacity failure, and the user needs the route out in the same breath as the refusal.
    #[error("this listing is {declared} bytes — over the {max}-byte transport limit. Export it to a .hbmanifest file and send that instead")]
    PayloadTooLarge { declared: usize, max: usize },

    /// An inbound manifest is structurally unacceptable to the transport for a reason the byte
    /// ceiling does not cover — today, declaring more parts than the producer can ever build. Kept
    /// separate from [`Self::PayloadTooLarge`] because the route out is not export: no export
    /// produces this either, so it means a hostile or broken peer.
    #[error("manifest refused by the transport: {0}")]
    InvalidManifest(String),

    /// A transport ticket is malformed, or is bound to a different request than the one being
    /// redeemed (M18 W1 — one ticket per request).
    #[error("invalid transport ticket: {0}")]
    InvalidTicket(String),

    /// The ticket was already spent on a completed transfer. Consumed on SUCCESS, so this is a
    /// genuine replay, never a retry after a dropped connection.
    #[error("this transport ticket has already been redeemed — ask again for a new one")]
    TicketAlreadyRedeemed,

    /// The redeemer is no longer a contact in good standing (blocked, declined, or unknown). The
    /// redeem-time standing check is what keeps a valid-until-redeemed ticket revocable.
    #[error("the requester is no longer an approved contact")]
    TicketRedeemerNotInGoodStanding,
}
