//! Pure, CI-testable half of the **portable self-updater** (devtest v0.12.1 follow-up).
//!
//! The Windows *portable* build is a loose `Hoardbook.exe` with no installer, so the NSIS updater
//! can't update it — NSIS always installs to its own managed location (`%LOCALAPPDATA%\Hoardbook`),
//! never "the loose exe you happen to run". This module holds the decision + verification logic
//! (manifest parsing, semver gating, portable-vs-installed detection, and the minisign signature
//! check) that the I/O command layer (`commands::portable_update`) drives. The actual download →
//! self-replace → relaunch is the **I/O boundary** and is not exercised here (mirrors [`update_logic`]).
//!
//! **Trust:** [`verify_signature`] is byte-identical to `tauri-plugin-updater`'s own `verify_signature`
//! — the same `minisign-verify` crate, the same key, the same base64-wrapped format — so a `.sig`
//! produced by `tauri signer sign` in CI verifies here with no divergence, and the portable path
//! shares the NSIS updater's exact trust root. There is never an unsigned self-replace.

use base64::Engine;
use serde::Deserialize;

/// One platform's portable artifact in the manifest: where to download it + its Tauri-format minisign
/// signature (base64 of the `.sig` file, exactly as `latest.json` carries a signature).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PortableArtifact {
    pub url: String,
    pub signature: String,
}

/// `portable.json` — the portable-update manifest published beside the stable `Hoardbook.exe` on the
/// GitHub release. Mirrors `latest.json` in spirit: a version + per-target signed artifacts.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PortableManifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    pub platforms: std::collections::HashMap<String, PortableArtifact>,
}

impl PortableManifest {
    /// The artifact for a target key (e.g. `"windows-x86_64"`), or `None` if this manifest carries no
    /// build for that platform.
    pub fn artifact_for(&self, target: &str) -> Option<&PortableArtifact> {
        self.platforms.get(target)
    }
}

/// The manifest-target key for an `(os, arch)` pair — the same `"<os>-<arch>"` shape CI writes.
pub fn target_key_for(os: &str, arch: &str) -> String {
    format!("{os}-{arch}")
}

/// The manifest-target key for the current build.
pub fn current_target_key() -> String {
    target_key_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Is `candidate` strictly newer than `current`? Both are parsed as semver; a parse failure on either
/// side is treated as **not newer** — fail closed, so a garbled manifest can never trigger an update.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (semver::Version::parse(current), semver::Version::parse(candidate)) {
        (Ok(cur), Ok(cand)) => cand > cur,
        _ => false,
    }
}

/// Whether an artifact download URL is one we trust to pull a binary from: it must be `https://` and
/// on a GitHub release host. Defense-in-depth *beside* the signature check (security review #3) — it
/// removes the "a tampered manifest redirects the download to a foreign host / plain http" primitive
/// even before verification, and defeats userinfo/lookalike-host tricks.
pub fn is_trusted_artifact_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority); // drop any userinfo
    let host = host.split(':').next().unwrap_or(host); // drop any port
    matches!(
        host,
        "github.com" | "objects.githubusercontent.com" | "release-assets.githubusercontent.com"
    )
}

/// Is THIS build a *portable* (loose-exe) install rather than an NSIS-installed one? The Tauri NSIS
/// installer drops an `uninstall.exe` beside the app; a portable exe has none. The caller passes
/// whether that sibling exists, so the branch is unit-tested without touching the filesystem.
pub fn is_portable_build(has_nsis_uninstaller_sibling: bool) -> bool {
    !has_nsis_uninstaller_sibling
}

/// Verify `data` against a Tauri-format base64 `signature` under a Tauri-format base64 `pubkey` —
/// **byte-identical to `tauri-plugin-updater::verify_signature`** (same crate, same key, same format).
/// `Ok(())` iff the signature is valid; `Err` on any decode failure or signature mismatch. A portable
/// update is applied ONLY when this returns `Ok`.
pub fn verify_signature(data: &[u8], signature: &str, pubkey: &str) -> Result<(), String> {
    let pubkey_text = base64_to_string(pubkey).map_err(|e| format!("bad updater public key: {e}"))?;
    let public_key = minisign_verify::PublicKey::decode(&pubkey_text)
        .map_err(|e| format!("bad updater public key: {e}"))?;
    let sig_text = base64_to_string(signature).map_err(|e| format!("bad update signature: {e}"))?;
    let sig = minisign_verify::Signature::decode(&sig_text)
        .map_err(|e| format!("bad update signature: {e}"))?;
    public_key
        .verify(data, &sig, true)
        .map_err(|_| "update signature verification failed".to_string())
}

