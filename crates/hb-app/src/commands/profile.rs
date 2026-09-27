//! Profile: a local draft (`Profile`) whose **public** fields are published as a NIP-01 **teaser**
//! (`hb-core::event::build_teaser`) to the relays. v5 (QURATOR-344): the teaser carries
//! `contact_hint` and a publish-computed `total_bytes`; `email`/`location` and the other private
//! fields stay out.

use hb_core::event::{build_teaser, Teaser};
use hb_core::types::Profile;
use nostr::prelude::*;
use tauri::State;

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    store::{DataStore, PublishedSave},
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
        // v5 (QURATOR-344): COMPUTED at publish from the published public collections' listings —
        // never user-typed, so the number cannot drift from the listing (one function, called
        // here). Private collections never count.
        total_bytes: total_published_public_bytes(store),
        // v5 (QURATOR-344): the hint rides the public teaser now (owner ruling rounds 10–11 —
        // the linked-handle risk is answered by the UI's copy, not crypto).
        contact_hint: profile.contact_hint.clone(),
        // QURATOR-142: input value is always OVERWRITTEN by build_teaser (derived there from the
        // `discoverable` param — the single source of truth); set for literal completeness only.
        hide_in_rosters: false,
    }
}

/// v5 (QURATOR-344): the teaser's `total_bytes` — the sum of file-leaf sizes over every
/// **published PUBLIC** collection's listing. Same enumeration pattern as
/// [`compute_content_types`]: published (`is_published`) public (`Visibility::Public`) drafts.
/// Private collections never count, so the teaser leaks nothing about private holdings.
fn total_published_public_bytes(store: &DataStore) -> u64 {
    let mut total = 0u64;
    for slug in store.list_collection_slugs().unwrap_or_default() {
        if store.is_published(&slug) {
            if let Ok(Some(col)) = store.load_collection_draft(&slug) {
                if col.visibility == hb_core::Visibility::Public {
                    total += hb_core::listing_size::sum_listing_bytes(&col.listing);
                }
            }
        }
    }
    total
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

    // Capture the revocation generation BEFORE the relay write (CWE-367): an Unpublish issued
    // mid-publish bumps it, and the guarded marker save below refuses to re-create the marker.
    let gen_at_start = store.published_generation(PROFILE_KEY);

    let client = net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    client.publish(&event).await.map_err(cmd_err)?;

    // Store the published event so unpublish can issue a NIP-09 deletion — guarded against a
    // concurrent unpublish (INV-8: a deliberately-skipped save, reported — never silent success).
    save_profile_marker_guarded(&store, &event.as_json(), gen_at_start)?;
    Ok(())
}

