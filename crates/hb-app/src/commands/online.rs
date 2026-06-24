//! The "🟢 N online" chip's backend (M9, Track C; spec §Privacy Model → Userbase metrics).
//!
//! `online_count` answers "how many hoarders are online now" with **no telemetry** — it is a *read*
//! of fresh presence events off the relays (`hb_net::count_online` → the sig-verified, canary-
//! excluded, distinct-`npub` tally). It is **best-effort and cached**: the command returns the last
//! cached value immediately and kicks off an async refresh only when the cache is stale, so it never
//! blocks startup or any user action, and a **bounded slow tick** (the cache `REFRESH_INTERVAL`)
//! keeps it from becoming the CPU/network drain L4 exists to catch.
//!
//! **m4 — zero-relay / empty-cache fallback.** On a fresh launch with no cached value *and* no
//! reachable relay, `online` is `None`; the chip renders "–" (or hides) — never a misleading
//! "0 online" or a blocking spinner.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::State;
use tokio::sync::RwLock;

use crate::error::CmdResult;
use crate::identity_state::SharedIdentity;
use crate::net::{self, SharedRelay};
use crate::store::DataStore;

/// Online freshness window (Decision #12 / Open Q#6 — the same 10 min the contact-list `● Online`
/// badge uses; confirm at launch).
pub const ONLINE_WINDOW_SECS: u64 = 600;

/// The bounded slow tick: the relay is queried at most once per this interval no matter how often
/// the chip polls the command (so the count can't become a drain — it is profiled by L4).
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The chip's data: `online = None` means "unknown" (no cache yet and no reachable relay) — render
/// "–" / hide, never "0". Presented as an **estimate per relay-set**, never an authoritative global.
#[derive(Debug, Clone, Serialize)]
pub struct OnlineCount {
    pub online: Option<usize>,
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    pub relay_set: Vec<String>,
}

/// Last-known count + when we last *attempted* a refresh (drives the slow-tick throttle).
#[derive(Default)]
pub struct OnlineCache {
    value: Option<OnlineCount>,
    last_attempt: Option<Instant>,
}

pub type SharedOnlineCache = Arc<RwLock<OnlineCache>>;

/// Whether the cache is stale enough to attempt a refresh. Pure (no clock capture beyond the passed
/// reference) so the slow-tick throttle is unit-testable.
fn is_stale(last_attempt: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last_attempt.map_or(true, |t| now.saturating_duration_since(t) >= interval)
}

/// Apply a refresh outcome to the cache (Decision C — **no sticky "–"**). A success replaces the
/// value (count + `fetched_at`); a **failure leaves the last-known value untouched** — it never
/// reverts a known count to `None` ("–") after one transient relay error, and a later success
/// recovers it. A first-ever failure (no prior value) stays `None` → the chip honestly shows "–"
/// (unknown, not a misleading "0"). Pure, so RELAY3 is a differential unit test with no relay.
fn apply_refresh(
    cache: &mut OnlineCache,
    result: Result<usize, ()>,
    relay_set: Vec<String>,
    now: chrono::DateTime<chrono::Utc>,
) {
    if let Ok(n) = result {
        cache.value = Some(OnlineCount { online: Some(n), fetched_at: Some(now), relay_set });
    }
    // On failure: keep cache.value as-is (last-known stays; never-fetched stays None).
}

/// Refresh the cached count: query fresh presence off the **persistent shared** relay client and
/// tally distinct online `npub`s. The caller has **already** marked `last_attempt` atomically (so
/// exactly one refresh is in flight per interval — see `online_count`); this just does the query.
/// On any failure the cached value is left untouched (Decision C / [`apply_refresh`]).
async fn refresh_count(
    store: &DataStore,
    identity: &SharedIdentity,
    relay: &SharedRelay,
    cache: &SharedOnlineCache,
) {
    let snapshot = {
        let guard = identity.read().await;
        guard.as_ref().map(|app| app.identity.clone())
    };
    let Some(id) = snapshot else { return };
    let relay_set = net::relay_urls(store);

    let result: Result<usize, ()> = match net::client(&id, store, relay).await {
        Ok(client) => hb_net::count_online(&client, ONLINE_WINDOW_SECS, net::RELAY_TIMEOUT)
            .await
            .map_err(|_| ()),
        Err(_) => Err(()),
    };

    let mut c = cache.write().await;
    apply_refresh(&mut c, result, relay_set, chrono::Utc::now());
}

