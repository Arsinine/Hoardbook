//! `hb-wan-it` — headless WAN integration harness binary (M20 W6, Suite WAN-P).
//! Thin wrapper around `hb_app::run_wan_it`; all logic lives in `hb-app/src/wan_it/`.

use std::time::Duration;

fn main() -> std::process::ExitCode {
    // Manual runtime instead of #[tokio::main] so the shutdown can be BOUNDED (QURATOR-318): the
    // iroh endpoints the WAN rows bind leave background tasks (relay keepalives) behind, and an
    // unbounded runtime drop can block process exit on them. shutdown_timeout abandons any
    // still-running work after the timeout instead of waiting forever, so the harness always exits
    // with the exit code `run_wan_it` returned. The canary --once path additionally closes its
    // iroh endpoints at the source (wan_it/mod.rs), so the normal case never needs the fallback.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("hb-wan-it: failed to build the tokio runtime");
    let code = rt.block_on(hb_app::run_wan_it());
    rt.shutdown_timeout(Duration::from_secs(5));
    code
}
