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
//!   escaping paths (including the Windows-normalization variants like `.. ` and `...`,
//!   QURATOR-224), forbids symlink + hardlink entries outright, and enforces tar-bomb caps
//!   (total size + entry count + materialized directory components, QURATOR-230). A
//!   corrupt/garbage tar is a reasoned `Err`, never a panic.
//! - **Non-empty target is a hard refuse, not a clobber.** `restore_inner` returns
//!   [`BackupError::TargetNotEmpty`]; the UI owns the confirm-and-wipe before re-calling.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use hb_core::backup::{decrypt_backup, encrypt_backup, BackupMode};

use crate::store::{DataStore, StoredIdentity};

/// Tar-bomb cap: the backup is profile *metadata* (KB–MB), never the hoard, so 500 MiB is a wide
/// safety bound. An archive that extracts to more is refused.
const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// Tar-bomb cap: entry count. A real profile has tens–hundreds of files.
const MAX_ENTRIES: usize = 10_000;
/// Tar-bomb cap: cumulative path components materialized (QURATOR-230). Directories declare
/// size 0 and count as one entry each, so neither the byte cap nor the entry cap bounds a sea
/// of deep paths (~2,000-deep names just under PATH_MAX × `MAX_ENTRIES` entries ≈ 20M mkdirs,
/// inode exhaust). Every entry's component count accrues here — a file's parent dirs are
/// materialized just as truly as a dir entry's own segments — so shared prefixes are
/// over-counted, which is conservative. 50k sits above a maximal legit profile (10,000 entries
/// × 3 components deep) and far below the amplification.
const MAX_DIR_COMPONENTS: usize = 50_000;

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
/// (an encrypted archive + `None` is a reasoned `Err`; a plaintext archive + a supplied passphrase
/// is a tamper-signal `Err`, QURATOR-228). Refuses a non-empty target.
///
/// **Atomic (QURATOR-126 #15):** the archive is extracted *in full* into a staging directory
/// beside the target; only a complete, error-free extraction is committed by renaming it into
/// place. A failure at any entry — truncated tail, hostile path, oversize entry, invalid identity
/// JSON — leaves the target exactly as it was, never a half-materialized store. INV-8: data lands
/// deliberately or not at all.
pub fn restore_inner(
    store: &DataStore,
    archive: &[u8],
    passphrase: Option<&str>,
) -> Result<(), BackupError> {
    if target_is_occupied(store) {
        return Err(BackupError::TargetNotEmpty);
    }
    let base = store.base_dir().to_path_buf();

    // Stage BESIDE the target so the commit is a same-filesystem rename, never EXDEV.
    let parent = base.parent().ok_or_else(|| {
        BackupError::Archive("profile dir has no parent to stage a restore beside".into())
    })?;
    let stage = tempfile::Builder::new()
        .prefix(".hb-restore-stage-")
        .tempdir_in(parent)?;
    let stage_store = DataStore::new(stage.path().to_path_buf());

    // Any extraction error discards staging (on drop) and returns before the target is touched.
    extract_archive(&stage_store, archive, passphrase)?;
    commit_stage(stage.path(), &base)
}

/// Fully validate an archive WITHOUT touching any datastore (QURATOR-126 #15): run the exact
/// [`extract_archive`] production path — Argon2id KDF, AEAD decrypt, every-entry sanitize + tar-bomb
/// caps, identity JSON parse + semantic validation (QURATOR-235) + at-rest re-wrap — into a
/// throwaway directory. `Ok(())` is the
/// promise that restoring this file with this passphrase into a wiped profile will succeed; every
/// failure a restore can hit is hit here first, at zero risk to live data. This replaces a 72-byte
/// header sniff as the pre-wipe gate.
pub fn validate_inner(archive: &[u8], passphrase: Option<&str>) -> Result<(), BackupError> {
    let scratch = tempfile::tempdir()?;
    let scratch_store = DataStore::new(scratch.path().to_path_buf());
    extract_archive(&scratch_store, archive, passphrase)
}

