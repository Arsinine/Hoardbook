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
