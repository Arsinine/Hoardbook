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

use nostr::prelude::FromBech32;
use serde::Serialize;
use tauri::State;
use tokio::sync::RwLock;

use crate::error::CmdResult;
use crate::identity_state::SharedIdentity;
use crate::net::{self, SharedRelay};
use crate::store::DataStore;

/// Online freshness window — a beacon older than this stops counting as evidence of liveness.
///
/// **480 s, owner ruling 2026-08-27** (devtest 2026-08-26 item 1: "the online indicator takes too
/// long to prune people who havent been on. It should only take heartbeat messages from the last
/// 5-10 minutes as evidence of liveness"). It was 600 s — the top of that range.
///
/// This is NOT a free knob: it is bounded below by [`crate::presence::PRESENCE_REFRESH_SECS`]
/// (300 s), the cadence at which we republish our own beacon. A window at or near the cadence has
/// no slack, so ONE late or failed publish flips a genuinely-online peer to Offline. 480 s leaves
/// 180 s of slack, which covers the first three steps of the failed-cycle retry backoff
/// (`RETRY_BACKOFF_SECS = [15, 30, 60, 120]` — 15+30+60 = 105 s) with room to spare, so a transient
/// relay flap still self-heals before the peer reads offline. Going lower means lowering the beacon
/// cadence too, which doubles beacon writes against the relays.
///
/// Two copies track this value and must move with it — the UI's `PRESENCE_WINDOW_MS`
/// (hb-app/ui/src/lib/presence-view.ts) and hb-it's harness constant. Both are pinned by tests that
/// PARSE this line rather than restate the number, so a one-sided edit reds.
pub const ONLINE_WINDOW_SECS: u64 = 480;

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
    /// **W1 (2026-08-02)** — who, of our stored contacts, we just saw, and when. This comes from an
    /// **author-filtered** read (`hb_net::fetch_presence_for_authors`) over the contact list, NOT
    /// from the chip's global aggregate: a contact's beacon can never be displaced by the relay's
    /// global response cap. `online` is the global count; `fresh` is the contacts-only map — they
    /// are independent. Empty when the read failed or there are no contacts.
    #[serde(default)]
    pub fresh: Vec<PresenceSeen>,
}

/// One "we saw this npub's presence beacon at this time" pair from the online poll.
#[derive(Debug, Clone, Serialize)]
pub struct PresenceSeen {
    pub npub: String,
    pub seen_at: chrono::DateTime<chrono::Utc>,
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
    last_attempt.is_none_or(|t| now.saturating_duration_since(t) >= interval)
}

/// Apply a refresh outcome to the cache (Decision C — **no sticky "–"**). The chip (`count`) and the
/// per-contact pills (`pills`) are refreshed **independently**: each is `Some` only when *its own*
/// read succeeded this cycle, and a failed half keeps its last-known value. So a transient chip-query
/// failure never discards freshly-read pills, and a pills-read failure (e.g. a store error reading
/// the contact list) never blanks the chip — nor, crucially, blanks every pill to a false "offline"
/// (the W1 bug class). A cycle in which **both** halves failed leaves the cache untouched; a
/// first-ever all-failure stays `None` → the chip honestly shows "–" (unknown, not a misleading
/// "0"). Pure, so RELAY3 is a differential unit test with no relay.
fn apply_refresh(
    cache: &mut OnlineCache,
    count: Option<usize>,
    pills: Option<Vec<PresenceSeen>>,
    relay_set: Vec<String>,
    now: chrono::DateTime<chrono::Utc>,
) {
    // Both halves failed → keep the cache as-is (last-known stays; never-fetched stays None).
    if count.is_none() && pills.is_none() {
        return;
    }
    // At least one half is fresh. Carry the last-known halves forward and overwrite only the ones
    // that succeeded this cycle — the chip and the pills degrade independently.
    let (prev_online, prev_fresh) = match cache.value.take() {
        Some(v) => (v.online, v.fresh),
        None => (None, Vec::new()),
    };
    cache.value = Some(OnlineCount {
        online: match count {
            Some(n) => Some(n),
            None => prev_online,
        },
        fetched_at: Some(now),
        relay_set,
        fresh: pills.unwrap_or(prev_fresh),
    });
}

