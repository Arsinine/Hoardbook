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
    net,
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

/// The public teaser derived from a profile draft (private fields stripped).
fn profile_to_teaser(profile: &Profile) -> Teaser {
    Teaser {
        display_name: profile.display_name.clone(),
        bio: profile.bio.clone().unwrap_or_default(),
        tags: profile.tags.clone(),
        content_types: profile.content_types.clone(),
    }
}

#[tauri::command]
pub async fn publish_profile(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
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

    let teaser = profile_to_teaser(&profile);
    let event = build_teaser(&id_clone, &teaser).map_err(cmd_err)?;

    let client = net::connect(&id_clone, &store).await.map_err(cmd_err)?;
    let res = client.publish(&event).await;
    client.disconnect().await;
    res.map_err(cmd_err)?;

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

#[tauri::command]
pub async fn unpublish_profile(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<()> {
    // Best-effort NIP-09 deletion of the previously-published teaser, then drop the local marker.
    if let Some(json) = store.load_published(PROFILE_KEY).map_err(cmd_err)? {
        if let (Ok(event), Some(id_clone)) =
            (Event::from_json(&json), identity_clone(&identity).await)
        {
            if let Ok(deletion) = hb_net::build_deletion(&id_clone, &event) {
                if let Ok(client) = net::connect(&id_clone, &store).await {
                    let _ = client.publish(&deletion).await;
                    client.disconnect().await;
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
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        store.save_collection_draft(&col).unwrap();
        store.save_published(slug, "{}").unwrap();
    }

    #[test]
    fn teaser_omits_private_fields() {
        // The published teaser must never carry contact_hint / email / location.
        let profile = make_profile("Tester", vec!["video".into()]);
        let teaser = profile_to_teaser(&profile);
        let json = serde_json::to_string(&teaser).unwrap();
        assert!(!json.contains("contact_hint"), "teaser must not leak contact_hint");
        assert!(!json.contains("secret@example.com"));
        assert_eq!(teaser.display_name, "Tester");
        assert_eq!(teaser.tags, vec!["anime".to_string()]);
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
            tags: vec![], languages: vec![], visibility: Visibility::Public,
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
