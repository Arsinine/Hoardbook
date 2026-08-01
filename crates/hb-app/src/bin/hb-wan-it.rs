//! `hb-wan-it` — headless WAN integration harness binary (M20 W6, Suite WAN-P).
//! Thin wrapper around `hb_app::run_wan_it`; all logic lives in `hb-app/src/wan_it/`.

#[tokio::main]
async fn main() -> std::process::ExitCode {
    hb_app::run_wan_it().await
}