/// Stamp `last_presence` on every stored contact we just saw a beacon from (W5.2). Local-only: the
/// pairs come from the poll that already ran, so this adds no network work — it just makes the age
/// survive a restart. A contact we did *not* see is left untouched: absence of a beacon in this
/// window is not evidence about when they were last around, and overwriting would erase the very
/// age we are trying to show. Best-effort — a store error is not worth failing a background poll.
fn persist_presence(store: &DataStore, fresh: &[PresenceSeen]) {
    for seen in fresh {
        let hash = crate::store::CachedPeer::pubkey_hash(&seen.npub);
        let Ok(Some(mut peer)) = store.load_contact(&hash) else { continue };
        // Never move the stamp backwards (a late-arriving older beacon must not age them up).
        if peer.last_presence.is_some_and(|prev| prev >= seen.seen_at) {
            continue;
        }
        peer.last_presence = Some(seen.seen_at);
        let _ = store.save_contact(&hash, &peer);
    }
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

    // W1: the per-contact pills come from an author-filtered read over the STORED contacts, immune
    // to the global relay cap. The chip's `online` stays the global count (an approximate userbase
    // metric). Two reads, two questions — decoupled, and they degrade independently below.
    //
    // A store error reading the contact list must NOT be read as "zero contacts": querying an empty
    // author set would return an empty map and blank every pill to a false "offline" — the exact W1
    // bug in a new place. On such an error we leave the pills half `None` (keep last-known) rather
    // than trust an empty list. (Known bound: a single author-filtered REQ is still subject to the
    // relay's own response cap, so a contact list larger than that cap — far past this phonebook's
    // scale — could omit the overflow; documented as a follow-up, not batched here.)
    let contacts = store.list_contacts();

    let (count_opt, pills_opt): (Option<usize>, Option<Vec<PresenceSeen>>) =
        match net::client(&id, store, relay).await {
            Ok(client) => {
                let count = hb_net::count_online(&client, ONLINE_WINDOW_SECS, net::RELAY_TIMEOUT)
                    .await
                    .ok();
                let pills = match &contacts {
                    Ok(cs) => {
                        let authors: Vec<nostr::PublicKey> = cs
                            .iter()
                            .filter_map(|c| nostr::PublicKey::from_bech32(&c.npub).ok())
                            .collect();
                        hb_net::fetch_presence_for_authors(
                            &client,
                            &authors,
                            ONLINE_WINDOW_SECS,
                            net::RELAY_TIMEOUT,
                        )
                        .await
                        .ok()
                        .map(|(map, _now)| to_presence_seen(map))
                    }
                    // Store error → keep last-known pills, never blank them to a false offline.
                    Err(_) => None,
                };
                (count, pills)
            }
            Err(_) => (None, None),
        };

    if let Some(fresh) = &pills_opt {
        persist_presence(store, fresh);
    }

    let mut c = cache.write().await;
    apply_refresh(&mut c, count_opt, pills_opt, relay_set, chrono::Utc::now());
}

