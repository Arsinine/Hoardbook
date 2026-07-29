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
    /// Optional avatar as a `data:` URI — mirrors `event::Teaser::picture` (M13 item #13); see that
    /// field's doc for the privacy rails (never http(s), 16 KB cap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
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

// ---------------------------------------------------------------------------
// Published-envelope metadata ceilings
// ---------------------------------------------------------------------------

/// Ceilings on the collection metadata that rides in every published listing, in **characters**
/// (the unit the UI's `maxlength` counts, so the limit the user sees is the limit enforced).
///
/// These exist because the metadata and the directory tree share one byte budget.
/// `hb_net::truncate_listing` measures the serialized envelope first and hands `entries` whatever
/// is left (`split.rs:586`); with no ceiling, metadata alone could exceed `LISTING_MAX_BYTES` and
/// the subtraction saturates to zero — publishing a teaser containing **no entries at all**, or,
/// a little larger, an event the relay rejects outright. Capped, the envelope's worst case is a
/// few KB against a 40 KB budget, so the tree keeps effectively all of it.
///
/// Worst case is measured, not assumed: `envelope_at_caps_leaves_the_tree_its_budget`
/// (hb-app `collection.rs`) builds a Collection with every field at its ceiling and every
/// character the most expensive one JSON can encode, then asserts the headroom.
pub const MAX_DESCRIPTION_CHARS: usize = 255;
/// How many tags a collection may carry. Tags are the discovery surface, so this is a product
/// limit as much as a byte one — the budget would tolerate far more.
pub const MAX_TAGS: usize = 8;
pub const MAX_TAG_CHARS: usize = 32;
pub const MAX_CONTENT_TYPES: usize = 10;
pub const MAX_LANGUAGES: usize = 10;
/// Shared ceiling for a single content-type or language entry.
pub const MAX_LIST_ITEM_CHARS: usize = 32;
pub const MAX_PATH_ALIAS_CHARS: usize = 128;
/// **Not** a cap this code applies — the filesystem's, recorded so the envelope test can size its
/// worst case. The slug is the draft's filename (`collections/<slug>.draft.json`) as well as the
/// relay `d` tag, so a collection that exists on disk necessarily has a slug some filesystem
/// accepted. Clamping it would be actively harmful: an existing collection whose alias slugifies
/// longer than the cap would re-derive a *different* slug on its next rescan, miss its own draft,
/// lose the notes/visibility/sorted carried across, and strand its published listing under the old
/// `d` tag.
pub const FILESYSTEM_SLUG_CHARS: usize = 255;
/// `est_size` is written by `format_size` and never typed, but a restored or hand-edited draft can
/// carry anything — so it is bounded like the rest rather than trusted.
pub const MAX_EST_SIZE_CHARS: usize = 32;

/// Truncate to at most `max` characters, always on a character boundary (slicing a `str` at an
/// arbitrary byte index panics mid-codepoint).
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => s[..byte_idx].to_string(),
        None => s.to_string(),
    }
}

/// Bound a display alias at creation, before a slug is derived from it. Separate from
/// [`Collection::clamp_metadata`] on purpose — see that method for why a *stored* alias must never
/// be shortened after the fact.
pub fn truncate_alias(alias: &str) -> String {
    truncate_chars(alias, MAX_PATH_ALIAS_CHARS)
}

