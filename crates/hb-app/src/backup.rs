//! The whole-`~/.hoardbook` backup/restore seam (spec §Backup & Durability).
//!
//! Tauri-free so vitest's Rust sibling — `cargo test` — can drive it with a `tempdir` instead of a
//! real `AppHandle`. The confidentiality lives in [`hb_core::backup`]; this layer is the
//! directory-archiving + the **at-rest re-wrap**:
//!
//! - **Portable, not machine-bound (round-3 HIGH).** `backup_inner` does **not** tar the on-disk
//!   identity file verbatim — on Windows that is DPAPI ciphertext, dead on new hardware. It loads
//!   the identity via [`DataStore::load_identity`] → [`AppIdentity::to_stored`] and archives the
//!   **portable `StoredIdentity` JSON**. The outer passphrase AEAD is the only confidentiality
//!   layer in the archive.
//! - **Restore re-wraps under the local at-rest scheme.** `restore_inner` reads the portable
//!   identity **into memory** and persists it **only** via [`DataStore::save_identity`] (DPAPI on
//!   Windows, 0600 elsewhere), so the portable plaintext never lands on disk.
//! - **Every tar entry is hostile input (AB6 discipline).** Restore rejects `..` / absolute /
//!   escaping paths, forbids symlink + hardlink entries outright, and enforces tar-bomb caps
//!   (total size + entry count). A corrupt/garbage tar is a reasoned `Err`, never a panic.
//! - **Non-empty target is a hard refuse, not a clobber.** `restore_inner` returns
//!   [`BackupError::TargetNotEmpty`]; the UI owns the confirm-and-wipe before re-calling.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use hb_core::backup::{decrypt_backup, encrypt_backup, BackupMode};

use crate::store::{DataStore, StoredIdentity};

/// Tar-bomb cap: the backup is profile *metadata* (KB–MB), never the hoard, so 500 MiB is a wide
/// safety bound. An archive that extracts to more is refused.
const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// Tar-bomb cap: entry count. A real profile has tens–hundreds of files.
const MAX_ENTRIES: usize = 10_000;

