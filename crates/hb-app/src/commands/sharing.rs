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

    // -----------------------------------------------------------------------
    // IPC-boundary tests (QURATOR-179's second concern) — dispatched as a real JSON
    // payload through `tauri::test::get_ipc_response`, exercising argument
    // deserialization itself, not the command fn called at its Rust signature.
    // -----------------------------------------------------------------------

    /// Builds an app with `get_share_settings` wired into a real `invoke_handler`, so IPC
    /// dispatch (unlike `guard_app()`, which calls command fns directly) can reach it.
    fn ipc_app() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![get_share_settings])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let dir = tempfile::tempdir().unwrap().keep();
        app.manage(DataStore::new(dir));
        app
    }

    /// Dispatches `get_share_settings` over the mocked IPC boundary with the given JSON body.
    fn dispatch_get_share_settings(
        app: &tauri::App<tauri::test::MockRuntime>,
        body: serde_json::Value,
    ) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
        let webview = tauri::WebviewWindowBuilder::new(app, "main", Default::default())
            .build()
            .unwrap();
        tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "get_share_settings".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(any(windows, target_os = "android")) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
    }

    #[test]
    fn get_share_settings_ipc_deserializes_a_well_formed_payload_and_returns_the_saved_value() {
        let app = ipc_app();
        app.state::<DataStore>()
            .save_share_settings("vault", &ShareSettings { root_path: Some("/x".into()) })
            .unwrap();

        let res = dispatch_get_share_settings(&app, serde_json::json!({ "slug": "vault" }));
        let got: ShareSettings = res.unwrap().deserialize().unwrap();
        assert_eq!(
            got.root_path,
            Some("/x".into()),
            "a real JSON payload, deserialized at the IPC boundary, must reach the command"
        );
        // mutation: rename the `slug` parameter in get_share_settings (e.g. to `collection_slug`)
        // — the payload `{"slug": "vault"}` would then miss the key the shim looks up and this
        // dispatch would return Err instead of the saved value.
    }

    #[test]
    fn get_share_settings_ipc_rejects_a_wrong_typed_slug_before_the_body_runs() {
        let app = ipc_app();
        let res = dispatch_get_share_settings(&app, serde_json::json!({ "slug": 42 }));
        assert!(
            res.is_err(),
            "a numeric slug must be rejected while deserializing IPC args, not coerced to a string"
        );
        // mutation: widen `slug: String` to `slug: serde_json::Value` in get_share_settings's
        // signature (body then reads `slug.as_str().unwrap_or_default()`) — a numeric slug would
        // then deserialize successfully instead of being rejected, and this dispatch would return
        // Ok instead of Err. No narrower one-line production mutation exists: the strict-typing
        // behaviour this test pins is tauri's `CommandItem` JSON deserializer
        // (`ipc/command.rs`), not anything `get_share_settings` itself does — which is exactly the
        // IPC-boundary gap this lane exists to test.
    }

    #[test]
    fn get_share_settings_ipc_rejects_a_payload_missing_the_slug_argument() {
        let app = ipc_app();
        let res = dispatch_get_share_settings(&app, serde_json::json!({}));
        assert!(
            res.is_err(),
            "a payload with no `slug` key must be rejected, not silently defaulted"
        );
        // mutation: change `slug: String` to `slug: Option<String>` in get_share_settings's
        // signature (body then reads `slug.unwrap_or_default()`) — a payload missing the key
        // would then deserialize to `None` instead of being rejected, and this dispatch would
        // return Ok instead of Err.
    }
}
