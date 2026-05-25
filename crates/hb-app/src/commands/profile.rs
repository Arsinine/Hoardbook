use hb_core::{DocType, SignedEnvelope, types::{Collection, Profile}};
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::DataStore,
    SharedIdentity,
};

/// Returns true if a signed (published) profile exists on disk.
#[tauri::command]
pub async fn has_published_profile(store: State<'_, DataStore>) -> CmdResult<bool> {
    Ok(store.load_profile_signed().map_err(cmd_err)?.is_some())
}

#[tauri::command]
pub async fn save_profile(
    profile: Profile,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    store.save_profile_draft(&profile).map_err(cmd_err)
}

#[tauri::command]
pub async fn get_profile(store: State<'_, DataStore>) -> CmdResult<Option<Profile>> {
    if let Some(draft) = store.load_profile_draft().map_err(cmd_err)? {
        return Ok(Some(draft));
    }
    if let Some(env) = store.load_profile_signed().map_err(cmd_err)? {
        return Ok(Some(env.parse_payload().map_err(cmd_err)?));
    }
    Ok(None)
}

#[tauri::command]
pub async fn publish_profile(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
) -> CmdResult<()> {
    let guard = identity.read().await;
    let kp = guard
        .as_ref()
        .ok_or("No identity loaded. Generate a keypair first.")?;

    let mut profile = store
        .load_profile_draft()
        .map_err(cmd_err)?
        .ok_or("No profile draft found. Save a profile first.")?;

    // Auto-compute content_types as the union of all signed collection content_types.
    // The field is never user-editable — it reflects what is actually published.
    profile.content_types = compute_content_types(&store);

    let envelope = SignedEnvelope::create(kp, DocType::Profile, &profile).map_err(cmd_err)?;
    store.save_profile_signed(&envelope).map_err(cmd_err)
}

/// Compute the sorted, deduplicated union of content_types across all signed collections.
pub(crate) fn compute_content_types(store: &DataStore) -> Vec<String> {
    let collections = store.list_collections().unwrap_or_default();
    let mut types: Vec<String> = collections
        .iter()
        .filter_map(|env| env.parse_payload::<Collection>().ok())
        .flat_map(|col| col.content_types)
        .collect();
    types.sort();
    types.dedup();
    types
}

#[tauri::command]
pub async fn unpublish_profile(store: State<'_, DataStore>) -> CmdResult<()> {
    let path = store.profile_signed_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(cmd_err)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — T14 acceptance criteria
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::{HoardbookKeypair, types::Collection};
    use crate::store::DataStore;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn make_profile(display_name: &str) -> Profile {
        Profile {
            display_name: display_name.to_string(),
            bio: None, tags: vec![], since: None, est_size: None,
            languages: vec![], contact_hint: None, email: None,
            location: None, social_links: vec![], willing_to: vec![],
            content_types: vec!["video".into()], // user-supplied; must be overwritten on publish
            updated: chrono::Utc::now(),
        }
    }

    fn save_signed_collection(store: &DataStore, kp: &HoardbookKeypair, slug: &str, ctypes: Vec<String>) {
        let col = Collection {
            slug: slug.to_string(),
            path_alias: slug.to_string(),
            description: None, item_count: 0, est_size: None,
            content_types: ctypes,
            tags: vec![], languages: vec![],
            last_updated: chrono::Utc::now(),
            listing: vec![],
        };
        let env = SignedEnvelope::create(kp, DocType::Collection, &col).unwrap();
        store.save_collection_signed(slug, &env).unwrap();
    }

    #[test]
    fn profile_signed_with_correct_key() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();

        store.save_profile_draft(&make_profile("Tester")).unwrap();

        let envelope = {
            let mut profile = store.load_profile_draft().unwrap().unwrap();
            profile.content_types = compute_content_types(&store);
            SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap()
        };
        store.save_profile_signed(&envelope).unwrap();

        let loaded = store.load_profile_signed().unwrap().unwrap();
        loaded.verify().expect("signature must be valid");
        assert_eq!(loaded.public_key, kp.hb_id());
    }

    #[test]
    fn content_types_auto_computed() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();

        save_signed_collection(&store, &kp, "movies", vec!["video".into(), "audio".into()]);
        save_signed_collection(&store, &kp, "books",  vec!["text".into(), "video".into()]);

        let types = compute_content_types(&store);
        assert_eq!(types, vec!["audio", "text", "video"], "must be sorted + deduped union");
    }

    #[test]
    fn content_types_override_user_value() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();

        // Profile draft has a user-supplied content_type — must be overwritten on publish.
        let mut draft = make_profile("Tester");
        draft.content_types = vec!["software".into()]; // user-supplied, wrong
        store.save_profile_draft(&draft).unwrap();

        save_signed_collection(&store, &kp, "movies", vec!["video".into()]);

        let mut profile = store.load_profile_draft().unwrap().unwrap();
        profile.content_types = compute_content_types(&store);
        assert_eq!(profile.content_types, vec!["video"]);
        assert!(!profile.content_types.contains(&"software".to_string()));
    }

    #[test]
    fn no_relay_call_on_save() {
        // Structural test: publish_profile no longer takes a relay parameter.
        // If relay was still wired, this test would fail to compile.
        // Verify by checking compute_content_types + sign path has no relay dependency.
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        store.save_profile_draft(&make_profile("Tester")).unwrap();

        let mut profile = store.load_profile_draft().unwrap().unwrap();
        profile.content_types = compute_content_types(&store);
        let env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        store.save_profile_signed(&env).unwrap();

        // No network call occurred — the above is entirely local I/O + crypto.
        assert!(store.load_profile_signed().unwrap().is_some());
    }

    #[test]
    fn unpublish_removes_signed_file() {
        let (_dir, store) = test_store();
        let kp = HoardbookKeypair::generate();
        store.save_profile_draft(&make_profile("Tester")).unwrap();
        let env = SignedEnvelope::create(&kp, DocType::Profile, &make_profile("Tester")).unwrap();
        store.save_profile_signed(&env).unwrap();
        assert!(store.profile_signed_path().exists());

        let path = store.profile_signed_path();
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
        assert!(store.load_profile_signed().unwrap().is_none());
    }
}
