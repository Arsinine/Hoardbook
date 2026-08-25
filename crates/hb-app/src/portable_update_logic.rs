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
///
/// Parsing is WHATWG-compliant (via `reqwest::Url`, a re-export of the `url` crate) rather than
/// hand-rolled string-splitting. Naive splitting diverges from WHATWG on a backslash-before-`@`
/// authority terminator: `https://evil.com\@github.com/x` *looks* like host `github.com` to a
/// string splitter but WHATWG (and reqwest itself) connect to `evil.com` (security review #27).
pub fn is_trusted_artifact_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    matches!(
        parsed.host_str(),
        Some("github.com")
            | Some("objects.githubusercontent.com")
            | Some("release-assets.githubusercontent.com")
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

/// Extract the `major.minor.patch` version embedded in a Windows PE's `VS_VERSION_INFO` resource,
/// which `tauri-build` writes into every Windows binary (`FILEVERSION`/`PRODUCTVERSION` from
/// `tauri.conf.json` — the same version the release manifest claims). This is the monotonicity half
/// of the secure-update framework (security review #34): the minisign signature binds the bytes, and
/// this binds the bytes' *own* version to the manifest's claimed version, so a manifest that lies
/// about the version while pointing at a genuinely-signed old release is rejected.
///
/// The version is read from the fixed `VS_FIXEDFILEINFO` block (`dwFileVersionMS` = `(major<<16)|minor`,
/// `dwFileVersionLS` = `(patch<<16)|build`), located by scanning for the resource's fixed `szKey`
/// `"VS_VERSION_INFO"` (UTF-16LE) and validating `dwSignature == 0xFEEF_04BD`. Returns `None` when no
/// well-formed version resource is found — callers fail closed.
pub fn portable_exe_version(data: &[u8]) -> Option<String> {
    let needle = utf16le("VS_VERSION_INFO\0");
    let mut version: Option<(u16, u16, u16)> = None;
    for (i, w) in data.windows(needle.len()).enumerate() {
        if w != needle.as_slice() {
            continue;
        }
        // `i` points at the `szKey`; the enclosing VS_VERSIONINFO starts 6 bytes earlier, and its
        // VS_FIXEDFILEINFO begins at offset 40 (2 wLength + 2 wValueLength + 2 wType + 32 szKey +
        // 2 pad). Fields: dwSignature @40, dwStrucVersion @44, dwFileVersionMS @48, dwFileVersionLS @52.
        let Some(base) = i.checked_sub(6) else { continue };
        let (Some(sig), Some(struc), Some(fvms), Some(fvls)) = (
            read_u32_le(data, base + 40),
            read_u32_le(data, base + 44),
            read_u32_le(data, base + 48),
            read_u32_le(data, base + 52),
        ) else {
            continue;
        };
        if sig != 0xFEEF_04BD || struc != 0x0001_0000 {
            continue; // a lookalike marker without a valid fixed block — keep scanning
        }
        let candidate = ((fvms >> 16) as u16, (fvms & 0xFFFF) as u16, (fvls >> 16) as u16);
        match version {
            None => version = Some(candidate),
            Some(prev) if prev != candidate => return None, // two resources disagree — refuse
            Some(_) => {}
        }
    }
    version.map(|(major, minor, patch)| format!("{major}.{minor}.{patch}"))
}

/// Does the (already signature-verified) binary's own embedded version equal the version the manifest
/// claims? Fails closed — an unreadable, unparseable, or mismatched version is `false`, so the caller
/// refuses to install. Closes the TUF-style rollback (security review #34): a manifest claiming a fake
/// high version while pointing at a genuinely-signed *old* release is rejected because the old binary
/// reports its real, lower version.
pub fn binary_matches_claimed_version(data: &[u8], claimed: &str) -> bool {
    let Some(embedded) = portable_exe_version(data) else {
        return false;
    };
    match (semver::Version::parse(&embedded), semver::Version::parse(claimed)) {
        (Ok(e), Ok(c)) => e == c,
        _ => false,
    }
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn read_u32_le(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
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
    fn trusted_artifact_url_rejects_backslash_userinfo_differential() {
        // The exact parser-differential from security review #27: a naive splitter sees the host as
        // `github.com` (the segment after the `@`), but WHATWG treats `\` as a path separator, so the
        // real host is `evil.com`. reqwest follows WHATWG, so the allowlist must agree with WHATWG.
        assert!(
            !is_trusted_artifact_url("https://evil.com\\@github.com/x"),
            "backslash-before-@ must NOT be accepted as github.com"
        );
        assert!(!is_trusted_artifact_url("https://evil.com\\@release-assets.githubusercontent.com/x"));
        assert!(!is_trusted_artifact_url("https://evil.com\\@objects.githubusercontent.com/x"));
    }

    /// A minimal, spec-faithful VS_VERSIONINFO blob: a root header plus the fixed `VS_FIXEDFILEINFO`
    /// block whose `dwFileVersionMS`/`LS` carry `(major,minor)`/`(patch,0)`. Mirrors the layout
    /// `tauri-build`/`tauri-winres` emit (`FILEVERSION major, minor, patch, 0`).
    fn fake_vs_versioninfo(major: u16, minor: u16, patch: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&92u16.to_le_bytes()); // wLength (40-byte header + 52-byte fixed info)
        v.extend_from_slice(&52u16.to_le_bytes()); // wValueLength (sizeof VS_FIXEDFILEINFO)
        v.extend_from_slice(&0u16.to_le_bytes()); // wType (binary)
        v.extend_from_slice(&utf16le("VS_VERSION_INFO\0")); // szKey (16 WCHARs)
        v.extend_from_slice(&[0, 0]); // padding to DWORD alignment
        v.extend_from_slice(&0xFEEF_04BDu32.to_le_bytes()); // dwSignature
        v.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // dwStrucVersion
        v.extend_from_slice(&((u32::from(major) << 16) | u32::from(minor)).to_le_bytes()); // dwFileVersionMS
        v.extend_from_slice(&(u32::from(patch) << 16).to_le_bytes()); // dwFileVersionLS (build=0)
        v.extend_from_slice(&((u32::from(major) << 16) | u32::from(minor)).to_le_bytes()); // dwProductVersionMS
        v.extend_from_slice(&(u32::from(patch) << 16).to_le_bytes()); // dwProductVersionLS
        for _ in 0..7 {
            v.extend_from_slice(&0u32.to_le_bytes()); // dwFileFlagsMask .. dwFileDateLS
        }
        assert_eq!(v.len(), 92);
        v
    }

    #[test]
    fn embedded_version_binds_to_the_manifest_claim() {
        let exe = fake_vs_versioninfo(0, 16, 0);
        assert_eq!(portable_exe_version(&exe).as_deref(), Some("0.16.0"));
        assert!(binary_matches_claimed_version(&exe, "0.16.0"), "matching version binds");
        assert!(
            !binary_matches_claimed_version(&exe, "9.9.9"),
            "a fake high version pointing at an old binary must be rejected (rollback)"
        );
        assert!(!binary_matches_claimed_version(&exe, "0.15.0"), "a lower real version is a downgrade");
    }

    #[test]
    fn missing_or_garbled_embedded_version_fails_closed() {
        assert_eq!(portable_exe_version(b"not a PE"), None);
        assert!(!binary_matches_claimed_version(b"", "0.16.0"));
        assert!(!binary_matches_claimed_version(b"garbage", "0.16.0"));
        // A UTF-16LE "VS_VERSION_INFO" marker with a clobbered dwSignature must not be trusted.
        let mut exe = fake_vs_versioninfo(0, 16, 0);
        exe[40] = 0x00; // break dwSignature (0xFEEF_04BD -> 0xFEEF_0400)
        assert_eq!(portable_exe_version(&exe), None);
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