/// Convert the pure tally's `PublicKey → created_at` map into the bech32-`npub` pairs the frontend
/// keys contacts by. An un-encodable key (impossible in practice) is dropped rather than faked.
fn to_presence_seen(map: std::collections::HashMap<nostr::PublicKey, u64>) -> Vec<PresenceSeen> {
    map.into_iter()
        .filter_map(|(pk, ts)| {
            let npub = nostr::prelude::ToBech32::to_bech32(&pk).ok()?;
            let seen_at = chrono::DateTime::from_timestamp(ts as i64, 0)?;
            Some(PresenceSeen { npub, seen_at })
        })
        .collect()
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
    Ok(cached.unwrap_or(OnlineCount {
        online: None,
        fetched_at: None,
        relay_set,
        fresh: vec![],
    }))
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
        let unknown =
            OnlineCount { online: None, fetched_at: None, relay_set: vec![], fresh: vec![] };
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
        apply_refresh(&mut cache, None, None, relays.clone(), now);
        assert!(cache.value.is_none(), "a first-ever failure stays unknown (–), not 0");

        // A success populates the count.
        apply_refresh(&mut cache, Some(5), Some(seen(5, now)), relays.clone(), now);
        assert_eq!(cache.value.as_ref().unwrap().online, Some(5));

        // A transient failure AFTER a success must NOT revert to "–" — it keeps the last count.
        apply_refresh(&mut cache, None, None, relays.clone(), now);
        assert_eq!(cache.value.as_ref().unwrap().online, Some(5), "no sticky –: last count survives a failed cycle");

        // A later success recovers/updates it.
        apply_refresh(&mut cache, Some(7), Some(seen(7, now)), relays, now);
        assert_eq!(cache.value.as_ref().unwrap().online, Some(7));
    }

    /// `n` synthetic fresh-presence pairs, all seen at `now`.
    fn seen(n: usize, now: chrono::DateTime<chrono::Utc>) -> Vec<PresenceSeen> {
        (0..n).map(|i| PresenceSeen { npub: format!("npub1test{i}"), seen_at: now }).collect()
    }

    #[test]
    fn count_is_global_and_pills_are_author_filtered_decoupled() {
        // W1: the chip's `online` is the GLOBAL count; `fresh` is the author-filtered contacts-only
        // map. They are independent now (pre-W1 they were the same vector's length). Prove it: a
        // global count of 42 with only 3 contact pills seen.
        let now = chrono::Utc::now();
        let mut cache = OnlineCache::default();
        apply_refresh(&mut cache, Some(42), Some(seen(3, now)), vec![], now);
        let v = cache.value.as_ref().unwrap();
        assert_eq!(v.online, Some(42), "chip = global count");
        assert_eq!(v.fresh.len(), 3, "pills = author-filtered contacts, independent of the count");
    }

    #[test]
    fn a_failed_cycle_keeps_the_last_fresh_set_not_an_empty_one() {
        // The "no sticky –" rule applies to the pairs too: one flaked poll must not blank every
        // contact's pill to "unknown" — that would be the same lie in a new place.
        let now = chrono::Utc::now();
        let mut cache = OnlineCache::default();
        apply_refresh(&mut cache, Some(2), Some(seen(2, now)), vec![], now);
        apply_refresh(&mut cache, None, None, vec![], now);
        assert_eq!(cache.value.as_ref().unwrap().fresh.len(), 2, "last-known fresh set survives");
    }

    #[test]
    fn chip_and_pills_degrade_independently() {
        // W1 codex-review fix (findings 1+2): the chip and the pills refresh on their own. A failed
        // chip query must not discard freshly-read pills, and a failed pills read (e.g. a store error
        // reading the contact list) must not blank the pills to a false "offline" nor disturb the chip.
        let now = chrono::Utc::now();
        let mut cache = OnlineCache::default();
        apply_refresh(&mut cache, Some(10), Some(seen(3, now)), vec![], now);

        // Chip query fails, pills succeed with a NEW (larger) set → chip holds at 10, pills update.
        apply_refresh(&mut cache, None, Some(seen(5, now)), vec![], now);
        let v = cache.value.as_ref().unwrap();
        assert_eq!(v.online, Some(10), "chip failure keeps the last count");
        assert_eq!(v.fresh.len(), 5, "pills updated independently of the failed chip");

        // Pills read fails (store error), chip succeeds → pills hold at 5, chip updates to 20.
        apply_refresh(&mut cache, Some(20), None, vec![], now);
        let v = cache.value.as_ref().unwrap();
        assert_eq!(v.online, Some(20), "chip updated independently of the failed pills");
        assert_eq!(v.fresh.len(), 5, "pills failure keeps the last set — never a false offline");
    }

    // -----------------------------------------------------------------------
    // Command-dispatch tests (QURATOR-179) — through the real #[tauri::command]
    // shim (arg deserialization + state injection), not by calling bodies directly.
    // -----------------------------------------------------------------------
    mod command_guards {
        use super::*;
        use tauri::Manager;

        fn guard_app() -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            // RELAY HAZARD: an empty configured relay set falls back to the real public
            // DEFAULT_RELAYS (net::relay_urls) — pin an unroutable one so the relay_set this
            // command reports, and any background refresh it spawns, never touch the internet.
            store
                .save_settings(&crate::store::Settings {
                    relay_urls: vec!["ws://127.0.0.1:9".into()],
                    ..Default::default()
                })
                .unwrap();
            app.manage(store);
            // No identity loaded: refresh_count's `let Some(id) = snapshot else { return };`
            // early-returns before any relay contact, so the spawned background refresh this
            // command may kick off is a guaranteed no-op — hermetic even without the dead relay.
            let identity: SharedIdentity = std::sync::Arc::new(tokio::sync::RwLock::new(None));
            app.manage(identity);
            app.manage(net::new_shared());
            app.manage(SharedOnlineCache::default());
            app
        }

        #[tokio::test]
        async fn online_count_command_reaches_the_managed_store_for_its_relay_set() {
            let app = guard_app();
            let got = online_count(
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
                app.state::<SharedRelay>(),
                app.state::<SharedOnlineCache>(),
            )
            .await
            .unwrap();
            assert_eq!(got.online, None, "no cache yet and no identity to refresh with → unknown");
            assert_eq!(
                got.relay_set,
                vec!["ws://127.0.0.1:9".to_string()],
                "relay_set is read from the managed store's own settings, not a default/stub"
            );
            // mutation: change the `relay_set` field in online_count's fallback
            // `OnlineCount { .. }` literal (the `Ok(cached.unwrap_or(...))` branch) to `vec![]` —
            // the shim would then report an empty set despite the managed store, reached through
            // `State<DataStore>`, holding ["ws://127.0.0.1:9"].
        }

        #[tokio::test]
        async fn online_count_command_claims_the_refresh_slot_on_the_managed_cache() {
            let app = guard_app();
            {
                let cache = app.state::<SharedOnlineCache>();
                assert!(cache.read().await.last_attempt.is_none(), "a fresh cache was never attempted");
            }
            online_count(
                app.state::<DataStore>(),
                app.state::<SharedIdentity>(),
                app.state::<SharedRelay>(),
                app.state::<SharedOnlineCache>(),
            )
            .await
            .unwrap();
            let cache = app.state::<SharedOnlineCache>();
            assert!(
                cache.read().await.last_attempt.is_some(),
                "the command shim reached the SAME managed cache Arc this test reads back from"
            );
            // mutation: delete the `c.last_attempt = Some(Instant::now());` line inside
            // online_count's critical section — the assertion above would then still observe
            // `None` on the same managed `SharedOnlineCache` after the call returns.
        }
    }
}