/// base64-decode a string and interpret the bytes as UTF-8 (the minisign key / sig file text). Mirrors
/// the plugin's `base64_to_string`.
fn base64_to_string(b64: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    // The `minisign-verify` crate's own published test vector — a public-key file + a signature file
    // over the message b"test" — re-wrapped into Tauri's base64 form (base64 of the whole file text).
    // Proves our wrapper agrees with the reference verifier bit-for-bit, offline, with no private key.
    const PUBKEY_FILE: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIG_FILE: &str = "untrusted comment: signature from minisign secret key\nRWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\ntrusted comment: timestamp:1555779966\tfile:test\nQtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

    #[test]
    fn verify_accepts_a_valid_signature_and_rejects_tampered_data() {
        let pk = b64(PUBKEY_FILE);
        let sig = b64(SIG_FILE);
        assert!(verify_signature(b"test", &sig, &pk).is_ok(), "the valid vector must verify");
        assert!(verify_signature(b"Test", &sig, &pk).is_err(), "one flipped byte must fail");
    }

    #[test]
    fn verify_rejects_malformed_inputs() {
        assert!(verify_signature(b"x", "not base64 !!", "also not base64 !!").is_err());
        let junk = base64::engine::general_purpose::STANDARD.encode("nonsense");
        assert!(verify_signature(b"x", &junk, &junk).is_err(), "valid base64 but not a key/sig");
    }

    #[test]
    fn is_newer_is_strict_semver_and_fails_closed() {
        assert!(is_newer("0.12.1", "0.12.2"));
        assert!(!is_newer("0.12.1", "0.12.1"), "equal is not newer");
        assert!(!is_newer("0.12.2", "0.12.1"), "a downgrade is not newer");
        assert!(!is_newer("garbage", "0.12.2"), "unparseable current fails closed");
        assert!(!is_newer("0.12.1", "garbage"), "unparseable candidate fails closed");
    }

    #[test]
    fn trusted_artifact_url_requires_https_and_a_github_host() {
        assert!(is_trusted_artifact_url(
            "https://github.com/Arsinine/Hoardbook/releases/latest/download/Hoardbook.exe"
        ));
        assert!(is_trusted_artifact_url("https://objects.githubusercontent.com/abc"));
        assert!(!is_trusted_artifact_url("http://github.com/x"), "plain http rejected");
        assert!(!is_trusted_artifact_url("https://evil.com/Hoardbook.exe"), "foreign host rejected");
        assert!(!is_trusted_artifact_url("https://github.com.evil.com/x"), "lookalike host rejected");
        assert!(!is_trusted_artifact_url("https://github.com@evil.com/x"), "userinfo trick rejected");
        assert!(!is_trusted_artifact_url("ftp://github.com/x"), "non-https scheme rejected");
    }

    #[test]
    fn portable_detection_keys_off_the_nsis_uninstaller() {
        assert!(is_portable_build(false), "no uninstaller sibling ⇒ portable");
        assert!(!is_portable_build(true), "an uninstall.exe beside the app ⇒ NSIS-installed");
    }

    #[test]
    fn manifest_parses_and_selects_the_target_artifact() {
        let json = r#"{
            "version": "0.12.2",
            "notes": "what's new",
            "platforms": { "windows-x86_64": { "url": "https://x/Hoardbook.exe", "signature": "sig" } }
        }"#;
        let m: PortableManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.version, "0.12.2");
        assert_eq!(m.artifact_for("windows-x86_64").unwrap().url, "https://x/Hoardbook.exe");
        assert!(m.artifact_for("linux-x86_64").is_none(), "a platform we didn't publish is absent");
    }

    #[test]
    fn target_key_is_os_dash_arch() {
        assert_eq!(target_key_for("windows", "x86_64"), "windows-x86_64");
    }
}
