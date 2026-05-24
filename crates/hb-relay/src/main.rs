#![forbid(unsafe_code)]

mod db;
mod error;
mod handlers;
mod state;

use anyhow::Context;
use axum::{Router, routing::{get, post}};
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://hb-relay.db".into());

    let bind_addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".into())
        .parse()
        .context("invalid BIND_ADDR")?;

    // Comma-separated list of peer relay URLs advertised in /v1/health.
    let peer_relays: Vec<String> = std::env::var("PEER_RELAYS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let pool = db::connect(&database_url)
        .await
        .context("failed to open database")?;

    db::migrate(&pool).await.context("migration failed")?;

    let state = AppState {
        pool: pool.clone(),
        // 30 publish/heartbeat requests per IP per minute.
        rate_limiter: state::RateLimiter::new(30, std::time::Duration::from_secs(60)),
        peer_relays,
    };

    // Background task: expire stale messages every hour. Heartbeat rows never expire.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                if let Err(e) = db::expire_messages(&pool).await {
                    tracing::warn!("expiry task error: {e}");
                }
            }
        });
    }

    let app = Router::new()
        .route("/v1/publish",          post(handlers::publish))
        .route("/v1/heartbeat",        post(handlers::heartbeat))
        .route("/v1/peer/:pubkey",     get(handlers::get_peer))
        .route("/v1/messages/:pubkey", get(handlers::get_messages))
        .route("/v1/health",           get(handlers::health))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    tracing::info!("hb-relay listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}
