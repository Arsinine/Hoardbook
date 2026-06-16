//! `hb-p2p-it` — headless P2P integration harness binary.
//! Thin wrapper around `hb_app::run_p2p_it`; all logic lives in `hb-app/src/p2p_it.rs`.

#[tokio::main]
async fn main() -> std::process::ExitCode {
    hb_app::run_p2p_it().await
}
