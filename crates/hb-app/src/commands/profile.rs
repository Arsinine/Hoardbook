//! Profile: a local draft (`Profile`) whose **public** fields are published as a NIP-01 **teaser**
//! (`hb-core::event::build_teaser`) to the relays. The teaser deliberately omits `contact_hint`
//! and other private fields — only `display_name` / `bio` / `tags` / `content_types` are public.

use hb_core::event::{build_teaser, Teaser};
use hb_core::types::Profile;
use nostr::prelude::*;
use tauri::State;

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    store::DataStore,
};

/// Key under which the published teaser event is stored locally (enables NIP-09 unpublish).
const PROFILE_KEY: &str = "profile";

/// Returns true if a teaser has been published.
#[tauri::command]
pub async fn has_published_profile(store: State<'_, DataStore>) -> CmdResult<bool> {
    Ok(store.is_published(PROFILE_KEY))
}

#[tauri::command]
pub async fn save_profile(profile: Profile, store: State<'_, DataStore>) -> CmdResult<()> {
    store.save_profile_draft(&profile).map_err(cmd_err)
}

#[tauri::command]
pub async fn get_profile(store: State<'_, DataStore>) -> CmdResult<Option<Profile>> {
    store.load_profile_draft().map_err(cmd_err)
}

/// Build the teaser to publish from a profile draft (M13 W5 item 2). `content_types` is read
/// straight off `profile.content_types` — the caller already recomputed + persisted it via
/// [`compute_content_types`] (unchanged since M9). `tags`, however, is the union of the profile's
/// **own** tags and [`compute_collection_tags`] — computed HERE, teaser-only, and never written
/// back onto `profile.tags`. The asymmetry is deliberate: the profile Tags editor stays the user's
/// own list; only `content_types` gets the persisted-union treatment M9 already established.
pub(crate) fn teaser_from_profile(store: &DataStore, profile: &Profile) -> Teaser {
    let mut tags = profile.tags.clone();
    tags.extend(compute_collection_tags(store));
    tags.sort();
    tags.dedup();
    Teaser {
        display_name: profile.display_name.clone(),
        bio: profile.bio.clone().unwrap_or_default(),
        tags,
        content_types: profile.content_types.clone(),
        picture: profile.picture.clone(),
    }
}

#[tauri::command]
pub async fn publish_profile(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let id_clone = {
        let guard = identity.read().await;
        guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?.identity.clone()
    };

    let mut profile = store
        .load_profile_draft()
        .map_err(cmd_err)?
        .ok_or("No profile draft found. Save a profile first.")?;

    // content_types reflect what's actually published (union of published collections).
    profile.content_types = compute_content_types(&store);
    store.save_profile_draft(&profile).map_err(cmd_err)?;

    // devtest #5: discoverability is opt-in, default off (a failed settings load is treated as off).
    let discoverable = store.load_settings().map_err(cmd_err)?.unwrap_or_default().discoverable;
    let teaser = teaser_from_profile(&store, &profile);
    let event = build_teaser(&id_clone, &teaser, discoverable).map_err(cmd_err)?;

    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    client.publish(&event).await.map_err(cmd_err)?;

    // Store the published event so unpublish can issue a NIP-09 deletion.
    store.save_published(PROFILE_KEY, &event.as_json()).map_err(cmd_err)?;
    Ok(())
}

/// Compute the sorted, deduplicated union of content_types across all **published, public**
/// collections. **Private collections are excluded (M10, F25):** the public teaser must leak
/// nothing about private holdings, so a private-only content-type never surfaces as a public `t`
/// tag (and is not tag-discoverable).
pub(crate) fn compute_content_types(store: &DataStore) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    for slug in store.list_collection_slugs().unwrap_or_default() {
        if store.is_published(&slug) {
            if let Ok(Some(col)) = store.load_collection_draft(&slug) {
                if col.visibility == hb_core::Visibility::Public {
                    types.extend(col.content_types);
                }
            }
        }
    }
    types.sort();
    types.dedup();
    types
}

