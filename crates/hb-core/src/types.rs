use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// A single social / contact link the user chooses to display publicly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SocialLink {
    /// Lowercase platform identifier, e.g. "reddit", "discord", "matrix".
    pub platform: String,
    /// The user's handle or URL on that platform.
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Self-reported year the user started hoarding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u16>,
    /// Freeform string, e.g. "~12TB". Not validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_size: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Freeform contact hint (legacy field, prefer email / social_links).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_hint: Option<String>,
    /// Publicly visible email address — user opts in by setting this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// City or region the user is based in, e.g. "Tokyo" or "EU/Germany".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Optional social/contact links (Reddit, Discord, Matrix, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub social_links: Vec<SocialLink>,
    pub updated: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// One shared root directory — a user may publish multiple collections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    /// URL-safe slug derived from `path_alias` at creation time.
    /// Used as the stable key in the relay (`pubkey + slug`).
    pub slug: String,
    /// Human-readable display name shown to visitors.
    pub path_alias: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub item_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_size: Option<String>,
    /// Raw byte total of all files in this collection, computed at scan time.
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub content_type: Vec<String>,
    pub last_updated: DateTime<Utc>,
    pub listing: Vec<DirectoryItem>,
}

impl Collection {
    /// Derive a URL-safe slug from a display name.
    /// "Criterion Collection" → "criterion-collection"
    pub fn slug_from_alias(alias: &str) -> String {
        alias
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }
}

// ---------------------------------------------------------------------------
// DirectoryItem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryItem {
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: ItemType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub children: Vec<DirectoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ItemType {
    Folder,
    File,
}

// ---------------------------------------------------------------------------
// Succession
// ---------------------------------------------------------------------------

/// Signed by the *old* private key. Links old identity → new identity.
/// Published to relays alongside the old key so followers auto-migrate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Succession {
    pub old_key: String,
    pub new_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// HeartbeatBody
// ---------------------------------------------------------------------------

/// The body signed for a heartbeat request.
/// Only non-None fields are included in the JCS-canonical form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatBody {
    /// When true, the peer opts into the relay's public directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listed: Option<bool>,
    /// Optional iroh NodeAddr (base64-encoded). Included only in direct mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_addr: Option<String>,
    /// The sender's Hoardbook ID.
    pub public_key: String,
    /// ISO 8601 timestamp — included in the signed payload to prevent replay.
    pub signed_at: String,
}

// ---------------------------------------------------------------------------
// ChatMessage
// ---------------------------------------------------------------------------

/// The signed payload for a peer-to-peer chat message.
/// Stored in the relay addressed to the `to` key's inbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Recipient's Hoardbook ID.
    pub to: String,
    /// When `encrypted` is false: plaintext content (max 4096 bytes).
    /// When `encrypted` is true: `base64(nonce[24] || xchacha20poly1305_ciphertext)`.
    pub content: String,
    /// True when content is E2E-encrypted (X25519 DH + ChaCha20-Poly1305).
    #[serde(default)]
    pub encrypted: bool,
    /// Timestamp included in the signed payload to prevent replay.
    pub sent_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// ChannelMessage
// ---------------------------------------------------------------------------

/// Signed payload for a public channel post (e.g. General).
/// Posted to `/v1/channel/:channel` on the relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub channel: String,
    pub content: String,
    pub sent_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// StoredKeypair
// ---------------------------------------------------------------------------

/// On-disk representation of the keypair before OS keychain encryption.
/// The app layer wraps this in the OS keychain; hb-core only handles the
/// serialization format.
#[derive(Serialize, Deserialize)]
pub struct StoredKeypair {
    pub version: u32,
    pub hb_id: String,
    /// Hex-encoded 32-byte Ed25519 private key.
    pub private_key_hex: String,
}

impl std::fmt::Debug for StoredKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredKeypair")
            .field("version", &self.version)
            .field("hb_id", &self.hb_id)
            .field("private_key_hex", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_derivation() {
        assert_eq!(Collection::slug_from_alias("Criterion Collection"), "criterion-collection");
        assert_eq!(Collection::slug_from_alias("90s Anime!!"), "90s-anime");
        assert_eq!(Collection::slug_from_alias("VHS / Rips"), "vhs-rips");
        assert_eq!(Collection::slug_from_alias("  spaces  "), "spaces");
    }

    #[test]
    fn directory_item_serde_roundtrip() {
        let item = DirectoryItem {
            name: "Seven Samurai (1954)".into(),
            item_type: ItemType::File,
            size: Some("14.2GB".into()),
            format: Some("MKV".into()),
            year: Some(1954),
            tags: vec!["kurosawa".into()],
            note: None,
            children: vec![],
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: DirectoryItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, item.name);
        assert_eq!(back.item_type, ItemType::File);
        assert_eq!(back.size.as_deref(), Some("14.2GB"));
        assert_eq!(back.year, Some(1954));
        assert_eq!(back.tags, ["kurosawa"]);
        assert!(back.children.is_empty());
        // note: None must be absent from JSON, not serialized as null
        assert!(!json.contains("\"note\""), "absent note field must not appear in JSON");
    }
}