/// The ONE decrypt-and-extract core, shared by [`restore_inner`] (into staging) and
/// [`validate_inner`] (into a throwaway dir): decrypt, sanitize, and write every entry into
/// `store`'s (empty) base dir, re-wrapping the identity under the local at-rest scheme. There is
/// deliberately no second, validation-only parser — validation must fail exactly where restore
/// would.
fn extract_archive(
    store: &DataStore,
    archive: &[u8],
    passphrase: Option<&str>,
) -> Result<(), BackupError> {
    // A supplied passphrase on a plaintext-header archive is a hard MISMATCH, not a silent
    // no-op (QURATOR-228): the mode byte is attacker-controlled, and a file that claims
    // plaintext while the user expects encryption is the classic swapped-file signal — the
    // exact threat the passphrase AEAD exists to make detectable. Honouring the header over
    // the user's encrypted posture would turn an authenticated restore into an unauthenticated
    // one with no error ever reaching the user.
    if !hb_core::backup::is_encrypted_backup(archive)? && passphrase.is_some() {
        return Err(BackupError::Archive(
            "passphrase supplied but the archive is not encrypted — the file may have been tampered with or swapped"
                .into(),
        ));
    }

    let tar_bytes = decrypt_backup(passphrase, archive)?;
    let base = store.base_dir().to_path_buf();

    let mut ar = tar::Archive::new(tar_bytes.as_slice());
    let entries = ar
        .entries()
        .map_err(|e| BackupError::Archive(format!("corrupt tar: {e}")))?;

    let mut total: u64 = 0;
    let mut read_total: u64 = 0;
    let mut count: usize = 0;
    let mut components_total: usize = 0;
    let mut pending_identity: Option<StoredIdentity> = None;

    for entry in entries {
        let entry = entry.map_err(|e| BackupError::Archive(format!("corrupt tar entry: {e}")))?;

        // Links add only TOCTOU/escape surface — a metadata backup is regular files + dirs only.
        let etype = entry.header().entry_type();
        if etype.is_symlink() || etype.is_hard_link() {
            return Err(BackupError::Archive("symlink/hardlink entries are forbidden".into()));
        }
        // A GNU sparse entry declares a small on-disk size while its read materializes the logical
        // (attacker-chosen, up to hundreds of GB) size — the one entry type that can outsize its
        // own header. Refused outright; a metadata backup is regular files + dirs only.
        if etype.is_gnu_sparse() {
            return Err(BackupError::Archive("sparse entries are forbidden".into()));
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
        // Directory materialization is part of the bomb surface too: dirs declare size 0 and
        // count as one entry, so only a cumulative component budget bounds deep-path
        // amplification (QURATOR-230). Counted before anything is created on disk.
        components_total = components_total.saturating_add(rel.components().count());
        if count > MAX_ENTRIES {
            return Err(BackupError::Archive(format!("too many entries (> {MAX_ENTRIES})")));
        }
        if total > MAX_TOTAL_BYTES {
            return Err(BackupError::Archive("archive exceeds the size cap (tar bomb?)".into()));
        }
        if components_total > MAX_DIR_COMPONENTS {
            return Err(BackupError::Archive(
                "archive exceeds the directory budget (tar bomb?)".into(),
            ));
        }

        if etype.is_dir() {
            std::fs::create_dir_all(base.join(&rel))?;
            continue;
        }

        // Bound the *actual* read to the remaining budget — a size-lying entry must not OOM
        // restore. `take(remaining + 1)` lets a >budget entry overflow by one byte so we detect
        // the overrun instead of silently truncating (INV-8) or materializing an attacker size.
        let remaining = MAX_TOTAL_BYTES.saturating_sub(read_total);
        let buf = read_entry_bounded(entry, remaining)?;
        read_total = read_total.saturating_add(buf.len() as u64);

        // The portable identity is re-wrapped via save_identity, never written verbatim — so the
        // portable plaintext never touches disk (on Windows it becomes DPAPI ciphertext).
        if rel_eq(&rel, IDENTITY_ENTRY) {
            let stored: StoredIdentity = serde_json::from_slice(&buf)
                .map_err(|e| BackupError::Archive(format!("identity entry is not valid JSON: {e}")))?;
            // Semantic validation BEFORE anything is persisted (QURATOR-235): a JSON-valid but
            // garbage identity must fail HERE, inside the shared core. `validate_inner`'s
            // `Ok(())` promises the restore will succeed; a `from_stored` failure surfacing
            // only after `commit_stage` has replaced the (already wiped) profile bricks it
            // with attacker files and a dead-end identity.
            crate::identity_state::AppIdentity::from_stored(&stored).map_err(|e| {
                BackupError::Archive(format!("identity entry is not a valid identity: {e}"))
            })?;
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

/// Move a fully-extracted staging dir into `base` — the commit step of atomic restore. The target
/// was verified empty up front, but it may have acquired content since (or been materialized as a
/// file); re-check and refuse rather than clobber. An existing-but-EMPTY dir is removed first
/// (`rename` onto a directory is refused on Windows); it holds no data, so losing it costs
/// nothing. The final `rename` is atomic on the same filesystem: the target is either untouched
/// or the complete store — no intermediate state a crash can leave behind (INV-8).
fn commit_stage(stage: &Path, base: &Path) -> Result<(), BackupError> {
    if target_path_is_occupied(base) {
        return Err(BackupError::TargetNotEmpty);
    }
    if base.exists() {
        // Verified empty above; rmdir cannot lose data. Failure means it wasn't empty after all.
        std::fs::remove_dir(base).map_err(|e| {
            BackupError::Archive(format!("target '{}' is not empty: {e}", base.display()))
        })?;
    }
    if let Err(e) = std::fs::rename(stage, base) {
        return Err(BackupError::Archive(format!(
            "cannot commit restore into '{}': {e}",
            base.display()
        )));
    }
    Ok(())
}

/// [`target_is_occupied`] for a bare path (staging uses a fresh `DataStore`, so the store-flavored
/// helper can't be reused at commit time). Same semantics: a missing dir or an existing-but-EMPTY
/// dir is free; anything else — entries, a file, a symlink — is occupied.
fn target_path_is_occupied(base: &Path) -> bool {
    match std::fs::symlink_metadata(base) {
        Err(_) => false, // does not exist → free
        Ok(md) if !md.is_dir() => true, // a file or symlink occupies the path
        Ok(_) => match std::fs::read_dir(base) {
            Ok(mut entries) => entries.next().is_some(),
            Err(_) => true, // unreadable → treat as occupied, never clobber blind
        },
    }
}

// --- Helpers -------------------------------------------------------------------------------

// --- Helpers --------------------------------------------------------------------------------

/// Read one entry bounded by the remaining byte budget. `take(remaining + 1)` lets a >budget
/// entry overflow by exactly one byte so we can detect the overrun and reject it — never silently
/// truncating user data (INV-8) or materializing an attacker-chosen size (tar bomb).
fn read_entry_bounded<R: Read>(entry: R, remaining: u64) -> Result<Vec<u8>, BackupError> {
    let mut buf = Vec::new();
    entry
        .take(remaining.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| BackupError::Archive(format!("truncated entry: {e}")))?;
    if buf.len() as u64 > remaining {
        return Err(BackupError::Archive("archive exceeds the size cap (tar bomb?)".into()));
    }
    Ok(buf)
}

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
///
/// Windows-normalization hardening (QURATOR-224): Win32 path normalization strips trailing
/// dots and spaces from every segment, so a component like `.. ` or `...` is `Normal` to
/// Rust's lexical rules but resolves as `..` on the filesystem (and `identity. ` collides with
/// `identity`). Any component that would not survive that normalization verbatim is rejected
/// here, before it is ever pushed — the check must bind under the FILESYSTEM's rules, not just
/// the lexer's.
fn sanitize_rel(path: &Path) -> Result<PathBuf, BackupError> {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(c) => {
                if win32_would_normalize(c) {
                    return Err(BackupError::Archive(format!(
                        "unsafe path component in '{}'",
                        path.display()
                    )));
                }
                out.push(c)
            }
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

/// A component Win32 path normalization would not preserve verbatim: normalization strips
/// trailing dots and spaces from each path segment (`.. ` and `...` resolve as `..`, `foo. `
/// as `foo`), so the filesystem-resolved name diverges from the lexically-checked one
/// (QURATOR-224). Our own writer never emits such names; seeing one means a hand-crafted
/// header.
fn win32_would_normalize(c: &OsStr) -> bool {
    matches!(c.as_encoded_bytes().last(), Some(b'.') | Some(b' '))
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

    // -- QURATOR-126 #15: pre-wipe validation + atomic restore ---------------------------------

    #[test]
    fn validate_inner_accepts_good_archive_without_touching_a_store() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();

        // A live store holding REAL data: validation must leave it byte-identical.
        let (_d2, dst, _n2) = store_with_fake_profile();
        let before: Vec<_> = std::fs::read(dst.identity_path()).unwrap();
        let before_coll = std::fs::read(dst.base_dir().join("collections/films.draft.json")).unwrap();
        validate_inner(&archive, Some(PASS)).unwrap();
        assert_eq!(std::fs::read(dst.identity_path()).unwrap(), before);
        assert_eq!(
            std::fs::read(dst.base_dir().join("collections/films.draft.json")).unwrap(),
            before_coll,
            "validation wrote nothing to the datastore"
        );
    }

    #[test]
    fn validate_inner_rejects_wrong_passphrase() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        let err = validate_inner(&archive, Some("a-typo-in-the-passphrase")).unwrap_err();
        assert!(matches!(err, BackupError::Crypto(_)), "got {err:?}");
    }

    #[test]
    fn validate_inner_rejects_truncated_archive() {
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Passphrase(PASS)).unwrap();
        let cut = &archive[..archive.len() / 2];
        let err = validate_inner(cut, Some(PASS)).unwrap_err();
        assert!(!matches!(err, BackupError::TargetNotEmpty), "got {err:?}");
    }

    #[test]
    fn validate_inner_goes_through_the_tar_guards() {
        // The hostile-entry guards (traversal/link/bomb/sparse) live in the shared extraction core;
        // validation must reject the same archives restore does, not a softer header sniff.
        // (`tar_with_entry` already seals with Plaintext mode; the traversal one forges the name
        // into the header bytes because the `tar` crate sanitizes `..` on write.)
        for hostile in [
            tar_with_forged_name(b"../escape.txt"),
            tar_with_entry("lnk", tar::EntryType::Symlink, Some("target")),
            tar_with_entry("hlk", tar::EntryType::Link, Some("target")),
        ] {
            assert!(validate_inner(&hostile, None).is_err(), "must reject hostile entry");
        }
    }

    #[test]
    fn validate_inner_rejects_invalid_identity_json() {
        // A corrupt identity entry must fail VALIDATION (pre-wipe), not first at restore time.
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(9);
        h.set_mode(0o600);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, IDENTITY_ENTRY, &b"not{json"[..]).unwrap();
        let tar_bytes = b.into_inner().unwrap();
        let archive = encrypt_backup(BackupMode::Plaintext, &tar_bytes).unwrap();
        assert!(validate_inner(&archive, None).is_err());
    }

    #[test]
    fn restore_failure_mid_archive_leaves_target_untouched() {
        // THE atomicity pin: entry #1 is fine, a LATER entry is hostile — the old code wrote entry
        // #1 directly into the target before erroring on #2. After the fix the target must hold
        // exactly its pre-restore state: empty (the UI wipes first), with entry #1 absent.
        let (_d1, _src, _npub) = store_with_fake_profile();
        let mut b = tar::Builder::new(Vec::new());
        append_bytes(&mut b, "collections/first.json", b"{\"ok\":true}").unwrap();
        // Entry #2 forges a traversal name straight into the header (the builder sanitizes `..`).
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o600);
        h.set_entry_type(tar::EntryType::Regular);
        {
            let raw = h.as_mut_bytes();
            let name = b"../escape";
            raw[..name.len()].copy_from_slice(name);
        }
        h.set_cksum();
        b.append(&h, &b"bad"[..]).unwrap();
        let tar_bytes = b.into_inner().unwrap();
        let archive = encrypt_backup(BackupMode::Plaintext, &tar_bytes).unwrap();

        let (_d2, dst) = empty_store();
        // A pre-existing file the failed restore must never touch: it sits in the parent (the
        // target itself must be empty for restore to run at all).
        let outside = dst.base_dir().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, b"pre-restore state").unwrap();
        assert!(restore_inner(&dst, &archive, None).is_err());

        let leftover: Vec<String> = std::fs::read_dir(dst.base_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftover.is_empty(),
            "a mid-archive failure must not leave a half-materialized store: {leftover:?}");
        assert_eq!(std::fs::read(&outside).unwrap(), b"pre-restore state");
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
    fn plaintext_archive_with_passphrase_is_refused() {
        // QURATOR-228: a supplied passphrase on a plaintext-header archive is a tamper signal,
        // not a no-op — the mode byte is attacker-controlled, and silently honouring it over the
        // user's encrypted posture turns an authenticated restore into an unauthenticated one
        // (the swapped-file attack). This test SUPERSEDES
        // `plaintext_archive_with_passphrase_ignores_passphrase`, which pinned the old
        // ignore-behaviour (a debug log, then success).
        //
        // MUTATION (must redden): in `extract_archive`, revert the plaintext+passphrase guard
        // to the old `tracing::debug!(...)`-and-continue — the restore then succeeds and
        // `unwrap_err()` panics.
        let (_d1, src, _npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Plaintext).unwrap();
        let (_d2, dst) = empty_store();
        let err = restore_inner(&dst, &archive, Some("a-passphrase")).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "got {err:?}");
        // The mismatch must also fire at the pre-wipe gate, before the UI destroys anything.
        assert!(validate_inner(&archive, Some("a-passphrase")).is_err());
        // And the target was never touched (staging discarded, INV-8).
        assert!(std::fs::read_dir(dst.base_dir()).unwrap().next().is_none());
    }

    #[test]
    fn plaintext_archive_restores_fine_without_passphrase() {
        // Positive control for the QURATOR-228 guard: plaintext + `None` is the legitimate
        // combination and must keep working — the guard pins the MISMATCH, not plaintext mode.
        let (_d1, src, npub) = store_with_fake_profile();
        let archive = backup_inner(&src, BackupMode::Plaintext).unwrap();
        validate_inner(&archive, None).unwrap();
        let (_d2, dst) = empty_store();
        restore_inner(&dst, &archive, None).unwrap();
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

    /// A sealed archive whose one entry carries `name` forged straight into the tar header — the
    /// `tar` crate sanitizes `..` on *write*, so an attacker-crafted traversal name must be laid
    /// into the raw header bytes (same trick as `restore_rejects_tar_path_traversal_entries`).
    fn tar_with_forged_name(name: &[u8]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(0o600);
        {
            let raw = h.as_mut_bytes();
            raw[..name.len()].copy_from_slice(name);
        }
        h.set_cksum();
        b.append(&h, std::io::empty()).unwrap();
        encrypt_backup(BackupMode::Plaintext, &b.into_inner().unwrap()).unwrap()
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

    #[test]
    fn restore_rejects_gnu_sparse_entries() {
        // A GNU sparse entry declares a small on-disk size (64 B here) while `read_to_end`
        // materializes its logical size — the one entry type that can outsize its own header and
        // blow past the caps. Restore must refuse it outright, not extract it.
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::GNUSparse);
        h.set_size(64);
        h.set_mode(0o600);
        h.set_path("sparse.bin").unwrap();
        {
            let gnu = h.as_gnu_mut().unwrap();
            gnu.sparse[0].set_offset(0);
            gnu.sparse[0].set_length(64);
            gnu.set_real_size(64);
        }
        h.set_cksum();
        b.append(&h, &[0xAAu8; 64][..]).unwrap();
        let archive = encrypt_backup(BackupMode::Plaintext, &b.into_inner().unwrap()).unwrap();

        let (_d, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "sparse entry must be refused, got {err:?}");
    }

    #[test]
    fn read_entry_bounded_rejects_oversize_reader() {
        // The cap must bound the *actual* read, not the declared size: a reader that lies (yields
        // more bytes than the remaining budget) is rejected, not materialized.
        let liar = std::io::Cursor::new(vec![0xAAu8; 100]);
        let err = read_entry_bounded(liar, 10).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "oversize reader must be refused, got {err:?}");
    }

    #[test]
    fn read_entry_bounded_accepts_within_budget() {
        // Positive control: a reader that fits the budget is read in full, not over-rejected.
        let ok = std::io::Cursor::new(vec![0xAAu8; 8]);
        let buf = read_entry_bounded(ok, 10).unwrap();
        assert_eq!(buf.len(), 8);
    }

    // -- QURATOR-224: Windows path-normalization traversal --------------------------------------

    #[test]
    fn sanitize_rel_rejects_windows_normalization_variants() {
        // `.. `, `...`, ` .`, `foo.`, `foo ` are `Component::Normal` to Rust's lexer, but Win32
        // normalization strips trailing dots/spaces per segment, so `.. ` resolves as `..` and
        // `identity. ` collides with `identity` on the filesystem. All must be refused before
        // any filesystem call, on every platform.
        //
        // MUTATION (must redden): in `sanitize_rel`, revert the `Component::Normal(c)` arm to
        // the unguarded `out.push(c)` (delete the `win32_would_normalize` check) — these
        // components then pass the lexer and `Ok(_)` is returned.
        for hostile in [
            ".. /escape",
            "...",
            "a/.. /b",
            " .",
            "foo.",
            "foo ",
            "identity/identity. ",
        ] {
            let err = sanitize_rel(Path::new(hostile)).unwrap_err();
            assert!(
                matches!(err, BackupError::Archive(_)),
                "{hostile:?} must be refused, got {err:?}"
            );
        }
        // Positive control: clean names (dots/spaces in the MIDDLE) still pass.
        assert_eq!(
            sanitize_rel(Path::new("a.b/c d/e.json")).unwrap(),
            PathBuf::from("a.b/c d/e.json")
        );
    }

    #[test]
    fn restore_rejects_windows_dot_traversal_entries() {
        // The QURATOR-224 vector end-to-end: a forged `.. ` component is `Normal` to the lexer
        // but Win32 trims it to `..` at the filesystem layer. On Linux `.. ` is a literal
        // filename, so the pin is that the GUARD refuses it — a `Normal(".. ")` component must
        // never reach the FS on any platform. Forged into the header bytes like the plain `..`
        // sibling test, since the builder sanitizes `..` on write but passes `.. ` through.
        //
        // MUTATION (must redden): in `sanitize_rel`, delete the `win32_would_normalize` guard
        // so the arm is `Component::Normal(c) => out.push(c)` again — the entry is then
        // written (on Linux as a literal `.. ` file) and restore returns `Ok(())`.
        let archive = tar_with_forged_name(b".. /escape.txt");
        let (_d, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "got {err:?}");
        assert!(!dst.base_dir().parent().unwrap().join("escape.txt").exists());
    }

    // -- QURATOR-230: directory-materialization tar bomb ----------------------------------------

    #[test]
    fn restore_rejects_directory_materialization_bomb() {
        // Directories declare size 0 and each counts as ONE entry, so neither the byte cap nor
        // the entry cap sees a sea of deep paths (~2,000-deep names just under PATH_MAX ×
        // MAX_ENTRIES entries ≈ 20M mkdirs of inode exhaust). The cumulative component budget
        // must refuse it. Entries share a prefix so a mutated (guard-removed) run stays cheap —
        // the budget still trips on the cumulative count.
        //
        // MUTATION (must redden): delete the `components_total` accrual + `MAX_DIR_COMPONENTS`
        // check in `extract_archive`'s caps block — the archive then extracts cleanly (one
        // shared ~1,899-dir prefix, 28 tiny files) and `unwrap_err()` panics.
        let depth: usize = 1_900; // components per entry — full path stays under PATH_MAX
        let entries = (MAX_DIR_COMPONENTS / depth) + 2; // 28 × 1,900 = 53,200 > 50,000
        let prefix = vec!["a"; depth - 1].join("/");
        let mut b = tar::Builder::new(Vec::new());
        for i in 0..entries {
            append_bytes(&mut b, &format!("{prefix}/f{i}"), b"x").unwrap();
        }
        let archive = encrypt_backup(BackupMode::Plaintext, &b.into_inner().unwrap()).unwrap();
        let (_d, dst) = empty_store();
        let err = restore_inner(&dst, &archive, None).unwrap_err();
        assert!(matches!(err, BackupError::Archive(_)), "got {err:?}");
        assert!(
            std::fs::read_dir(dst.base_dir()).unwrap().next().is_none(),
            "a refused bomb must not leave a half-materialized tree"
        );
    }

    // -- QURATOR-235: semantic identity validation before the commit ----------------------------

    #[test]
    fn validate_inner_rejects_semantically_invalid_identity() {
        // A JSON-valid but garbage identity must fail the PRE-WIPE gate: the semantic check
        // (`AppIdentity::from_stored`) runs inside the shared extraction core, not only in
        // `restore_data` after `commit_stage` has already replaced the wiped profile with
        // attacker files and a broken identity.
        //
        // MUTATION (must redden): delete the `AppIdentity::from_stored` check in
        // `extract_archive`'s identity arm — the JSON parses fine, `save_identity` accepts
        // anything, and validation returns `Ok(())`.
        let stored = StoredIdentity {
            version: 1,
            nsec: "garbage".into(),
            browse_key_hex: "00".repeat(32),
            transport_secret_hex: "00".repeat(32),
        };
        let json = serde_json::to_vec(&stored).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        append_bytes(&mut b, IDENTITY_ENTRY, &json).unwrap();
        let archive = encrypt_backup(BackupMode::Plaintext, &b.into_inner().unwrap()).unwrap();

        assert!(validate_inner(&archive, None).is_err());
        let (_d, dst) = empty_store();
        assert!(restore_inner(&dst, &archive, None).is_err());
        assert!(
            std::fs::read_dir(dst.base_dir()).unwrap().next().is_none(),
            "a semantically-invalid identity must not be committed"
        );
    }
}
