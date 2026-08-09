//! v0.12.10 diagnostic build — file-based tracing subscriber.
//!
//! The shipped Windows build has no tracing subscriber (every `tracing::debug!` is a no-op) and no
//! devtools, so the fifth presence-class bug — `run_presence_loop` never records a cycle on two
//! packaged-Windows machines running v0.12.9 — has no evidence trail. This installs a
//! `tracing_subscriber` writing to `<app_data_dir>/logs/hb-app.log` (rolling daily, 3 files kept).
//!
//! **Non-fatal by construction**: any failure in appender build or subscriber init logs to stderr
//! best-effort and returns `Ok(())`. The app must start even if the log dir is unwritable — a dead
//! log subscriber is strictly better than a dead app, and the beacon-panel breadcrumbs
//! (`BeaconReport::stage`) are the primary instrument, not these logs.
//!
//! Default filter `hb_app=debug,hb_net=debug,info` is overridable via the `HB_LOG` env var.
//! **No secret material is logged at the default level** (secrets audit 2026-08-03 — see the
//! presence.rs / chat.rs / client.rs `tracing::debug!` sites: every one logs an error message,
//! a relay URL, or a sleep duration, never an nsec / DM plaintext / sealed payload).

use std::path::Path;

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// The diagnostic header's privacy contract (INV-2): the npub is TRUNCATED to its first 12 chars,
/// and neither the browse-key nor the nsec/private key ever appears. One paste must answer "what
/// build, what config, which machine" — and nothing more. See `diagnostics_header` and the
/// `header_never_contains_the_browse_key_or_nsec` red-green test.
pub(crate) const NPUB_TRUNC_LEN: usize = 12;

/// Build the diagnostics header: the lines that appear at the top of every freshly-rotated log file
/// AND at the top of the "Copy diagnostics" clipboard output. Pure — takes already-resolved strings
/// so the caller owns the data-dir / identity reads, and the privacy contract is unit-testable
/// without a Tauri runtime.
///
/// `npub` is the full bech32 npub; it is truncated here to [`NPUB_TRUNC_LEN`] chars. `relays` is the
/// effective configured-or-default set (what the app actually dials). Never logs the browse-key,
/// nsec, DM plaintext, or private-collection contents.
pub(crate) fn diagnostics_header(
    version: &str,
    os: &str,
    arch: &str,
    profile: &str,
    relays: &[String],
    npub: &str,
) -> String {
    let npub_disp: String = npub.chars().take(NPUB_TRUNC_LEN).collect();
    let relays_line = if relays.is_empty() { "(none)".to_string() } else { relays.join(", ") };
    format!(
        "=== Hoardbook diagnostics ===\n\
         version: {version}\n\
         os: {os} {arch}\n\
         build: {profile}\n\
         relays: {relays_line}\n\
         npub: {npub_disp}…\n\
         === end diagnostics ==="
    )
}

/// The build profile label for the diagnostics header. `debug` under `debug_assertions`, else
/// `release` (the shipped package build). Pure — testable without a runtime.
pub(crate) fn build_profile() -> &'static str {
    if cfg!(debug_assertions) { "debug" } else { "release" }
}

/// The OS + arch pair the header reports (`std::env::consts`, resolved by the caller so the pure
/// header builder stays platform-stable in snapshot tests).
pub(crate) fn os_arch() -> (&'static str, &'static str) {
    (std::env::consts::OS, std::env::consts::ARCH)
}

/// The daily-rotated log file prefix (matches the `RollingFileAppender` `filename_prefix` below) —
/// exposed so the copy-diagnostics command can locate the newest file without re-deriving the name.
pub(crate) const LOG_PREFIX: &str = "hb-app.log";