/// The portable identity entry name inside the archive (always JSON, even on Windows where the
/// at-rest file is `identity.bin`).
const IDENTITY_ENTRY: &str = "identity/identity.json";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The target profile already holds data — clear it (wipe) before restoring. The UI owns the
    /// confirm-dialog and re-calls only after the directory is cleared (decision #5).
    #[error("the target profile already contains data — wipe it before restoring a backup")]
    TargetNotEmpty,

    /// A hostile or corrupt archive: path traversal, a forbidden link entry, a tar bomb, or a
    /// truncated/garbage tar (incl. a corrupted plaintext archive, which has no AEAD to catch it).
    #[error("backup archive rejected: {0}")]
    Archive(String),

    #[error(transparent)]
    Crypto(#[from] hb_core::HbError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Tar `~/.hoardbook` (with the portable identity injected) and seal it via `mode`. Returns the
/// versioned archive bytes; the Tauri wrapper writes them to a user-chosen file.
pub fn backup_inner(store: &DataStore, mode: BackupMode<'_>) -> Result<Vec<u8>, BackupError> {
    let base = store.base_dir();
    let identity_file = store.identity_path();

    let mut builder = tar::Builder::new(Vec::new());

    // Inject the PORTABLE identity (not the at-rest file). Absent identity → skip; an empty profile
    // is still archivable.
    if let Some(stored) = store.load_identity()? {
        let json = serde_json::to_vec_pretty(&stored).map_err(|e| BackupError::Other(e.into()))?;
        append_bytes(&mut builder, IDENTITY_ENTRY, &json)?;
    }

    // Archive every other regular file, skipping the at-rest identity file (it is machine-bound and
    // replaced by the portable form above).
    let mut files = Vec::new();
    if base.exists() {
        collect_files(base, base, &identity_file, &mut files)?;
    }
    files.sort(); // deterministic ordering
    for (rel, abs) in files {
        let data = std::fs::read(&abs)?;
        append_bytes(&mut builder, &rel, &data)?;
    }

    let tar_bytes = builder.into_inner().map_err(BackupError::Io)?;
    Ok(encrypt_backup(mode, &tar_bytes)?)
}

/// Decrypt → sanitize → unpack an archive into `store`'s directory, re-wrapping the secrets under
/// the local at-rest scheme. `passphrase` is `Option` because the archive header is self-describing
/// (an encrypted archive + `None` is a reasoned `Err`). Refuses a non-empty target.
pub fn restore_inner(
    store: &DataStore,
    archive: &[u8],
    passphrase: Option<&str>,
) -> Result<(), BackupError> {
    if target_is_occupied(store) {
        return Err(BackupError::TargetNotEmpty);
    }
    if !hb_core::backup::is_encrypted_backup(archive)? && passphrase.is_some() {
        tracing::debug!("plaintext backup: ignoring the supplied passphrase (header is authoritative)");
    }

    let tar_bytes = decrypt_backup(passphrase, archive)?;
    let base = store.base_dir().to_path_buf();

    let mut ar = tar::Archive::new(tar_bytes.as_slice());
    let entries = ar
        .entries()
        .map_err(|e| BackupError::Archive(format!("corrupt tar: {e}")))?;

    let mut total: u64 = 0;
    let mut count: usize = 0;
    let mut pending_identity: Option<StoredIdentity> = None;

    for entry in entries {
        let mut entry = entry.map_err(|e| BackupError::Archive(format!("corrupt tar entry: {e}")))?;

        // Links add only TOCTOU/escape surface — a metadata backup is regular files + dirs only.
        let etype = entry.header().entry_type();
        if etype.is_symlink() || etype.is_hard_link() {
            return Err(BackupError::Archive("symlink/hardlink entries are forbidden".into()));
        }

        let raw = entry
            .path()
            .map_err(|e| BackupError::Archive(format!("unreadable entry path: {e}")))?
            .into_owned();
        let rel = sanitize_rel(&raw)?;

        // Tar-bomb caps, on the *declared* size, before reading bytes into memory.
        let size = entry.header().size().unwrap_or(0);
        total = total.saturating_add(size);
        count += 1;
        if count > MAX_ENTRIES {
            return Err(BackupError::Archive(format!("too many entries (> {MAX_ENTRIES})")));
        }
        if total > MAX_TOTAL_BYTES {
            return Err(BackupError::Archive("archive exceeds the size cap (tar bomb?)".into()));
        }

        if etype.is_dir() {
            std::fs::create_dir_all(base.join(&rel))?;
            continue;
        }

        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| BackupError::Archive(format!("truncated entry: {e}")))?;

        // The portable identity is re-wrapped via save_identity, never written verbatim — so the
        // portable plaintext never touches disk (on Windows it becomes DPAPI ciphertext).
        if rel_eq(&rel, IDENTITY_ENTRY) {
            let stored: StoredIdentity = serde_json::from_slice(&buf)
                .map_err(|e| BackupError::Archive(format!("identity entry is not valid JSON: {e}")))?;
            pending_identity = Some(stored);
            continue;
        }

        let dest = base.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // `create_new` (O_EXCL) refuses to follow or overwrite an existing path — closes the
        // symlink-follow + TOCTOU gap a bare `fs::write` would leave (chorus/Codex). The target is
        // already required to be empty, so a collision here means a hostile/duplicate entry.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| BackupError::Archive(format!("cannot create '{}': {e}", rel.display())))?;
        f.write_all(&buf)?;
    }

    if let Some(stored) = pending_identity {
        store.save_identity(&stored).map_err(BackupError::Other)?;
    }
    Ok(())
}

// --- Helpers --------------------------------------------------------------------------------

fn append_bytes(
    builder: &mut tar::Builder<Vec<u8>>,
    name: &str,
    data: &[u8],
) -> Result<(), BackupError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, name, data)
        .map_err(BackupError::Io)
}

/// Recursively collect regular files under `dir` as `(forward-slash relative path, absolute path)`,
/// skipping the at-rest identity file. (No symlink following: `read_dir` + `is_file` only.)
fn collect_files(
    base: &Path,
    dir: &Path,
    identity_file: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), BackupError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue; // never archive a symlink out of our own dir
        }
        if ft.is_dir() {
            collect_files(base, &path, identity_file, out)?;
        } else if ft.is_file() {
            if path == identity_file {
                continue; // machine-bound at-rest file; the portable form is injected separately
            }
            let rel = path
                .strip_prefix(base)
                .map_err(|e| BackupError::Other(anyhow::anyhow!(e)))?;
            out.push((rel_to_slash(rel), path));
        }
    }
    Ok(())
}

/// Reject absolute / `..` / prefix / root components; return a clean relative path. The AB6
/// discipline applied to restore — a `../../.ssh/authorized_keys` entry must never be written.
fn sanitize_rel(path: &Path) -> Result<PathBuf, BackupError> {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(BackupError::Archive(format!(
                    "unsafe path component in '{}'",
                    path.display()
                )));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(BackupError::Archive("empty entry path".into()));
    }
    Ok(out)
}