/// Return the cached online count immediately; if the cache is stale, kick off an async refresh
/// (fire-and-forget) whose result the *next* poll picks up. Never blocks on the network.
///
/// The staleness check **and** the `last_attempt` mark happen in **one** write-lock critical section,
/// so two concurrent callers can't both observe "stale" and both fan out a relay query (the
/// check-then-spawn TOCTOU). Exactly one refresh runs per `REFRESH_INTERVAL`, which also removes the
/// last-write-wins race on `cache.value` between two overlapping refreshes (chorus: Codex/Gemini/opencode).
#[tauri::command]
pub async fn online_count(
    store: State<'_, DataStore>,
    identity: State<'_, SharedIdentity>,
    relay: State<'_, SharedRelay>,
    cache: State<'_, SharedOnlineCache>,
) -> CmdResult<OnlineCount> {
    let relay_set = net::relay_urls(store.inner());
    let (cached, should_refresh) = {
        let mut c = cache.write().await;
        let cached = c.value.clone();
        let refresh = is_stale(c.last_attempt, Instant::now(), REFRESH_INTERVAL);
        if refresh {
            c.last_attempt = Some(Instant::now()); // claim the slot before releasing the lock
        }
        (cached, refresh)
    };

    if should_refresh {
        let store = store.inner().clone();
        let identity = Arc::clone(identity.inner());
        let relay = Arc::clone(relay.inner());
        let cache = Arc::clone(cache.inner());
        tauri::async_runtime::spawn(async move {
            refresh_count(&store, &identity, &relay, &cache).await;
        });
    }

    // No cache yet → unknown (m4): online = None, chip shows "–" / hides.
    Ok(cached.unwrap_or(OnlineCount { online: None, fetched_at: None, relay_set }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stale_true_when_never_attempted() {
        // A fresh process (no attempt yet) is always stale → the first poll triggers a refresh.
        assert!(is_stale(None, Instant::now(), REFRESH_INTERVAL));
    }

    #[test]
    fn is_stale_false_within_interval_true_after() {
        let now = Instant::now();
        let recent = now - Duration::from_secs(10);
        assert!(!is_stale(Some(recent), now, REFRESH_INTERVAL), "10s < 60s → not stale (no re-query)");
        let old = now - Duration::from_secs(61);
        assert!(is_stale(Some(old), now, REFRESH_INTERVAL), "61s > 60s → stale (slow-tick re-query)");
    }

    #[test]
    fn online_count_shape_supports_unknown_fallback() {
        // The m4 contract: `online` is an Option so the chip can render "–" instead of a fake "0".
        let unknown = OnlineCount { online: None, fetched_at: None, relay_set: vec![] };
        let json = serde_json::to_string(&unknown).unwrap();
        assert!(json.contains("\"online\":null"), "unknown count serializes online=null: {json}");
    }

    #[test]
    fn refresh_failure_keeps_last_count_no_sticky_dash_relay3() {
        // RELAY3 / Decision C (differential, no relay): a fetch error after a prior success keeps the
        // last-known count (NOT "–"); a later success updates it; a first-ever failure stays unknown.
        let now = chrono::Utc::now();
        let relays = vec!["wss://r".to_string()];
        let mut cache = OnlineCache::default();

        // First-ever failure → still unknown (None → chip shows "–", honest, not a fake "0").
        apply_refresh(&mut cache, Err(()), relays.clone(), now);
        assert!(cache.value.is_none(), "a first-ever failure stays unknown (–), not 0");

        // A success populates the count.
        apply_refresh(&mut cache, Ok(5), relays.clone(), now);
        assert_eq!(cache.value.as_ref().unwrap().online, Some(5));

        // A transient failure AFTER a success must NOT revert to "–" — it keeps the last count.
        apply_refresh(&mut cache, Err(()), relays.clone(), now);
        assert_eq!(cache.value.as_ref().unwrap().online, Some(5), "no sticky –: last count survives a failed cycle");

        // A later success recovers/updates it.
        apply_refresh(&mut cache, Ok(7), relays, now);
        assert_eq!(cache.value.as_ref().unwrap().online, Some(7));
    }
}
