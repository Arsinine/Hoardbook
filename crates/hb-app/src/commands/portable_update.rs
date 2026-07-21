//! Portable self-updater commands (devtest v0.12.1 follow-up) — the update path for the loose,
//! installer-less `Hoardbook.exe` (the build people actually run). The NSIS updater
//! (`commands::update`) stays untouched as the **regression path** for installed builds; the app
//! routes to whichever matches how it was launched ([`updater_is_portable`]).
//!
//! Flow: [`check_portable_update`] reads the signed `portable.json` manifest and reports a newer
//! version; [`apply_portable_update`] downloads the stable `Hoardbook.exe`, **verifies its minisign
//! signature under the SAME key as the NSIS updater** (`portable_update_logic::verify_signature`), then
//! swaps the running exe in place via `self_replace` and relaunches. **No unsigned binary is ever
//! written over the running exe** — verification happens before anything touches it. The
//! download/replace is the I/O boundary (not offline-testable); the pure logic + the signature check
//! are unit-tested in `portable_update_logic`.

use std::time::Duration;

use serde::Serialize;

use crate::error::{cmd_err, CmdResult};
use crate::portable_update_logic::{
    current_target_key, is_newer, is_portable_build, is_trusted_artifact_url, verify_signature,
    PortableManifest,
};

/// The minisign public key that signs releases — the SAME key as the NSIS updater
/// (`plugins.updater.pubkey` in `tauri.conf.json`). A test asserts this stays in lock-step with the
/// config, so the two updaters can never drift onto different trust roots.
const UPDATER_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEI2Q0ZERjVDRTFBRDI5MgpSV1NTMGhyTzlmMXNDeE1FR1k2Y1BXTXc4RlQvZ0U1TVN6QmlwT28zL1dwSU8rK3B6ZXlxNU5TYgo=";

/// The portable-update manifest, published beside the stable `Hoardbook.exe` on the latest release.
/// `releases/latest/download/…` is a stable URL that always resolves to the newest published release.
const PORTABLE_MANIFEST_URL: &str =
    "https://github.com/Arsinine/Hoardbook/releases/latest/download/portable.json";

/// A self-identifying User-Agent (GitHub is friendlier to requests that set one).
const USER_AGENT: &str = concat!("Hoardbook/", env!("CARGO_PKG_VERSION"), " (portable-updater)");

/// Per-request network timeout — bounds a slow-loris / hung endpoint (security review #2).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Size cap for the manifest JSON — a tiny document; anything larger is hostile (security review #2).
const MAX_MANIFEST_BYTES: u64 = 1 << 20; // 1 MiB
/// Size cap for the downloaded binary — bounds a memory-exhaustion DoS (security review #2).
const MAX_BINARY_BYTES: u64 = 512 << 20; // 512 MiB

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent(USER_AGENT).timeout(REQUEST_TIMEOUT).build().map_err(cmd_err)
}

/// GET `url` with a hard size cap: a declared `Content-Length` over `max` is refused before the body
/// is read, and the buffered body is re-checked as a backstop. HTTPS/host trust of `url` is the
/// caller's responsibility ([`is_trusted_artifact_url`]).
async fn fetch_capped(client: &reqwest::Client, url: &str, max: u64) -> Result<Vec<u8>, String> {
    let resp = client.get(url).send().await.map_err(cmd_err)?.error_for_status().map_err(cmd_err)?;
    if let Some(len) = resp.content_length() {
        if len > max {
            return Err(format!("update download too large ({len} bytes; cap {max})"));
        }
    }
    let bytes = resp.bytes().await.map_err(cmd_err)?;
    if bytes.len() as u64 > max {
        return Err(format!("update download exceeded the {max}-byte cap"));
    }
    Ok(bytes.to_vec())
}

#[derive(Serialize)]
pub struct PortableUpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

/// Whether this running build is the portable (loose-exe) build — the frontend uses it to route to the
/// portable updater vs the NSIS/Tauri one. **Windows only:** macOS (`.dmg`) and Linux (AppImage) keep
/// the Tauri updater, which already updates them in place, so this is always `false` off Windows.
/// On Windows it's best-effort: an `uninstall.exe` beside the running exe marks an NSIS install; its
/// absence marks portable.
#[tauri::command]
pub fn updater_is_portable() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let has_uninstaller = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("uninstall.exe").exists()))
        .unwrap_or(false);
    is_portable_build(has_uninstaller)
}

