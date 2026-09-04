//! Portable self-updater commands (devtest v0.12.1 follow-up) — the update path for the loose,
//! installer-less `Hoardbook.exe` (the build people actually run). The NSIS updater
//! (`commands::update`) stays untouched as the **regression path** for installed builds; the app
//! routes to whichever matches how it was launched ([`updater_is_portable`]).
//!
//! Flow: [`check_portable_update`] reads the signed `portable.json` manifest and reports a newer
//! version; [`apply_portable_update`] downloads the stable `Hoardbook.exe`, **verifies its minisign
//! signature under the SAME key as the NSIS updater** (`portable_update_logic::verify_signature`), then
//! swaps the running exe in place via `self_replace` and relaunches. **No unsigned binary is ever
//! written over the running exe** — verification happens before anything touches it. The pure logic
//! and the signature check are unit-tested in `portable_update_logic`; the guards' ORDER at the
//! site is unit-tested here via [`apply_portable_update_inner`] (fetch injected). The replace/relaunch
//! tail is the I/O boundary (not offline-testable).

use std::time::Duration;

use serde::Serialize;

use crate::error::{cmd_err, CmdResult};
use crate::portable_update_logic::{
    binary_matches_claimed_version, current_target_key, is_newer, is_portable_build,
    is_trusted_artifact_url, verify_signature, PortableManifest,
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

/// Security review #27 (transport half): the per-hop redirect decision used by [`http_client`]'s
/// redirect policy. A redirect may be followed ONLY when the hop's full URL passes the same GitHub
/// https allowlist as the initial download URL ([`is_trusted_artifact_url`]) — reqwest's default
/// policy follows up to 10 hops to ANY host, so a trusted URL that 30x's to a foreign host (or to
/// plain http) would otherwise deliver attacker bytes down the channel that updates every install.
/// Pure so the decision is unit-testable; the closure in [`http_client`] is its only caller.
///
/// NOTE (reported, deliberately not changed here): a `custom` policy does NOT inherit reqwest's
/// default 10-hop cap — this decision is per-hop and hop-count-independent, so a chain that stays
/// on allowlisted hosts is followed without bound (bounded only by [`REQUEST_TIMEOUT`]). Adding a
/// hop budget would be a policy change, out of scope for the lane that pins the current policy.
fn redirect_hop_allowed(next_url: &str) -> bool {
    is_trusted_artifact_url(next_url)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        // Security review #27: the default redirect policy follows up to 10 hops with no re-validation.
        // Re-check every hop's host against the GitHub allowlist, so a trusted URL that 30x's to a
        // foreign host (or to plain http) is refused before any bytes are fetched.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if redirect_hop_allowed(attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.error("redirect to an untrusted host")
            }
        }))
        .build()
        .map_err(cmd_err)
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
/// the signature fails — the self-replace happens ONLY after a good signature. The guards are the
/// seam-extracted [`apply_portable_update_inner`]; this shim owns only the manifest fetch and the
/// replace/relaunch tail.
#[tauri::command]
pub async fn apply_portable_update(app: tauri::AppHandle) -> CmdResult<()> {
    let current = app.package_info().version.to_string();
    let manifest = fetch_manifest().await.map_err(cmd_err)?;
    let bytes = apply_portable_update_inner(&current, &manifest, |url| async move {
        let client = http_client()?;
        fetch_capped(&client, &url, MAX_BINARY_BYTES).await
    })
    .await?;
    install_verified_bytes(&app, &bytes)
}