/// The marker-save tail of [`publish_profile`] (CWE-367). The relay write happens before the
/// marker save, so an Unpublish issued mid-publish would otherwise be silently undone by the save
/// re-creating the marker. This captured-generation compare-and-save is extracted so the pinning
/// test ends where production ends (P-6): on [`PublishedSave::Revoked`] it skips the write and
/// reports the revocation instead of succeeding (INV-8).
fn save_profile_marker_guarded(
    store: &DataStore,
    event_json: &str,
    gen_at_start: u64,
) -> CmdResult<()> {
    if store
        .save_published_guarded(PROFILE_KEY, event_json, gen_at_start)
        .map_err(cmd_err)?
        == PublishedSave::Revoked
    {
        return Err(
            "profile was unpublished while its publish was in flight; not re-creating the published marker"
                .into(),
        );
    }
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
            picture: None, hide_in_rosters: false,
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
    fn teaser_carries_contact_hint_but_not_email_or_location() {
        // v5 (QURATOR-344): contact_hint rides the teaser now (owner ruling rounds 10–11 — copy,
        // not crypto). email/location are still private: the Teaser struct has no such fields.
        let (_dir, store) = test_store();
        let profile = make_profile("Tester", vec!["video".into()]);
        let teaser = teaser_from_profile(&store, &profile);
        let json = serde_json::to_string(&teaser).unwrap();
        assert!(json.contains("contact_hint"), "teaser now carries contact_hint (v5)");
        assert!(json.contains("secret@example.com"));
        assert!(!json.contains("email"), "email stays out");
        assert!(!json.contains("location"), "location stays out");
        assert_eq!(teaser.display_name, "Tester");
        assert_eq!(teaser.tags, vec!["anime".to_string()]);
        // No collections in the store ⇒ the computed total is 0.
        assert_eq!(teaser.total_bytes, 0);
    }

    #[test]
    fn teaser_from_profile_not_discoverable_still_carries_contact_hint_no_email_location() {
        // devtest #5: build_teaser(.., false) at the teaser_from_profile seam — no `t` hashtags,
        // regardless. v5 (QURATOR-344): contact_hint rides the SIGNED BODY even when not
        // discoverable (npub lookup + share-code browse read the body); email/location stay out.
        let (_dir, store) = test_store();
        let profile = make_profile("Tester", vec!["video".into()]);
        let teaser = teaser_from_profile(&store, &profile);
        let id = hb_core::Identity::generate();
        let event = build_teaser(&id, &teaser, false).unwrap();
        assert_eq!(event.tags.hashtags().count(), 0, "no hashtags when not discoverable");
        assert!(event.content.contains("contact_hint"), "v5: the hint rides the body");
        assert!(event.content.contains("secret@example.com"));
        assert!(!event.content.contains("location"), "location stays out");
    }

    #[test]
    fn teaser_from_profile_total_bytes_sums_published_public_listings_only() {
        // v5 (QURATOR-344): total_bytes is COMPUTED at publish from the published PUBLIC
        // collections' listings — never user-typed, so the number and the listing cannot drift.
        // A published PRIVATE collection contributes nothing (the teaser leaks nothing about
        // private holdings), and a folder's own size is ignored (file leaves only).
        //
        // P-10 mutations (orchestrator applies; each must red this test, revert after):
        //   (a) in teaser_from_profile, replace `total_published_public_bytes(store)` with `0`;
        //   (b) in total_published_public_bytes, drop the `col.visibility == Visibility::Public`
        //       check (accept every collection) — the Private fixture must then inflate the total.
        let (_dir, store) = test_store();
        let mut profile = make_profile("Tester", vec!["video".into()]);
        profile.contact_hint = Some("hint rides public (v5)".into());

        let col = |slug: &str, visibility: Visibility, listing: Vec<hb_core::DirectoryItem>| Collection {
            slug: slug.into(),
            path_alias: slug.into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec![],
            tags: vec![],
            languages: vec![],
            visibility,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing,
        };
        let file = |name: &str, size: &str| hb_core::DirectoryItem {
            name: name.into(),
            item_type: hb_core::ItemType::File,
            size: Some(size.into()),
            format: None,
            year: None,
            tags: vec![],
            note: None,
            children: vec![],
        };
        // Public: 1.5 KB + 1023 B nested under a folder (the folder's own "9 GB" is ignored).
        store
            .save_collection_draft(&col(
                "pub",
                Visibility::Public,
                vec![
                    file("a.txt", "1.5 KB"),
                    hb_core::DirectoryItem {
                        name: "movies".into(),
                        item_type: hb_core::ItemType::Folder,
                        size: Some("9 GB".into()),
                        format: None,
                        year: None,
                        tags: vec![],
                        note: None,
                        children: vec![file("b.mkv", "1023 B")],
                    },
                ],
            ))
            .unwrap();
        store.save_published("pub", "{}").unwrap();
        // Private and published — must NOT count.
        store
            .save_collection_draft(&col("priv", Visibility::Private, vec![file("x", "2.0 GB")]))
            .unwrap();
        store.save_published("priv", "{}").unwrap();

        let teaser = teaser_from_profile(&store, &profile);
        assert_eq!(teaser.total_bytes, 1536 + 1023, "public only, file leaves only");
        assert_eq!(teaser.contact_hint.as_deref(), Some("hint rides public (v5)"));
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

    // ── CWE-367: the revocation-generation guard at the profile marker save ────────────
    // `publish_profile` writes to the relay BEFORE it saves the published marker, so an Unpublish
    // issued mid-publish can be silently undone: the relay write re-creates the marker and
    // resurrects a revoked profile. `save_profile_marker_guarded` is the exact marker-save tail
    // `publish_profile` runs at its save site (extracted so the test ends where production ends,
    // P-6). It is driven directly here because the full `publish_profile` needs a live relay
    // (`net::client` + `client.publish`); the guard itself is pure store I/O. This test is the
    // production-boundary kind: it calls the SAME function `publish_profile` calls, so reverting
    // production to unguarded `save_published` reds it.

    #[test]
    fn profile_marker_save_refuses_to_resurrect_after_mid_write_unpublish() {
        let (_dir, store) = test_store();
        let event_json = r#"{"kind":30078}"#;

        // A published profile, as it stands when the publish begins.
        store.save_published(PROFILE_KEY, event_json).unwrap();
        let gen_at_start = store.published_generation(PROFILE_KEY);

        // Mid-write Unpublish: `unpublish_profile` ends in `delete_published`, which removes the
        // marker AND bumps the revocation generation.
        store.delete_published(PROFILE_KEY).unwrap();
        assert!(!store.is_published(PROFILE_KEY), "premise: the unpublish removed the marker");

        // The marker-save tail of `publish_profile`, re-run after the relay write. It must detect
        // the bump, refuse to re-create the marker, and report the revocation explicitly.
        let outcome = save_profile_marker_guarded(&store, event_json, gen_at_start);
        assert!(
            outcome.is_err(),
            "a mid-write unpublish must report revocation, not silently succeed"
        );
        assert!(
            !store.is_published(PROFILE_KEY),
            "the revoked marker must NOT be resurrected"
        );
    }

    #[test]
    fn profile_marker_save_succeeds_when_generation_is_stable() {
        // A missing generation file reads as 0 (fresh install / first publish), and an un-bumped
        // generation must save — the guard must not over-reject and lose a legitimate publish.
        let (_dir, store) = test_store();
        let event_json = r#"{"kind":30078}"#;
        let gen_at_start = store.published_generation(PROFILE_KEY);
        assert_eq!(gen_at_start, 0, "a missing generation file reads as 0");
        save_profile_marker_guarded(&store, event_json, gen_at_start).unwrap();
        assert!(
            store.is_published(PROFILE_KEY),
            "an un-bumped generation must save the marker"
        );
    }

    // -----------------------------------------------------------------------
    // Command-dispatch tests (QURATOR-179) — through the real #[tauri::command]
    // shim (arg deserialization + state injection), not by calling bodies directly.
    // publish_profile / unpublish_profile reach relay I/O past their first guard, so every test
    // here is chosen to fail (or short-circuit past relay entirely) BEFORE `net::client` is ever
    // built — the settings relay is a dead loopback port precisely so a guard that were removed by
    // mistake would fail loudly at connect, never silently succeed.
    // -----------------------------------------------------------------------
    mod command_guards {
        use super::*;
        use crate::identity_state::AppIdentity;
        use tauri::Manager;

        fn guard_app(identity_loaded: bool) -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            store
                .save_settings(&crate::store::Settings {
                    relay_urls: vec!["ws://127.0.0.1:9".into()],
                    ..Default::default()
                })
                .unwrap();
            let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(
                identity_loaded.then(AppIdentity::generate),
            ));
            app.manage(identity);
            app.manage(store);
            app.manage(net::new_shared());
            app
        }

        #[tokio::test]
        async fn has_published_profile_command_reaches_the_managed_store() {
            let app = guard_app(true);
            assert!(!has_published_profile(app.state::<DataStore>()).await.unwrap());
            app.state::<DataStore>().save_published(PROFILE_KEY, "{}").unwrap();
            assert!(has_published_profile(app.state::<DataStore>()).await.unwrap());
            // mutation: change has_published_profile's body from
            // `Ok(store.is_published(PROFILE_KEY))` to `Ok(false)` — the second assert would red
            // despite the managed store (reached via State<DataStore>) holding a published marker.
        }

        #[tokio::test]
        async fn save_profile_command_persists_struct_fields_intact_for_get_profile_to_read_back() {
            let app = guard_app(true);
            let mut profile = make_profile("Tester", vec!["video".into()]);
            profile.contact_hint = Some("hint-value".into());
            profile.email = Some("email-value".into());
            save_profile(profile.clone(), app.state::<DataStore>()).await.unwrap();

            let got = get_profile(app.state::<DataStore>()).await.unwrap().unwrap();
            assert_eq!(
                got.contact_hint.as_deref(),
                Some("hint-value"),
                "contact_hint must not transpose with email"
            );
            assert_eq!(
                got.email.as_deref(),
                Some("email-value"),
                "email must not transpose with contact_hint"
            );
            assert_eq!(got.display_name, "Tester");
            // mutation: in save_profile's body, before `store.save_profile_draft(&profile)`, insert
            // `std::mem::swap(&mut profile.contact_hint, &mut profile.email);` (requires making the
            // `profile` parameter `mut`) — the two assert_eq! calls above would then red because
            // the struct arg-deserialized by the shim would reach the managed store transposed.
        }

        #[tokio::test]
        async fn publish_profile_command_refuses_without_identity_before_any_relay_io() {
            let app = guard_app(false);
            let err = publish_profile(
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert_eq!(err, "No identity loaded. Generate a keypair first.");
            // mutation: reword publish_profile's
            // `.ok_or("No identity loaded. Generate a keypair first.")?` string — this assert_eq
            // pins the exact text the frontend receives through the shim's error conversion.
        }

        #[tokio::test]
        async fn publish_profile_command_refuses_without_a_saved_draft_before_any_relay_io() {
            let app = guard_app(true);
            let err = publish_profile(
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap_err();
            assert_eq!(err, "No profile draft found. Save a profile first.");
            // mutation: reword publish_profile's
            // `.ok_or("No profile draft found. Save a profile first.")?` string — identity is
            // loaded here, so this pins the SECOND guard (still before `compute_content_types` /
            // `net::client`), distinct from the identity-missing test above.
        }

        #[tokio::test]
        async fn unpublish_profile_command_clears_the_published_marker_via_the_managed_store() {
            // identity_loaded(false): the seeded marker "{}" also fails Event::from_json on its
            // own, so the relay branch is unreachable from two independent directions — belt and
            // braces against ever dialing the dead loopback relay in this guard_app.
            let app = guard_app(false);
            app.state::<DataStore>().save_published(PROFILE_KEY, "{}").unwrap();
            assert!(app.state::<DataStore>().is_published(PROFILE_KEY));

            unpublish_profile(
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
                app.state::<SharedRelay>(),
            )
            .await
            .unwrap();

            assert!(
                !app.state::<DataStore>().is_published(PROFILE_KEY),
                "the managed store's marker must be cleared"
            );
            // mutation: change unpublish_profile's final line from
            // `store.delete_published(PROFILE_KEY).map_err(cmd_err)` to `Ok(())` — the marker
            // seeded directly on the SAME managed store the command was given would still read as
            // published after the call.
        }
    }
}