fn clamp_list(v: &mut Vec<String>, max_items: usize, max_chars: usize) {
    v.truncate(max_items);
    for s in v.iter_mut() {
        if s.chars().count() > max_chars {
            *s = truncate_chars(s, max_chars);
        }
    }
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

    /// Clamp the metadata that is **safe to persist**, for data the user just entered.
    ///
    /// **`path_alias` is excluded, and that exclusion is the whole point.** The slug is derived
    /// from the alias ([`Collection::slug_from_alias`]) and is both the relay `d` tag and the
    /// draft's filename. Shortening a *stored* alias therefore re-addresses the collection: its
    /// next rescan derives a different slug, misses its own draft, loses the notes/visibility/
    /// sorted carried across, and forks a second draft that defaults to `Public` while the original
    /// listing stays published under the old `d` tag — a Private collection can fork into a Public
    /// one. Clamping the identity itself was rejected for this reason; clamping the value the
    /// identity is *derived from* has the identical effect, which is easy to miss.
    ///
    /// Bound the alias only where it cannot re-address anything: at creation, before the slug is
    /// derived from it, and on the throwaway copy in [`Collection::clamp_for_publish`].
    pub fn clamp_metadata(&mut self) {
        if let Some(d) = &self.description {
            self.description = Some(truncate_chars(d, MAX_DESCRIPTION_CHARS));
        }
        if let Some(e) = &self.est_size {
            self.est_size = Some(truncate_chars(e, MAX_EST_SIZE_CHARS));
        }
        clamp_list(&mut self.tags, MAX_TAGS, MAX_TAG_CHARS);
        clamp_list(&mut self.content_types, MAX_CONTENT_TYPES, MAX_LIST_ITEM_CHARS);
        clamp_list(&mut self.languages, MAX_LANGUAGES, MAX_LIST_ITEM_CHARS);
    }

    /// [`Collection::clamp_metadata`] plus `path_alias` — the full envelope bound.
    ///
    /// **Only ever call this on a copy that is about to be serialized into a published listing,
    /// never on anything that gets saved.** This is what actually enforces the budget: the metadata
    /// and the directory tree share `LISTING_MAX_BYTES`, and `truncate_listing` measures the
    /// metadata first, so an unbounded envelope leaves the tree nothing and publishes a teaser with
    /// no entries in it.
    ///
    /// Bounding here rather than on load is deliberate. Load-time clamping reached the *stored*
    /// draft, which meant the background filesystem watcher re-saved a truncated copy and destroyed
    /// a legacy description no one had asked it to touch. A published copy is a fine thing to
    /// shorten; the user's own data is not.
    pub fn clamp_for_publish(&mut self) {
        self.clamp_metadata();
        self.path_alias = truncate_chars(&self.path_alias, MAX_PATH_ALIAS_CHARS);
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
            picture: None,
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
    fn clamp_metadata_holds_every_ceiling() {
        let mut col = Collection {
            slug: "keep-me".into(),
            path_alias: "a".repeat(500),
            description: Some("d".repeat(9_000)),
            item_count: 0,
            est_size: None,
            content_types: (0..40).map(|i| format!("{i}{}", "c".repeat(80))).collect(),
            tags: (0..40).map(|i| format!("{i}{}", "t".repeat(80))).collect(),
            languages: (0..40).map(|i| format!("{i}{}", "l".repeat(80))).collect(),
            visibility: Visibility::Public,
            sorted: false,
            last_updated: Utc::now(),
            listing: vec![],
        };
        col.clamp_metadata();

        assert_eq!(col.path_alias.chars().count(), 500, "clamp_metadata must leave the alias alone");
        assert_eq!(col.description.as_ref().unwrap().chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(col.tags.len(), MAX_TAGS);
        assert_eq!(col.content_types.len(), MAX_CONTENT_TYPES);
        assert_eq!(col.languages.len(), MAX_LANGUAGES);
        for t in &col.tags {
            assert!(t.chars().count() <= MAX_TAG_CHARS, "tag over ceiling: {t}");
        }
        for v in col.content_types.iter().chain(col.languages.iter()) {
            assert!(v.chars().count() <= MAX_LIST_ITEM_CHARS, "list item over ceiling: {v}");
        }
        // The slug is the relay `d` tag and the draft filename — clamping must never touch it.
        assert_eq!(col.slug, "keep-me", "clamp_metadata must not re-address the collection");
        // Truncation keeps the FIRST items, so what the user typed first survives.
        assert!(col.tags[0].starts_with('0'), "clamp kept the wrong end of the list");
    }

    #[test]
    fn clamp_metadata_never_touches_the_slugs_source() {
        // The regression this exists for: `slug` is derived from `path_alias`, and is both the
        // relay `d` tag and the draft filename. An earlier version clamped `path_alias` in
        // `clamp_metadata`, which is applied to STORED data — so a collection whose alias exceeded
        // the ceiling re-derived a different slug on its next rescan, missed its own draft, lost
        // the notes/visibility/sorted carried across, and forked a second draft defaulting to
        // Public while the original listing stayed published under the old `d` tag. A Private
        // collection could fork into a Public one.
        let long_alias = "A".repeat(MAX_PATH_ALIAS_CHARS + 40);
        let mut col = Collection {
            slug: Collection::slug_from_alias(&long_alias),
            path_alias: long_alias.clone(),
            description: Some("d".repeat(9_000)),
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Private,
            sorted: false,
            last_updated: Utc::now(),
            listing: vec![],
        };
        let slug_before = col.slug.clone();
        col.clamp_metadata();

        assert_eq!(col.path_alias, long_alias, "clamp_metadata must not shorten a STORED alias");
        assert_eq!(
            Collection::slug_from_alias(&col.path_alias),
            slug_before,
            "the alias must still re-derive the SAME slug — otherwise a rescan forks the collection"
        );
        assert_eq!(col.visibility, Visibility::Private, "visibility must survive clamping");
        // The fields it is responsible for are still bounded.
        assert_eq!(col.description.as_ref().unwrap().chars().count(), MAX_DESCRIPTION_CHARS);
    }

    #[test]
    fn clamp_for_publish_bounds_the_alias_on_the_outgoing_copy() {
        // The alias still has to be bounded somewhere, or a legacy oversize one starves the tree.
        // `clamp_for_publish` is that place: it runs on the throwaway copy that becomes the
        // published envelope, never on anything saved.
        let mut col = Collection {
            slug: "keep".into(),
            path_alias: "A".repeat(MAX_PATH_ALIAS_CHARS + 40),
            description: Some("d".repeat(9_000)),
            item_count: 0,
            est_size: Some("x".repeat(400)),
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: Utc::now(),
            listing: vec![],
        };
        col.clamp_for_publish();
        assert_eq!(col.path_alias.chars().count(), MAX_PATH_ALIAS_CHARS);
        assert_eq!(col.description.as_ref().unwrap().chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(col.est_size.as_ref().unwrap().chars().count(), MAX_EST_SIZE_CHARS);
        assert_eq!(col.slug, "keep", "even the publish copy must not re-address the collection");
    }

    #[test]
    fn clamp_metadata_truncates_on_character_boundaries() {
        // Slicing a str at an arbitrary byte index panics mid-codepoint. Every field gets a
        // multi-byte value whose ceiling falls inside a character.
        let mut col = Collection {
            slug: "s".into(),
            path_alias: "é".repeat(MAX_PATH_ALIAS_CHARS + 40),
            description: Some("🎬".repeat(MAX_DESCRIPTION_CHARS + 40)),
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec!["日".repeat(MAX_TAG_CHARS + 10)],
            languages: vec![],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: Utc::now(),
            listing: vec![],
        };
        col.clamp_metadata(); // must not panic

        assert_eq!(col.description.as_ref().unwrap().chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(col.tags[0].chars().count(), MAX_TAG_CHARS);
        // A 4-byte codepoint means the char ceiling is not the byte length — that is fine, and the
        // envelope test in hb-app is what pins the resulting bytes.
        assert_eq!(col.description.as_ref().unwrap().len(), MAX_DESCRIPTION_CHARS * 4);
    }

    #[test]
    fn clamp_metadata_is_idempotent_and_leaves_short_values_alone() {
        let mut col = Collection {
            slug: "s".into(),
            path_alias: "Shorts".into(),
            description: Some("a tweet-sized note".into()),
            item_count: 0,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec!["anime".into(), "bluray".into()],
            languages: vec!["en".into()],
            visibility: Visibility::Public,
            sorted: false,
            last_updated: Utc::now(),
            listing: vec![],
        };
        let before = col.clone();
        col.clamp_metadata();
        col.clamp_metadata();
        assert_eq!(col.path_alias, before.path_alias);
        assert_eq!(col.description, before.description);
        assert_eq!(col.tags, before.tags);
        assert_eq!(col.content_types, before.content_types);
        assert_eq!(col.languages, before.languages);
    }

    #[test]
    fn slug_from_alias_is_left_uncapped_on_purpose() {
        // The slug is the draft's FILENAME and the relay `d` tag. Capping it here would change the
        // slug an existing long-aliased collection re-derives on rescan — it would miss its own
        // draft, lose the notes/visibility/sorted that get carried across, and strand its published
        // listing under the old `d` tag. The filesystem is what bounds it, not this function.
        let alias = "The ".to_string() + &"Very ".repeat(80) + "Long Collection";
        let slug = Collection::slug_from_alias(&alias);
        assert!(
            slug.chars().count() > MAX_PATH_ALIAS_CHARS,
            "a long alias must still produce its full slug — clamping it orphans collections"
        );
        assert_eq!(slug, Collection::slug_from_alias(&alias), "slug derivation is not stable");
        assert_eq!(Collection::slug_from_alias("Criterion Collection"), "criterion-collection");
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
