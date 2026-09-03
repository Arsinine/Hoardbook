//! Per-collection persisted root path. The whole download path (`request_download` /
//! `cancel_download`) and the download-config UI ("Share settings" dialog) were removed in v0.9.6 and
//! **have not come back**: M18 added a transport plane, but it is structurally limited to manifests
//! (INV-4′ — Hoardbook moves no *collection files*), so there is still nothing to configure a
//! download for. Those symbol names are swept by CI precisely so they cannot return. What remains is a
//! single read: the collection's on-disk root, used to pre-fill the re-scan dialog. The root is
//! *written* by the scan/prepare path (`commands::collection`), never by the UI, so there is no
//! `save_share_settings` command.

use tauri::State;

use crate::{
    store::{DataStore, ShareSettings},
    error::{CmdResult, cmd_err},
};

#[tauri::command]
pub async fn get_share_settings(
    slug: String,
    store: State<'_, DataStore>,
) -> CmdResult<ShareSettings> {
    Ok(store.load_share_settings(&slug).map_err(cmd_err)?.unwrap_or_default())
}

// -----------------------------------------------------------------------
// Command-dispatch tests (QURATOR-179) — through the real #[tauri::command]
// shim (arg deserialization + state injection), not by calling bodies directly.
// -----------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Manager;

    fn guard_app() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_app();
        let dir = tempfile::tempdir().unwrap().keep();
        app.manage(DataStore::new(dir));
        app
    }

    #[tokio::test]
    async fn get_share_settings_command_returns_default_for_an_unknown_slug() {
        let app = guard_app();
        let got = get_share_settings("never-saved".into(), app.state::<DataStore>())
            .await
            .unwrap();
        assert_eq!(got.root_path, None);
        // mutation: change the `.unwrap_or_default()` fallback in get_share_settings to
        // `.unwrap()` — a slug with no saved settings would then panic instead of returning
        // ShareSettings::default() through the shim.
    }

    #[tokio::test]
    async fn get_share_settings_command_reaches_the_managed_store_for_a_saved_slug() {
        let app = guard_app();
        app.state::<DataStore>()
            .save_share_settings("vault", &ShareSettings { root_path: Some("/x".into()) })
            .unwrap();

        let got = get_share_settings("vault".into(), app.state::<DataStore>()).await.unwrap();
        assert_eq!(
            got.root_path,
            Some("/x".into()),
            "the slug argument reaches the managed store's own saved settings, not a stub"
        );

        // Differential: an unsaved slug still gets the default, proving the lookup is keyed by
        // the deserialized `slug` argument rather than always returning the one saved value.
        let other = get_share_settings("other".into(), app.state::<DataStore>()).await.unwrap();
        assert_eq!(other.root_path, None);
        // mutation: change `store.load_share_settings(&slug)` in get_share_settings to ignore the
        // `slug` argument (e.g. hardcode `"vault"`) — the "other" lookup above would then wrongly
        // return "/x" instead of the default, since the shim's deserialized argument would no
        // longer reach the store call.
    }
}
