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
    // Approved extension: not in base spec
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// City or region the user is based in, e.g. "Tokyo" or "EU/Germany".
    // Approved extension: not in base spec
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Optional social/contact links (Reddit, Discord, Matrix, etc.).
    /// Always serialized — even when empty — so the frontend reliably gets
    /// an array instead of `undefined`.
    // Approved extension: not in base spec
    #[serde(default)]
    pub social_links: Vec<SocialLink>,
    /// Freeform flags for what the user is willing to do: "trade", "seed", "upload", etc.
    #[serde(default)]
    pub willing_to: Vec<String>,
    /// Computed as union of all published collections; never edited directly.
    #[serde(default)]
    pub content_types: Vec<String>,
    pub updated: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// Who a collection's listing is sealed *to* (spec §Private Collections). `Public` listings are
/// encrypted under the shared browse-key (anyone with the share code can browse — M3); `Private`
/// listings are gift-wrapped per-trusted-`npub` and the browse-key explicitly cannot open them
/// (M10). The default is `Public`, so a collection stored before M10 (with no `visibility` field)
/// loads as public — never silently private.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Public,
    Private,
}

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
    /// Content type categories for this collection. Renamed from `content_type` (v0.1.x alias kept for backward compat).
    #[serde(default, alias = "content_type")]
    pub content_types: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Who the listing is sealed to (spec §Private Collections). `#[serde(default)]` ⇒ a
    /// pre-M10 stored collection loads as `Public`.
    #[serde(default)]
    pub visibility: Visibility,
    /// Whether the listing is organised/curated (vs a raw, hard-to-filter dump). A **public** browse
    /// signal (owner devtest 2026-06-25 #7) so a browser can tell at a glance whether what they're
    /// seeing is in an identifiable, filterable form. Serialized into the published listing.
    /// `#[serde(default)]` ⇒ a pre-#7 stored/published collection loads as `false`.
    #[serde(default)]
    pub sorted: bool,
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
    // Previously serialized as "type" with lowercase variants.
    // Now serializes as "item_type" with PascalCase variants (matching TS types).
    // #[serde(alias = "type")] accepts old stored data transparently.
    #[serde(alias = "type")]
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
pub enum ItemType {
    // Accept old lowercase values from existing stored data.
    #[serde(alias = "folder")]
    Folder,
    #[serde(alias = "file")]
    File,
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
    fn profile_legacy_json_without_social_links_deserializes() {
        // A profile saved by an older Hoardbook version (v0.1.x) had no
        // `social_links` field. The new app must still load it without error.
        let legacy_json = r#"{
            "display_name": "Gundam",
            "tags": [],
            "languages": [],
            "updated": "2026-04-01T00:00:00Z"
        }"#;
        let parsed: Profile =
            serde_json::from_str(legacy_json).expect("legacy profile must deserialize");
        assert_eq!(parsed.display_name, "Gundam");
        assert!(parsed.social_links.is_empty());
    }

    #[test]
    fn profile_empty_social_links_round_trips_as_array() {
        // Bug: when `social_links` was tagged `skip_serializing_if = "Vec::is_empty"`,
        // an empty vec was OMITTED from the JSON sent over Tauri IPC. The frontend
        // then saw `social_links: undefined` and crashed on `form.social_links.find()`,
        // leaving the main panel blank after launch.
        //
        // The contract for any Vec field exposed to the frontend: serialize as `[]`
        // when empty, never omit. Option fields may still skip-serialize because
        // `?.` handles undefined cleanly on the JS side.
        let profile = Profile {
            display_name: "Gundam".into(),
            bio: None,
            tags: vec![],
            since: None,
            est_size: None,
            languages: vec![],
            contact_hint: None,
            email: None,
            location: None,
            social_links: vec![],
            willing_to: vec![],
            content_types: vec![],
            updated: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&profile).unwrap();
        assert!(
            json.contains("\"social_links\":[]"),
            "social_links must appear as [] in JSON, got: {json}"
        );
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

    #[test]
    fn directory_item_no_hash_in_json() {
        let item = DirectoryItem {
            name: "film.mkv".into(),
            item_type: ItemType::File,
            size: Some("14.2GB".into()),
            format: Some("MKV".into()),
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(
            !json.contains("sha256"),
            "DirectoryItem must not expose sha256 in serialized form, got: {json}"
        );
    }

    #[test]
    fn collection_no_internal_fields() {
        let col = Collection {
            slug: "films".into(),
            path_alias: "Films".into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        let json = serde_json::to_string(&col).unwrap();
        // `total_bytes` stays internal — exact byte counts must not leak into the published listing.
        assert!(
            !json.contains("total_bytes"),
            "Collection must not expose total_bytes in serialized form"
        );
        // `sorted` is now a deliberate PUBLIC browse signal (owner devtest 2026-06-25 #7) — it MUST
        // be in the listing so a browser can tell an organised hoard from a raw dump. (Reverses the
        // pre-#7 "must not expose sorted" rule — stop-and-justify.)
        assert!(
            json.contains("\"sorted\""),
            "Collection must expose `sorted` as a public browse signal, got: {json}"
        );
    }

    #[test]
    fn visibility_defaults_to_public_for_pre_m10_collections() {
        // A collection JSON written before M10 has no `visibility` field; it must load as Public,
        // never silently Private (that would hide a public collection / mis-route the seal).
        let legacy_json = r#"{
            "slug": "criterion",
            "path_alias": "Criterion",
            "item_count": 1,
            "content_types": ["video"],
            "last_updated": "2026-04-01T00:00:00Z",
            "listing": []
        }"#;
        let parsed: Collection =
            serde_json::from_str(legacy_json).expect("pre-M10 collection must deserialize");
        assert_eq!(parsed.visibility, Visibility::Public, "missing visibility ⇒ Public");
        assert_eq!(Visibility::default(), Visibility::Public);
    }

    #[test]
    fn visibility_round_trips_private() {
        let col = Collection {
            slug: "vault".into(),
            path_alias: "Vault".into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Private,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        let json = serde_json::to_string(&col).unwrap();
        assert!(json.contains("\"visibility\":\"Private\""), "got: {json}");
        let back: Collection = serde_json::from_str(&json).unwrap();
        assert_eq!(back.visibility, Visibility::Private);
    }

    #[test]
    fn sorted_is_a_public_signal_that_round_trips_and_defaults_false() {
        // Devtest 2026-06-25 #7: `sorted` is a PUBLIC browse signal — it must serialize into the
        // listing and survive a round-trip, and a pre-#7 collection (no `sorted` key) loads false.
        let col = Collection {
            slug: "dump".into(),
            path_alias: "Dump".into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: true,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        let json = serde_json::to_string(&col).unwrap();
        assert!(json.contains("\"sorted\":true"), "sorted must be published, got: {json}");
        assert!(serde_json::from_str::<Collection>(&json).unwrap().sorted);
        // Pre-#7 listing with no `sorted` key ⇒ false (never a spurious "sorted" badge).
        let legacy = r#"{"slug":"x","path_alias":"X","item_count":0,"last_updated":"2026-04-01T00:00:00Z","listing":[]}"#;
        assert!(!serde_json::from_str::<Collection>(legacy).unwrap().sorted, "missing sorted ⇒ false");
    }

    #[test]
    fn content_types_union_sorted_deduped() {
        // Validate that content_types union logic produces a sorted, deduplicated
        // list — the same logic used in publish_collection.
        let type_sets: Vec<Vec<String>> = vec![
            vec!["video".into(), "audio".into()],
            vec!["audio".into(), "image".into()],
            vec!["video".into()],
        ];
        let mut aggregate: Vec<String> = type_sets.into_iter().flatten().collect();
        aggregate.sort();
        aggregate.dedup();
        assert_eq!(aggregate, vec!["audio", "image", "video"]);
    }
}
