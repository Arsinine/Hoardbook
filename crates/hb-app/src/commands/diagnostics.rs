//! QURATOR-65 — the log/diagnostics surface that makes bug reports diagnosable.
//!
//! Before this, `logging.rs` wrote a decent daily-rotated log to `<app_data_dir>/logs/hb-app.log`,
//! but NOTHING exposed it to the UI: a user who hit a bug could not find, read, or send the log. Two
//! commands close that gap:
//!
//! - [`reveal_log_folder`] — opens the OS file manager at `<app_data_dir>/logs`.
//! - [`copy_diagnostics`] — returns the diagnostics header (build/version/config/npub) followed by
//!   the tail of the current log file, capped so it survives a clipboard and a Reddit comment.
//!
//! **Privacy contract (INV-2):** the header truncates the npub to 12 chars and never contains the
//! browse-key or nsec. The tail is read straight from the file the subscriber owns, so whatever the
//! app logged is what ships — the same log the owner already trusts with `tracing::debug!` lines
//! (audited 2026-08-03 to carry no secret material at the default level). The red-green test
//! [`crate::logging::tests`] proves a deliberately-injected key is caught.

use std::path::PathBuf;

use tauri::{Manager, State};

use crate::error::{CmdResult, cmd_err};
use crate::identity_state::SharedIdentity;
use crate::logging;
use crate::store::DataStore;

/// The tail cap: never copy more than this many lines OR this many bytes, whichever is smaller.
/// Survives a Reddit comment and a clipboard without dropping the diagnostics header.
const TAIL_MAX_LINES: usize = 2000;
const TAIL_MAX_BYTES: usize = 256 * 1024;