/// The guarded half of [`apply_portable_update`] — every security refusal in the command, in their
/// REQUIRED ORDER, with the artifact download injected so a test can observe whether (and when) any
/// bytes are actually fetched. Extracted per the seam the file's own OWED comment (2026-08-30,
/// QURATOR-161 slice 2) prescribed: "extract an `*_inner` taking the URL/manifest" — the ordering
/// pins are otherwise undriveable: the guards sit downstream of `fetch_manifest()`, which is
/// hard-wired to [`PORTABLE_MANIFEST_URL`]. The extraction is behavior-preserving — every guard
/// condition and refusal message is unchanged from the ones that stood inline in the command; the
/// only differences are the injected `fetch` and `current` arriving as a borrowed `&str`. `fetch` is the artifact
/// download (production: [`fetch_capped`] under the [`MAX_BINARY_BYTES`] cap); it is the ONLY I/O in
/// this half, so "a guard fired before the fetch" is observable here.
///
/// Guard ORDER, which the `mod guard_order` tests pin:
/// 1. `!is_newer(current, manifest.version)` — refuse before ANYTHING else happens.
/// 2. `!is_trusted_artifact_url(artifact.url)` — refuse before any artifact bytes are fetched.
/// 3. `verify_signature(bytes, artifact.signature, UPDATER_PUBKEY)` — verify before the
///    embedded-version binding is consulted.
/// 4. `!binary_matches_claimed_version(bytes, manifest.version)` — bind before anything touches the
///    running exe.
pub(crate) async fn apply_portable_update_inner<F, Fut>(
    current: &str,
    manifest: &PortableManifest,
    fetch: F,
) -> Result<Vec<u8>, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>> + Send,
{
    if !is_newer(current, &manifest.version) {
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

    let bytes = fetch(artifact.url.clone()).await?;

    // Verify BEFORE anything touches the running exe — same trust root as the NSIS updater.
    verify_signature(&bytes, &artifact.signature, UPDATER_PUBKEY).map_err(cmd_err)?;

    // Security review #34 (TUF rollback): the signature binds the bytes, but nothing yet binds the
    // bytes' *own* embedded version to the manifest's claimed version. A manifest that lies about the
    // version while pointing at a genuinely-signed old release would silently downgrade this client.
    // The bytes are already authenticated here, so their embedded version is trustworthy — refuse if it
    // disagrees with the claim, before anything touches the running exe.
    if !binary_matches_claimed_version(&bytes, &manifest.version) {
        return Err(format!(
            "the downloaded binary reports a version other than {} — refusing to install a possible downgrade",
            manifest.version
        ));
    }

    Ok(bytes)
}

/// The tail of [`apply_portable_update`] — stage the verified bytes next to the current exe (same
/// volume), swap in place, and relaunch. This is the I/O boundary (not offline-testable) and is
/// reached ONLY with bytes that have cleared every guard in [`apply_portable_update_inner`].
fn install_verified_bytes(app: &tauri::AppHandle, bytes: &[u8]) -> CmdResult<()> {
    // `self_replace` handles the Windows running-exe lock (rename-self dance); the in-memory process
    // keeps running until we relaunch it below.
    let exe = std::env::current_exe().map_err(cmd_err)?;
    let dir = exe.parent().ok_or_else(|| "cannot resolve the running exe's directory".to_string())?;
    let staged = tempfile::Builder::new()
        .prefix(".hoardbook-update-")
        .tempfile_in(dir)
        .map_err(cmd_err)?;
    std::fs::write(staged.path(), bytes).map_err(cmd_err)?;
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
    use super::redirect_hop_allowed;

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

    #[test]
    fn redirect_policy_allows_a_hop_to_a_trusted_github_host() {
        // A redirect that stays on the https GitHub release hosts (e.g. github.com's
        // `releases/latest/download` → the tag's `releases/download/...`, or a hop to the
        // objects/release-assets CDN) must still be followed — pinning that the #27 fix did not
        // brick the legitimate GitHub redirect chain the updater depends on.
        assert!(redirect_hop_allowed(
            "https://github.com/Arsinine/Hoardbook/releases/download/v0.16.0/Hoardbook.exe"
        ));
        assert!(redirect_hop_allowed("https://objects.githubusercontent.com/abc"));
        assert!(redirect_hop_allowed("https://release-assets.githubusercontent.com/abc"));
    }

    #[test]
    fn redirect_policy_refuses_a_hop_to_a_foreign_host_or_downgrade() {
        // Security review #27 (transport half): every hop is re-checked with the SAME WHATWG
        // allowlist as the initial URL — a trusted origin that 30x's to a foreign host, a
        // lookalike, a userinfo trick, a backslash differential, or a plain-http downgrade is
        // refused before reqwest connects to it.
        assert!(!redirect_hop_allowed("https://evil.com/Hoardbook.exe"), "foreign host");
        assert!(!redirect_hop_allowed("http://github.com/x"), "http downgrade");
        assert!(!redirect_hop_allowed("https://github.com.evil.com/x"), "lookalike host");
        assert!(!redirect_hop_allowed("https://github.com@evil.com/x"), "userinfo trick");
        assert!(!redirect_hop_allowed("https://evil.com\\@github.com/x"), "backslash differential");
        assert!(!redirect_hop_allowed("ftp://github.com/x"), "non-https scheme");
        assert!(!redirect_hop_allowed("not a url"), "garbage");
    }

    /// The transport-level pin for security review #27: `http_client`'s redirect policy must
    /// refuse a cross-host hop BEFORE any bytes are fetched from the redirect target. Drives a
    /// real `reqwest::Client` (the one production uses, via [`super::http_client`]) against two
    /// loopback stubs: "origin" answers 302 pointing at "attacker"; the test fails if the client
    /// ever contacts the attacker or returns its bytes. The initial URL is loopback purely as a
    /// test vehicle — production checks the initial URL at the call site
    /// ([`super::is_trusted_artifact_url`]); the redirect policy's job is the hops.
    ///
    /// Mutation guard: with the `.redirect(Policy::custom(…))` block removed, reqwest's DEFAULT
    /// policy follows the 302 to the attacker, its marker body comes back, and this test reds.
    #[tokio::test]
    async fn http_client_refuses_a_cross_host_redirect_before_any_foreign_bytes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // The "attacker": any host outside the GitHub allowlist. Records whether it is EVER contacted.
        let attacker = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind attacker");
        let attacker_addr = attacker.local_addr().expect("attacker addr");
        let attacker_hit = Arc::new(AtomicBool::new(false));
        let hit_flag = Arc::clone(&attacker_hit);
        let attacker_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = attacker.accept().await {
                hit_flag.store(true, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await; // drain the request line/headers
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nATTACKER BYTES",
                    )
                    .await;
            }
        });

        // The "origin": plays the compromised-but-initially-trusted endpoint that 30x's away.
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
        let origin_addr = origin.local_addr().expect("origin addr");
        let origin_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = origin.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await; // drain the request line/headers
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{attacker_addr}/Hoardbook.exe\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });

        let client = super::http_client().expect("client builds");
        let send = client.get(format!("http://{origin_addr}/portable.json")).send().await;

        // The request must NOT come back carrying attacker bytes: either it errors at the hop
        // (the policy's `attempt.error`), or — if it somehow "succeeds" — the body must not be
        // the marker. Both branches assert; under the default (unfixed) policy the send resolves
        // Ok with the marker body, so the second assert reds even if the first somehow passed.
        if let Ok(resp) = send {
            // If the send "succeeded", it must not be carrying the attacker's body. (An Err is
            // the expected outcome: refused at the hop by the policy's `attempt.error`.)
            let body = resp.text().await.unwrap_or_default();
            assert!(
                !body.contains("ATTACKER BYTES"),
                "the redirect was followed and foreign bytes were fetched"
            );
        }

        let _ = origin_task.await;
        // The attacker listener is never accepted-from under the fix, so awaiting it would hang;
        // bound it, then check it was never contacted.
        let _ = tokio::time::timeout(Duration::from_secs(1), attacker_task).await;
        assert!(
            !attacker_hit.load(Ordering::SeqCst),
            "the client contacted the untrusted redirect target — the redirect policy is absent or not refusing cross-host hops"
        );
    }

    // ── QURATOR-181 item 1 — the guards' ORDER at the call site ────────────────────────────────
    //
    // The comment that stood here (QURATOR-161 slice 2, 2026-08-30) said the `!is_newer` and
    // `is_trusted_artifact_url` PLACEMENT was unpinned because both sit downstream of
    // `fetch_manifest()`, hard-wired to `PORTABLE_MANIFEST_URL`, and named the fix: "extract an
    // `*_inner` taking the URL/manifest". That seam now exists — `apply_portable_update_inner`, guard
    // conditions and messages unchanged from the ones that stood inline, with the artifact fetch injected so a test
    // can observe whether (and when) any bytes were fetched. The pure halves stay pinned where they
    // were (`is_newer`, `is_trusted_artifact_url` in `portable_update_logic`; `redirect_hop_allowed`
    // above); what this block adds is the ORDERING.
    //
    // Method: every input below is built so that TWO guards would both fire, and the test asserts
    // WHICH one wins — a reorder changes the winner, a removal changes the winner or lets the fetch
    // spy run. A test that only checked "it returns an error" would pass in any order; these do not.
    // The fetch spy's log is the "before any bytes were fetched" witness.
    mod guard_order {
        use super::super::apply_portable_update_inner;
        use crate::portable_update_logic::{current_target_key, PortableArtifact, PortableManifest};
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        fn manifest(version: &str, url: &str) -> PortableManifest {
            PortableManifest {
                version: version.to_string(),
                notes: None,
                platforms: HashMap::from([(
                    current_target_key(),
                    PortableArtifact { url: url.to_string(), signature: String::new() },
                )]),
            }
        }

        /// ORDER 1 — a non-newer version is refused before ANYTHING else: before the URL trust
        /// check, and before any artifact bytes are fetched. The manifest carries BOTH defects
        /// (version 0.0.1 vs current 9.9.9, and an untrusted artifact URL) — with the guards in
        /// order, the `is_newer` refusal must win; with them swapped, the URL refusal would.
        ///
        /// P-10 mutation: in `apply_portable_update_inner`, move the `if !is_newer(current,
        /// &manifest.version) { … }` block to just AFTER the `if !is_trusted_artifact_url(&artifact.url)
        /// { … }` block — this test reds on the message assert; moving the fetch line above the
        /// `is_newer` block instead reds the log assert.
        #[tokio::test]
        async fn non_newer_refuses_before_the_url_trust_check_and_any_fetch() {
            let m = manifest("0.0.1", "https://evil.com/Hoardbook.exe");
            let fetched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&fetched);
            let err = apply_portable_update_inner("9.9.9", &m, move |url| async move {
                seen.lock().unwrap().push(url);
                Ok(Vec::new())
            })
            .await
            .unwrap_err();
            assert_eq!(err, "No newer portable version is available.");
            assert!(
                fetched.lock().unwrap().is_empty(),
                "a non-newer manifest must be refused before any artifact bytes are fetched"
            );
        }

        /// ORDER 2 — an untrusted URL is refused BEFORE the download: the fetch spy must never run.
        /// Download-first-check-after is exactly the security-review-#3 primitive ("tampered manifest
        /// redirects the download") the guard exists to close, so the empty fetch log — not the error
        /// message alone — is the load-bearing assert here.
        ///
        /// P-10 mutation: in `apply_portable_update_inner`, move the `let bytes =
        /// fetch(artifact.url.clone()).await?;` line to just ABOVE the `if
        /// !is_trusted_artifact_url(&artifact.url) { … }` block — this test reds on the log assert
        /// (the spy recorded a fetch of the untrusted URL).
        #[tokio::test]
        async fn untrusted_url_is_refused_before_any_bytes_are_fetched() {
            let m = manifest("99.0.0", "https://github.com.evil.com/Hoardbook.exe");
            let fetched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&fetched);
            let err = apply_portable_update_inner("9.9.9", &m, move |url| async move {
                seen.lock().unwrap().push(url);
                Ok(b"attacker bytes".to_vec())
            })
            .await
            .unwrap_err();
            assert!(
                err.starts_with("refusing to download the update from an untrusted URL"),
                "the URL-trust guard must own this refusal, got {err}"
            );
            assert!(
                fetched.lock().unwrap().is_empty(),
                "bytes were fetched from an untrusted URL before the guard fired"
            );
        }

        /// ORDER 3 — the signature verifies BEFORE the embedded-version binding is consulted. The
        /// fetched bytes are junk (no VS_VERSION_INFO resource) with no valid signature, so BOTH
        /// post-fetch guards would refuse; which one's message surfaces is the order witness. If
        /// `binary_matches_claimed_version` ran first, this input would surface the downgrade error
        /// instead of the signature error.
        ///
        /// P-10 mutation: in `apply_portable_update_inner`, swap the `verify_signature(&bytes,
        /// &artifact.signature, UPDATER_PUBKEY).map_err(cmd_err)?;` line with the whole `if
        /// !binary_matches_claimed_version(&bytes, &manifest.version) { … }` block — this test reds
        /// (the error no longer names the signature; it becomes "the downloaded binary reports a
        /// version other than 99.0.0 …").
        ///
        /// Honest limit: the green side of guard 4 (validly-SIGNED bytes whose embedded version
        /// mismatches the claim) is not driveable here — `UPDATER_PUBKEY` is pinned inside the seam,
        /// and signing under it requires the release private key. What IS pinned is that guard 3
        /// precedes guard 4 for every input a test can construct.
        #[tokio::test]
        async fn signature_verifies_before_the_embedded_version_binding() {
            let m = manifest(
                "99.0.0",
                "https://github.com/Arsinine/Hoardbook/releases/download/v99.0.0/Hoardbook.exe",
            );
            let fetched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&fetched);
            let err = apply_portable_update_inner("9.9.9", &m, move |url| async move {
                seen.lock().unwrap().push(url);
                Ok(b"these bytes are not an exe and carry no version resource".to_vec())
            })
            .await
            .unwrap_err();
            assert!(
                err.contains("signature"),
                "the signature guard must fire before the version binding is consulted, got {err}"
            );
            assert_eq!(
                fetched.lock().unwrap().len(),
                1,
                "a trusted URL on a newer manifest IS downloaded — only the guards' order is under test here"
            );
        }
    }
}