/// Install the file-based subscriber writing under `<app_data_dir>/logs/`. Never panics — a
/// failure returns silently (stderr gets a best-effort line) so the app starts regardless.
pub(crate) fn install(data_dir: &Path) {
    // v0.12.11: route panics through tracing into the log file. The 2026-08-03 root-cause hunt
    // established that a panicking background task is INVISIBLE in the packaged app — no console
    // for stderr, and panics bypass tracing — so the presence loop died silently inside
    // `RelayClient::connect` across five reports. The default stderr hook still runs after ours.
    // Installed even if the subscriber below fails: tracing::error! is then a no-op and the
    // stderr hook alone remains (the pre-v0.12.10 state). NB: a panic that aborts the process
    // may lose this line (the non-blocking appender flushes asynchronously); an unwinding task
    // panic — the observed class — flushes fine because the process lives on.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("PANIC: {info}\n{backtrace}");
        prev_hook(info);
    }));

    let filter = EnvFilter::try_from_env("HB_LOG")
        .unwrap_or_else(|_| EnvFilter::new("hb_app=debug,hb_net=debug,info"));

    let log_dir = data_dir.join("logs");
    // The appender builder's `max_log_files(3)` caps retention to 3 rolled files; a failure here
    // (e.g. read-only fs) drops to stderr rather than aborting startup.
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("hb-app.log")
            .max_log_files(3)
            .build(&log_dir)?;
        let (non_blocking, _guard) = tracing_appender::non_blocking(appender);
        // The guard must outlive the subscriber; leak it so the appender never flushes-and-closes
        // mid-run. The subscriber itself holds the writer, so this is a process-lifetime allocation
        // either way — a deliberate, bounded one (one handle, one app).
        std::mem::forget(_guard);

        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(non_blocking));
        // try_init, not init: init() panics if a global subscriber is already set (e.g. a second
        // install() call, or a test harness that set one), which would violate the never-fail
        // contract above.
        subscriber.try_init()?;
        Ok(())
    })();

    if let Err(e) = result {
        // Best-effort stderr line; never panics. The subscriber may be unset, in which case tracing
        // events simply go nowhere — the same state the shipped app has always had.
        eprintln!("hb-app: file logging unavailable ({e}); continuing without a subscriber");
    }
}

/// Write the diagnostics header as the first lines of the current log file. Called once on startup,
/// after the subscriber is installed and the identity/relay set are resolved (so the npub and relays
/// are the real values, not pre-load defaults). Emits via `tracing::info!`, so it lands in the same
/// file the subscriber owns and respects the non-blocking flush — a no-op if the subscriber failed
/// to install. On each launch the header is the first content written to the current day's file; the
/// copy button ([`crate::commands::diagnostics::copy_diagnostics`]) rebuilds it from the same pure
/// builder so the two stay in sync.
pub(crate) fn write_startup_header(version: &str, relays: &[String], npub: &str) {
    let (os, arch) = os_arch();
    let profile = build_profile();
    let header = diagnostics_header(version, os, arch, profile, relays, npub);
    // One multi-line info! so the header lands as a contiguous block at the file top, not interleaved
    // with the background tasks' debug! lines.
    tracing::info!("{header}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_truncates_npub_to_twelve_chars() {
        let relays = vec!["wss://relay.example".to_string()];
        let npub = "npub1abcdefghijklmnop";
        let h = diagnostics_header("0.12.11", "linux", "x86_64", "debug", &relays, npub);
        // NPUB_TRUNC_LEN=12: first 12 chars of "npub1abcdefghijklmnop" = "npub1abcdefg".
        assert!(h.contains("npub: npub1abcdefg…"), "expected 12-char truncation; got:\n{h}");
        assert!(!h.contains("npub1abcdefgh"), "full npub past 12 chars must not appear");
    }

    #[test]
    fn header_lists_relays_and_version() {
        let relays = vec!["wss://a.example".to_string(), "wss://b.example".to_string()];
        let h = diagnostics_header("1.2.3", "macos", "aarch64", "release", &relays, "npub1xyz");
        assert!(h.contains("version: 1.2.3"));
        assert!(h.contains("os: macos aarch64"));
        assert!(h.contains("build: release"));
        assert!(h.contains("relays: wss://a.example, wss://b.example"));
    }

    #[test]
    fn header_shows_none_for_empty_relay_set() {
        let h = diagnostics_header("1.0.0", "linux", "x86_64", "debug", &[], "npub1abc");
        assert!(h.contains("relays: (none)"));
    }

    #[test]
    fn header_never_contains_the_browse_key_or_nsec() {
        // INV-2 red-green: the header builder takes only an npub (truncated) and never sees the
        // browse-key or nsec. This test pins that — if a future change passes a key in, it fails.
        let h = diagnostics_header("0.12.11", "linux", "x86_64", "debug", &[], "npub1abc");
        assert!(!h.contains("nsec"), "header must never contain an nsec string");
        assert!(!h.contains("browse_key"), "header must never contain the browse-key");
    }
}