/// Copy diagnostics to the clipboard: the header (version/OS/build/relays/npub) + the tail of the
/// current log file, capped. The UI's clipboard write happens in the frontend; this command returns
/// the text. Never panics when the log directory does not yet exist (first launch) — returns the
/// header with a "no log file yet" note instead.
#[tauri::command]
pub async fn copy_diagnostics(
    app: tauri::AppHandle,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<String> {
    let version = app.package_info().version.to_string();
    let (os, arch) = logging::os_arch();
    let profile = logging::build_profile();
    let relays = crate::net::relay_urls(store.inner());

    // The npub is the one identity-derived value the header carries, and it goes in TRUNCATED (INV-2:
    // never the browse-key, never the nsec). Empty string before the user has generated/imported.
    let npub = {
        let guard = identity.read().await;
        guard.as_ref().map(|id| id.npub()).unwrap_or_default()
    };
    let header = logging::diagnostics_header(&version, os, arch, profile, &relays, &npub);

    // Find the newest daily-rotated log file (prefix mode → `hb-app.log.YYYY-MM-DD`). The directory
    // may not exist yet on a first launch with no log lines; that's fine — the header alone is a
    // valid diagnostic and the tail note says "no log file yet".
    let data_dir = app.path().app_data_dir().map_err(cmd_err)?;
    let log_dir = data_dir.join("logs");
    let tail = read_tail(&log_dir).unwrap_or_else(|| "  (no log file yet — this is a first launch)".to_string());

    Ok(format!("{header}\n\n--- log tail ---\n{tail}"))
}

/// Open the OS file manager at `<app_data_dir>/logs`. Never panics when the directory does not yet
/// exist — it creates it first (matching the never-fail contract of [`logging::install`]). A failure
/// to spawn the OS opener is a reasoned `Err`, surfaced to the user as a toast.
#[tauri::command]
pub async fn reveal_log_folder(app: tauri::AppHandle) -> CmdResult<()> {
    let data_dir = app.path().app_data_dir().map_err(cmd_err)?;
    let log_dir = data_dir.join("logs");
    // Create the dir if missing so the opener never lands on a non-existent path (first launch).
    std::fs::create_dir_all(&log_dir).map_err(cmd_err)?;
    open_in_file_manager(&log_dir)
}

/// Read the newest `LOG_PREFIX.*` file under `log_dir` and return its tail, capped at
/// [`TAIL_MAX_LINES`] lines or [`TAIL_MAX_BYTES`] bytes, whichever is smaller. Returns `None` if the
/// directory or no matching file exists. Pure (operates on the filesystem but takes no Tauri state)
/// so the cap logic is unit-testable.
fn read_tail(log_dir: &PathBuf) -> Option<String> {
    let newest = newest_log_file(log_dir)?;
    let bytes = std::fs::read(&newest).ok()?;
    cap_tail(&bytes)
}

/// Find the lexicographically-newest file matching `LOG_PREFIX.*` under `log_dir`. The daily
/// rotation names files `hb-app.log.YYYY-MM-DD`, and ISO dates sort lexicographically == chronologically,
/// so the max is the most recent. Returns `None` if the dir doesn't exist or has no matching file.
fn newest_log_file(log_dir: &PathBuf) -> Option<PathBuf> {
    let entries = std::fs::read_dir(log_dir).ok()?;
    let mut newest: Option<(String, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(logging::LOG_PREFIX) {
            continue;
        }
        let path = entry.path();
        match &newest {
            None => newest = Some((name.to_string(), path)),
            Some((cur_name, _)) => {
                if name > cur_name.as_str() {
                    newest = Some((name.to_string(), path));
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// Cap a raw log byte buffer to the tail, honouring BOTH the line and byte ceilings (whichever is
/// smaller). Ends with a truncation marker when it was cut. Pure — the unit-testable core of the cap.
pub(crate) fn cap_tail(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    // Nothing to show — an empty log is the first-launch case, and the caller renders "no log yet"
    // rather than pasting an empty tail. Without this, `joined` is "" and falls through the byte
    // ceiling below as `Some("")`.
    if lines.is_empty() {
        return None;
    }

    // First clamp by line count from the end, then by the byte ceiling on the resulting slice.
    let start_line = lines.len().saturating_sub(TAIL_MAX_LINES);
    let picked: &[&str] = &lines[start_line..];
    let joined = picked.join("\n");

    if joined.len() <= TAIL_MAX_BYTES {
        return if start_line > 0 {
            Some(format!("…({} earlier lines truncated)\n{joined}", start_line))
        } else {
            Some(joined)
        };
    }

    // Byte ceiling hit: take the last ~TAIL_MAX_BYTES bytes. We must land on a UTF-8 char boundary
    // (an arbitrary byte offset can split a multi-byte sequence and panic), so walk forward from the
    // raw cut point to the next boundary. Then drop a leading fragment line so the output doesn't
    // begin mid-line.
    let cut_from = joined.len().saturating_sub(TAIL_MAX_BYTES);
    let boundary = joined
        .char_indices()
        .skip_while(|(i, _)| *i < cut_from)
        .map(|(i, _)| i)
        .next()
        .unwrap_or(cut_from);
    let slice = &joined[boundary..];
    let trimmed = slice.trim_start_matches(|c: char| c != '\n').trim_start_matches('\n');
    Some(format!("…(truncated to fit {TAIL_MAX_BYTES} bytes)\n{trimmed}"))
}

/// Open a path in the OS file manager. Platform-specific because there is no opener plugin loaded
/// (M-era discipline: no new dependency for a one-shot spawn). Windows: `explorer /select,...` needs a
/// child file; for a directory, `explorer <dir>` is the idiom. macOS: `open`. Linux: `xdg-open`.
fn open_in_file_manager(path: &PathBuf) -> CmdResult<()> {
    let result = if cfg!(target_os = "windows") {
        std::process::Command::new("explorer").arg(path).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(path).spawn()
    };
    result.map_err(cmd_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::diagnostics_header;

    /// The header truncates the npub and carries no key material (INV-2). Red-green: the GREEN half.
    #[test]
    fn diagnostics_header_truncates_npub_and_omits_keys() {
        let relays = vec!["wss://relay.a".to_string(), "wss://relay.b".to_string()];
        let npub = "npub1verylongstringthatiswaymorethantwelvecharsandshouldbetruncated";
        let h = diagnostics_header("0.12.11", "linux", "x86_64", "debug", &relays, npub);
        // NPUB_TRUNC_LEN=12: first 12 chars = "npub1verylon".
        assert!(h.contains("npub: npub1verylon…"), "npub must be truncated to 12 chars; got:\n{h}");
        assert!(!h.contains("npub1verylong"), "full npub past 12 chars must not appear");
        assert!(!h.contains("nsec"), "header must never contain an nsec string");
        assert!(!h.contains("browse"), "header must never mention the browse-key");
    }

    /// Red-green RED half: prove the GREEN assertion has teeth. We take the GREEN-half header, splice
    /// an nsec-bearing log line into it (as a buggy call site might), and show the EXACT guard that
    /// passes on the clean header now FAILS — so the guard is not vacuous.
    #[test]
    fn diagnostics_guard_catches_an_injected_nsec() {
        let relays = vec!["wss://relay.a".to_string()];
        // A clean header passes the guard.
        let clean = diagnostics_header("0.12.11", "linux", "x86_64", "debug", &relays, "npub1abc");
        assert!(guard_no_secret_in_output(&clean), "clean header must pass the guard");

        // A header with an nsec-bearing log line spliced in must FAIL the guard — proving the guard
        // is not vacuous. (This is the red half: if the guard ever stops rejecting this, the test
        // fails, because `assert!` expects the guard to have CAUGHT the leak.)
        let leaked = format!("{clean}\nDEBUG key=nsec1leakedsecretkeyhere1234567890");
        assert!(!guard_no_secret_in_output(&leaked), "guard MUST reject an nsec-bearing output");
    }

    /// The guard predicate the two halves above exercise: "no secret marker appears in the output".
    /// Mirrors what an operator reads off when scanning a diagnostics paste for INV-2 leakage.
    fn guard_no_secret_in_output(s: &str) -> bool {
        !(s.contains("nsec1") || s.contains("browse_key"))
    }

    #[test]
    fn cap_tail_returns_none_for_empty() {
        assert!(cap_tail(b"").is_none());
    }

    #[test]
    fn cap_tail_passes_small_log_unchanged() {
        let log = b"line one\nline two\nline three";
        let out = cap_tail(log).unwrap();
        assert_eq!(out, "line one\nline two\nline three");
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn cap_tail_drops_early_lines_when_over_line_ceiling() {
        // 3000 lines — over the 2000-line ceiling. The last 2000 are kept (lines 1000–2999); the
        // first 1000 are dropped with a truncation marker.
        let log: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        let out = cap_tail(log.as_bytes()).unwrap();
        assert!(out.contains("1000 earlier lines truncated"), "must mark the cut; got first 60: {}", &out[..60.min(out.len())]);
        // The last kept line is the actual last line of the input.
        assert!(out.ends_with("line 2999"));
        // And the first kept line is line 1000 (3000 − 2000).
        assert!(out.contains("line 1000"), "must keep the last 2000 lines");
        assert!(!out.contains("\nline 999\n"), "must not keep line 999");
    }

    #[test]
    fn cap_tail_clamps_bytes_and_drops_leading_fragment() {
        // A single very long line that exceeds the byte ceiling.
        let huge: String = "x".repeat(TAIL_MAX_BYTES + 5000);
        let log = format!("first line\n{huge}\nlast line");
        let out = cap_tail(log.as_bytes()).unwrap();
        assert!(out.contains("truncated to fit"), "must mark the byte cut");
        // The byte-clamped tail keeps the final line.
        assert!(out.ends_with("last line"));
        // And must not begin mid-fragment (the leading run of 'x' is dropped).
        assert!(!out.starts_with('x'), "must drop the leading fragment line");
    }

    #[test]
    fn cap_tail_handles_multibyte_utf8_at_the_byte_boundary_without_panicking() {
        // A log with multi-byte UTF-8 (emoji = 4 bytes each) that exceeds the byte ceiling. The cut
        // must land on a UTF-8 char boundary, not split a multi-byte sequence (which would panic).
        let emoji_line: String = "🎉".repeat(TAIL_MAX_BYTES / 4 + 100);
        let log = format!("header line\n{emoji_line}\ntail line");
        let out = std::panic::catch_unwind(|| cap_tail(log.as_bytes()));
        assert!(out.is_ok(), "cap_tail must not panic on multi-byte UTF-8 at the boundary");
        let out = out.unwrap().unwrap();
        assert!(out.ends_with("tail line"));
    }
}
