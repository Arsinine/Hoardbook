use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Simple sliding-window rate limiter: max N requests per IP per window.
#[derive(Clone)]
pub struct RateLimiter {
    /// Map from IP string → (window_start, request_count)
    state: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
    pub max_per_window: u32,
    pub window: Duration,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            max_per_window,
            window,
        }
    }

    /// Drop entries whose window has fully elapsed. Called periodically so the map
    /// can't grow without bound from IP rotation (M5).
    pub fn sweep(&self) {
        let mut map = self.state.lock().unwrap();
        let now = Instant::now();
        let window = self.window;
        map.retain(|_, (start, _)| now.duration_since(*start) < window);
    }

    /// Returns `true` if the request should be allowed, `false` if rate-limited.
    pub fn check(&self, ip: &str) -> bool {
        let mut map = self.state.lock().unwrap();
        let now = Instant::now();
        // Opportunistic eviction: bound memory even between periodic sweeps.
        if map.len() > 10_000 {
            let window = self.window;
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = map.entry(ip.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 1);
            true
        } else if entry.1 < self.max_per_window {
            entry.1 += 1;
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    /// Rate limiter for publish + heartbeat endpoints.
    pub rate_limiter: RateLimiter,
    /// Known peer relay URLs advertised in GET /v1/health (seeded from PEER_RELAYS env var).
    pub peer_relays: Vec<String>,
}