fn rel_to_slash(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Case-insensitive match against a known slash path. The identity entry is special-cased
/// (re-wrapped, not written verbatim); on a case-insensitive FS (macOS/Windows) a `Identity/…`
/// variant would otherwise slip past as a regular file and collide with the real one
/// (chorus/opencode). We always *write* lowercase, so an exact-case match is the norm; this just
/// closes the case-variant collision.
fn rel_eq(rel: &Path, slash_name: &str) -> bool {
    rel_to_slash(rel).eq_ignore_ascii_case(slash_name)
}

/// Is the target profile already occupied (so a restore would clobber)? **Any** entry in the base
/// dir counts — not just known Hoardbook paths (chorus/Codex: an allowlist would let an unknown /
/// stale / attacker-placed file survive and be overwritten). The UI wipes first; a missing base dir
/// is "empty".
fn target_is_occupied(store: &DataStore) -> bool {
    match std::fs::read_dir(store.base_dir()) {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => false, // base does not exist yet → empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity_state::AppIdentity;
    use tempfile::TempDir;

    const PASS: &str = "a-strong-restore-passphrase";

    fn store_with_fake_profile() -> (TempDir, DataStore, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let id = AppIdentity::generate();
        let npub = id.npub();
        store.save_identity(&id.to_stored().unwrap()).unwrap();
        // A spread of profile data across the layout.
        std::fs::create_dir_all(store.base_dir().join("collections")).unwrap();
        std::fs::write(store.base_dir().join("collections/films.draft.json"), b"{\"slug\":\"films\"}").unwrap();
        std::fs::create_dir_all(store.base_dir().join("contacts")).unwrap();
        std::fs::write(store.base_dir().join("contacts/abc.json"), b"{\"npub\":\"x\"}").unwrap();
        std::fs::write(store.settings_path(), b"{\"relay_urls\":[],\"allow_dms\":true}").unwrap();
        (dir, store, npub)
    }

    fn empty_store() -> (TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn backup_inner_then_restore_inner_roundtrips_whole_dir() {
        let (_d1, src, npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();

        let (_d2, dst) = empty_store();
        restore_inner(&dst, &archive, Some(PASS)).unwrap();

        // Identity + every file came back.
        let restored = AppIdentity::from_stored(&dst.load_identity().unwrap().unwrap()).unwrap();
        assert_eq!(restored.npub(), npub, "identity npub survives the backup roundtrip");
        assert_eq!(
            std::fs::read(dst.base_dir().join("collections/films.draft.json")).unwrap(),
            b"{\"slug\":\"films\"}"
        );
        assert_eq!(
            std::fs::read(dst.base_dir().join("contacts/abc.json")).unwrap(),
            b"{\"npub\":\"x\"}"
        );
        assert!(dst.settings_path().exists());
    }

    #[test]
    fn backup_archives_portable_identity_not_at_rest_ciphertext() {
        // round-3 HIGH: the tar carries the portable StoredIdentity JSON, so it restores on new
        // hardware. We assert the archived identity entry parses as StoredIdentity (the portable
        // form), independent of the platform at-rest scheme.
        let (_d, src, npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Plaintext).unwrap();
        let tar_bytes = decrypt_backup(None, &archive).unwrap();
        let mut ar = tar::Archive::new(tar_bytes.as_slice());
        let mut found = None;
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            if rel_eq(&sanitize_rel(&e.path().unwrap()).unwrap(), IDENTITY_ENTRY) {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).unwrap();
                found = Some(buf);
            }
        }
        let buf = found.expect("identity entry present");
        let stored: StoredIdentity =
            serde_json::from_slice(&buf).expect("archived identity is the portable StoredIdentity JSON");
        let id = AppIdentity::from_stored(&stored).unwrap();
        assert_eq!(id.npub(), npub);
    }

    #[test]
    fn restore_rewraps_secrets_under_local_at_rest() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        let (_d2, dst) = empty_store();
        restore_inner(&dst, &archive, Some(PASS)).unwrap();

        let on_disk = std::fs::read(dst.identity_path()).unwrap();
        #[cfg(target_os = "windows")]
        {
            // DPAPI ciphertext: not the portable JSON.
            assert!(serde_json::from_slice::<StoredIdentity>(&on_disk).is_err(),
                "Windows at-rest identity must be DPAPI ciphertext, not portable plaintext");
        }
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dst.identity_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "restored identity must be re-wrapped at 0600");
            // On Linux 0600-plaintext IS the at-rest scheme, so it parses — that's correct.
            let _ = on_disk;
        }
    }

    #[test]
    fn restore_into_nonempty_dir_returns_target_not_empty() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        // Restore back into the SAME occupied dir → hard refuse, no clobber.
        let err = restore_inner(&src, &archive, Some(PASS)).unwrap_err();
        assert!(matches!(err, BackupError::TargetNotEmpty), "got {err:?}");
    }

    #[test]
    fn restore_refuses_any_nonempty_target_even_unknown_files() {
        // chorus/Codex: a stray/unknown file in the target (not a known Hoardbook path) must still
        // block restore — the invariant is "empty target", not an allowlist.
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        let (_d2, dst) = empty_store();
        std::fs::create_dir_all(dst.base_dir()).unwrap();
        std::fs::write(dst.base_dir().join("some-unrelated-file.txt"), b"stray").unwrap();
        let err = restore_inner(&dst, &archive, Some(PASS)).unwrap_err();
        assert!(matches!(err, BackupError::TargetNotEmpty), "got {err:?}");
    }

    #[test]
    fn encrypted_archive_restored_without_passphrase_errs() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        let (_d2, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Crypto(hb_core::HbError::PassphraseRequired)), "got {err:?}");
    }

    #[test]
    fn plaintext_archive_with_passphrase_ignores_passphrase() {
        let (_d1, src, npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Plaintext).unwrap();
        let (_d2, dst) = empty_store();
        // A stray passphrase on a plaintext archive is ignored (the mode=0 header is authoritative).
        restore_inner(&dst, &archive, Some("ignored-passphrase")).unwrap();
        let restored = AppIdentity::from_stored(&dst.load_identity().unwrap().unwrap()).unwrap();
        assert_eq!(restored.npub(), npub);
    }

    #[test]
    fn restore_of_tampered_archive_fails_with_reason() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let mut archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        let last = archive.len() - 1;
        archive[last] ^= 0x01;
        let (_d2, dst) = empty_store();
        let err = restore_inner(&dst, &archive, Some(PASS)).unwrap_err();
        assert!(matches!(err, BackupError::Crypto(hb_core::HbError::DecryptionFailed)), "got {err:?}");
    }

    #[test]
    fn corrupted_plaintext_archive_rejected_not_panicked() {
        // round-2: a bit-flipped/truncated mode=0 archive has no AEAD, so the rejection comes from
        // tar extraction — a reasoned Err, never a panic.
        let (_d1, src, _npub) = store_with_fake_profile();
        let mut archive = backup_inner(&src, BackupMode::Plaintext).unwrap();
        // Corrupt the tar body (past the 72-byte header) so extraction fails.
        for b in archive.iter_mut().skip(80).take(64) {
            *b ^= 0xFF;
        }
        let (_d2, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "got {err:?}");
    }

    fn tar_with_entry(name: &str, etype: tar::EntryType, link_target: Option<&str>) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o600);
        h.set_entry_type(etype);
        if let Some(t) = link_target {
            h.set_link_name(t).unwrap();
        }
        h.set_cksum();
        b.append_data(&mut h, name, std::io::empty()).unwrap();
        let tar_bytes = b.into_inner().unwrap();
        encrypt_backup(BackupMode::Plaintext, &tar_bytes).unwrap()
    }

    #[test]
    fn restore_rejects_tar_path_traversal_entries() {
        // The `tar` crate sanitizes `..` on *write*, so forge the entry name directly into the
        // header bytes (an attacker-crafted archive would). Restore's own guard must catch it.
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(0o600);
        {
            let raw = h.as_mut_bytes();
            let name = b"../../escape.txt";
            raw[..name.len()].copy_from_slice(name);
        }
        h.set_cksum();
        b.append(&h, std::io::empty()).unwrap();
        let archive = encrypt_backup(BackupMode::Plaintext, &b.into_inner().unwrap()).unwrap();

        let (_d, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "got {err:?}");
        // Nothing escaped the target dir.
        assert!(!dst.base_dir().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn restore_rejects_symlink_and_hardlink_entries() {
        for et in [tar::EntryType::Symlink, tar::EntryType::Link] {
            let archive = tar_with_entry("evil", et, Some("/etc/passwd"));
            let (_d, dst) = empty_store();
            let err = restore_inner(&dst, &archive, None).unwrap_err();
            assert!(matches!(err, BackupError::Archive(_)), "link entry {et:?} must be refused, got {err:?}");
        }
    }

    #[test]
    fn restore_rejects_tar_bomb_too_many_entries() {
        // Uncompressed tar's realistic bomb vector is entry count (a sea of tiny files). Build one
        // past the cap → refused by the entry-count guard, never an unbounded extraction.
        let mut b = tar::Builder::new(Vec::new());
        for i in 0..=MAX_ENTRIES {
            let mut h = tar::Header::new_gnu();
            h.set_size(0);
            h.set_mode(0o600);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            b.append_data(&mut h, format!("f{i}.bin"), std::io::empty()).unwrap();
        }
        let archive = encrypt_backup(BackupMode::Plaintext, &b.into_inner().unwrap()).unwrap();
        let (_d, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "too-many-entries must be refused, got {err:?}");
    }
}
