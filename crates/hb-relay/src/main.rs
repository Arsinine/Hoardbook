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

    let max_collection_bytes: usize = std::env::var("MAX_COLLECTION_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5 * 1024 * 1024); // 5 MB default

    let pool = db::connect(&database_url)
        .await
        .context("failed to open database")?;

    db::migrate(&pool).await.context("migration failed")?;

    let state = AppState {
        pool: pool.clone(),
        max_collection_bytes,
        // 30 publish/heartbeat requests per IP per minute.
        rate_limiter: state::RateLimiter::new(30, std::time::Duration::from_secs(60)),
    };

    // Background task: expire stale documents every hour.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                if let Err(e) = db::expire_documents(&pool).await {
                    tracing::warn!("expiry task error: {e}");
                }
            }
        });
    }

    let app = Router::new()
        .route("/v1/publish",                post(handlers::publish))
        .route("/v1/heartbeat",              post(handlers::heartbeat))
        .route("/v1/peer/:pubkey",           get(handlers::get_peer))
        .route("/v1/messages/:pubkey",       get(handlers::get_messages))
        .route("/v1/directory",              get(handlers::get_directory))
        .route("/v1/channel/:channel",       get(handlers::get_channel).post(handlers::post_channel))
        .route("/v1/name/:display_name",     get(handlers::check_name))
        .route("/v1/health",                 get(handlers::health))
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