/// Compute the sorted, deduplicated union of `tags` across all **published, public** collections —
/// mirrors [`compute_content_types`] exactly, including its privacy pin (M10/F25): a Private
/// collection's tags must never surface in the public teaser union.
pub(crate) fn compute_collection_tags(store: &DataStore) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for slug in store.list_collection_slugs().unwrap_or_default() {
        if store.is_published(&slug) {
            if let Ok(Some(col)) = store.load_collection_draft(&slug) {
                if col.visibility == hb_core::Visibility::Public {
                    tags.extend(col.tags);
                }
            }
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

#[tauri::command]
pub async fn unpublish_profile(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    // Best-effort NIP-09 deletion of the previously-published teaser, then drop the local marker.
    if let Some(json) = store.load_published(PROFILE_KEY).map_err(cmd_err)? {
        if let (Ok(event), Some(id_clone)) =
            (Event::from_json(&json), identity_clone(&identity).await)
        {
            if let Ok(deletion) = hb_net::build_deletion(&id_clone, &event) {
                if let Ok(client) = net::client(&id_clone, &store, &relay).await {
                    let _ = client.publish(&deletion).await;
                }
            }
        }
    }
    store.delete_published(PROFILE_KEY).map_err(cmd_err)
}

async fn identity_clone(identity: &SharedIdentity) -> Option<hb_core::Identity> {
    identity.read().await.as_ref().map(|id| id.identity.clone())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DataStore;
    use hb_core::types::{Collection, Visibility};
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn make_profile(name: &str, content_types: Vec<String>) -> Profile {
        Profile {
            display_name: name.into(),
            bio: Some("90s anime".into()),
            tags: vec!["anime".into()],
            since: None,
            est_size: None,
            languages: vec![],
            contact_hint: Some("secret@example.com".into()),
            email: None,
            location: None,
            social_links: vec![],
            willing_to: vec![],
            content_types,
            picture: None,
            updated: chrono::Utc::now(),
        }
    }

    fn published_collection(store: &DataStore, slug: &str, ctypes: Vec<String>) {
        published_collection_vis(store, slug, ctypes, Visibility::Public);
    }

    fn published_collection_vis(
        store: &DataStore,
        slug: &str,
        ctypes: Vec<String>,
        visibility: Visibility,
    ) {
        let col = Collection {
            slug: slug.into(),
            path_alias: slug.into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: ctypes,
            tags: vec![],
            languages: vec![],
            visibility,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        store.save_published(slug, "{}").unwrap();
    }

    #[test]
    fn teaser_omits_private_fields() {
        // The published teaser must never carry contact_hint / email / location.
        let (_dir, store) = test_store();
        let profile = make_profile("Tester", vec!["video".into()]);
        let teaser = teaser_from_profile(&store, &profile);
        let json = serde_json::to_string(&teaser).unwrap();
        assert!(!json.contains("contact_hint"), "teaser must not leak contact_hint");
        assert!(!json.contains("secret@example.com"));
        assert_eq!(teaser.display_name, "Tester");
        assert_eq!(teaser.tags, vec!["anime".to_string()]);
    }

    #[test]
    fn teaser_from_profile_not_discoverable_emits_no_hashtags_and_still_omits_private_fields() {
        // devtest #5: build_teaser(.., false) at the teaser_from_profile seam — no `t` hashtags, and
        // the private-field omission (contact_hint / email / location) still holds regardless.
        let (_dir, store) = test_store();
        let profile = make_profile("Tester", vec!["video".into()]);
        let teaser = teaser_from_profile(&store, &profile);
        let id = hb_core::Identity::generate();
        let event = build_teaser(&id, &teaser, false).unwrap();
        assert_eq!(event.tags.hashtags().count(), 0, "no hashtags when not discoverable");
        assert!(!event.content.contains("contact_hint"));
        assert!(!event.content.contains("secret@example.com"));
    }

    #[test]
    fn teaser_from_profile_carries_picture_through() {
        let (_dir, store) = test_store();
        let mut profile = make_profile("Tester", vec!["video".into()]);
        profile.picture = Some("data:image/webp;base64,AAAA".into());
        let teaser = teaser_from_profile(&store, &profile);
        assert_eq!(teaser.picture.as_deref(), Some("data:image/webp;base64,AAAA"));
        let id = hb_core::Identity::generate();
        let event = build_teaser(&id, &teaser, true).unwrap();
        let parsed = hb_core::event::parse_teaser(&event).unwrap();
        assert_eq!(parsed.picture.as_deref(), Some("data:image/webp;base64,AAAA"));
    }

    fn published_collection_tags(
        store: &DataStore,
        slug: &str,
        tags: Vec<String>,
        visibility: Visibility,
    ) {
        let col = Collection {
            slug: slug.into(),
            path_alias: slug.into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags,
            languages: vec![],
            visibility,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        store.save_published(slug, "{}").unwrap();
    }

    #[test]
    fn compute_collection_tags_unions_published_public_only() {
        let (_dir, store) = test_store();
        published_collection_tags(
            &store,
            "movies",
            vec!["classic".into(), "arthouse".into()],
            Visibility::Public,
        );
        published_collection_tags(
            &store,
            "books",
            vec!["scifi".into(), "classic".into()],
            Visibility::Public,
        );
        // A draft that is NOT published must not contribute (mirrors
        // content_types_union_over_published_collections).
        let unpublished = Collection {
            slug: "drafts".into(), path_alias: "drafts".into(), description: None,
            item_count: 0, est_size: None, content_types: vec![], tags: vec!["hidden".into()],
            languages: vec![], visibility: Visibility::Public, sorted: false,
            last_updated: chrono::Utc::now(), listing: vec![],
        };
        store.save_collection_draft(&unpublished).unwrap();

        let tags = compute_collection_tags(&store);
        assert_eq!(tags, vec!["arthouse", "classic", "scifi"], "sorted+deduped union of published-public tags");
        assert!(!tags.contains(&"hidden".to_string()));
    }

    #[test]
    fn teaser_tags_exclude_private_collections() {
        // Sibling of teaser_aggregation_excludes_private_collections (content_types): a private
        // collection's tag must appear neither in the union nor as a `t` hashtag on the built
        // teaser event.
        let (_dir, store) = test_store();
        published_collection_tags(&store, "public-films", vec!["arthouse".into()], Visibility::Public);
        published_collection_tags(&store, "secret-stash", vec!["forbidden".into()], Visibility::Private);

        let tags = compute_collection_tags(&store);
        assert_eq!(tags, vec!["arthouse".to_string()]);
        assert!(!tags.contains(&"forbidden".to_string()));

        let profile = make_profile("Tester", vec![]);
        let teaser = teaser_from_profile(&store, &profile);
        assert!(!teaser.tags.contains(&"forbidden".to_string()));

        let id = hb_core::Identity::generate();
        let event = build_teaser(&id, &teaser, true).unwrap();
        let hashtags: Vec<&str> = event.tags.iter().filter_map(|t| t.content()).collect();
        assert!(
            !hashtags.contains(&"forbidden"),
            "a private collection's tag must never be an emitted `t` hashtag"
        );
    }

    #[test]
    fn teaser_tags_union_profile_and_public_collection_tags() {
        let (_dir, store) = test_store();
        published_collection_tags(
            &store,
            "movies",
            vec!["classic".into(), "anime".into()],
            Visibility::Public,
        );
        let profile = make_profile("Tester", vec![]); // make_profile's tags are fixed to ["anime"]

        let teaser = teaser_from_profile(&store, &profile);
        assert_eq!(
            teaser.tags,
            vec!["anime".to_string(), "classic".to_string()],
            "profile tags ∪ public collection tags, sorted+deduped"
        );
        assert_eq!(
            profile.tags,
            vec!["anime".to_string()],
            "the union is teaser-only — the profile draft's own tags are untouched"
        );
    }

    #[test]
    fn content_types_union_over_published_collections() {
        let (_dir, store) = test_store();
        published_collection(&store, "movies", vec!["video".into(), "audio".into()]);
        published_collection(&store, "books", vec!["text".into(), "video".into()]);
        // A draft that is NOT published must not contribute.
        let unpublished = Collection {
            slug: "drafts".into(), path_alias: "drafts".into(), description: None,
            item_count: 0, est_size: None, content_types: vec!["software".into()],
            tags: vec![], languages: vec![], visibility: Visibility::Public, sorted: false,
            last_updated: chrono::Utc::now(), listing: vec![],
        };
        store.save_collection_draft(&unpublished).unwrap();

        let types = compute_content_types(&store);
        assert_eq!(types, vec!["audio", "text", "video"], "sorted+deduped union of published only");
        assert!(!types.contains(&"software".to_string()));
    }

    #[test]
    fn teaser_aggregation_excludes_private_collections() {
        // M10/F25: a content-type that exists ONLY in a private collection must never surface in
        // the public teaser aggregation (it would otherwise leak a private holding + become
        // tag-discoverable). A published *private* collection contributes nothing.
        let (_dir, store) = test_store();
        published_collection(&store, "public-films", vec!["video".into()]);
        published_collection_vis(&store, "secret-stash", vec!["forbidden".into()], Visibility::Private);

        let types = compute_content_types(&store);
        assert_eq!(types, vec!["video"], "only the public collection's type appears");
        assert!(
            !types.contains(&"forbidden".to_string()),
            "a private-only content-type must NOT leak into the public teaser"
        );
    }
}