/// Fetch + parse the portable manifest (size-capped). Any network / non-2xx / parse failure is
/// surfaced verbatim.
async fn fetch_manifest() -> Result<PortableManifest, String> {
    let client = http_client()?;
    let bytes = fetch_capped(&client, PORTABLE_MANIFEST_URL, MAX_MANIFEST_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("bad portable manifest: {e}"))
}

/// Check for a newer portable release. Returns the new version + notes, or `None` if up to date.
#[tauri::command]
pub async fn check_portable_update(app: tauri::AppHandle) -> CmdResult<Option<PortableUpdateInfo>> {
    let current = app.package_info().version.to_string();
    let manifest = fetch_manifest().await.map_err(cmd_err)?;
    if is_newer(&current, &manifest.version) {
        Ok(Some(PortableUpdateInfo { version: manifest.version, notes: manifest.notes }))
    } else {
        Ok(None)
    }
}

/// Download the newer portable binary, verify its signature under [`UPDATER_PUBKEY`], swap the running
/// exe in place, and relaunch. Refuses if there is no newer version, no artifact for this platform, or
/// the signature fails — the self-replace happens ONLY after a good signature.
#[tauri::command]
pub async fn apply_portable_update(app: tauri::AppHandle) -> CmdResult<()> {
    let current = app.package_info().version.to_string();
    let manifest = fetch_manifest().await.map_err(cmd_err)?;
    if !is_newer(&current, &manifest.version) {
        return Err("No newer portable version is available.".into());
    }
    let target = current_target_key();
    let artifact = manifest
        .artifact_for(&target)
        .ok_or_else(|| format!("This platform ({target}) has no portable build to update to."))?;
    // Defense-in-depth (security review #3): only pull a binary from an https GitHub release host,
    // even before the signature is checked — closes the "tampered manifest redirects the download"
    // primitive.
    if !is_trusted_artifact_url(&artifact.url) {
        return Err(format!("refusing to download the update from an untrusted URL: {}", artifact.url));
    }

    let client = http_client()?;
    let bytes = fetch_capped(&client, &artifact.url, MAX_BINARY_BYTES).await?;

    // Verify BEFORE anything touches the running exe — same trust root as the NSIS updater.
    verify_signature(&bytes, &artifact.signature, UPDATER_PUBKEY).map_err(cmd_err)?;

    // Stage the verified bytes next to the current exe (same volume), then swap in place. `self_replace`
    // handles the Windows running-exe lock (rename-self dance); the in-memory process keeps running
    // until we relaunch it below.
    let exe = std::env::current_exe().map_err(cmd_err)?;
    let dir = exe.parent().ok_or_else(|| "cannot resolve the running exe's directory".to_string())?;
    let staged = tempfile::Builder::new()
        .prefix(".hoardbook-update-")
        .tempfile_in(dir)
        .map_err(cmd_err)?;
    std::fs::write(staged.path(), &bytes).map_err(cmd_err)?;
    self_replace::self_replace(staged.path()).map_err(cmd_err)?;
    drop(staged); // self_replace copied it into place; remove the leftover temp.

    // Relaunch. `app.restart()` re-execs `current_exe()` — which `self_replace` leaves pointing at the
    // now-updated binary at its original path. **Windows-validation boundary (untestable offline):**
    // this shares the single-instance guard with the NSIS `apply_staged_update`; if the re-exec races
    // the single-instance lock (a fresh process signalling the still-exiting one and bailing), the swap
    // has still succeeded on disk — reopening Hoardbook runs the new version. Verify the auto-relaunch
    // on a real signed release; if it proves flaky, hand off to a detached relauncher that waits for
    // this process to exit before starting the new exe.
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::UPDATER_PUBKEY;

    #[test]
    fn embedded_pubkey_matches_tauri_conf() {
        // The portable updater MUST verify against the same key the NSIS updater (and CI signing) use.
        // Read the checked-in tauri.conf.json and assert the embedded const is byte-for-byte identical.
        let conf = include_str!("../../tauri.conf.json");
        let cfg: serde_json::Value = serde_json::from_str(conf).expect("tauri.conf.json parses");
        let pubkey = cfg["plugins"]["updater"]["pubkey"]
            .as_str()
            .expect("plugins.updater.pubkey is a string");
        assert_eq!(
            UPDATER_PUBKEY, pubkey,
            "portable updater pubkey drifted from tauri.conf.json — they MUST stay in sync"
        );
    }
}
