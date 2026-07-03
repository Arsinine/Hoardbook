//! Persistent **shared** Nostr relay access (M12 W1 — replaces the M4 connect-per-command model).
//!
//! Every network action used to open a *fresh* [`RelayClient`] to all configured relays, use it,
//! and drop it. Under load (the DM poll every 4 s, presence every 5 min, the online/nav polls) that
//! hammered public relays into rate-limits → intermittent "Unreachable" → a slow relay dragged every
//! read to the timeout ceiling → the online chip stuck at "–" and two clients never saw each other
//! (HANDOVER #8/#9/#11). M12 keeps **one** lazily-initialised, Tauri-managed [`RelayClient`] and
//! reuses its single connection.
//!
//! **Concurrency (chorus round-1 non-negotiables):** the managed state is a
//! [`tokio::sync::RwLock`] — never `std::sync::RwLock` — because the guard must survive `.await`;
//! [`client`] clones the inner `Arc<RelayClient>` **out** and releases the guard before the caller
//! awaits any network op (no lock held across publish/fetch). Lazy init is **double-checked** under
//! the write lock so a race can't open two connections. A mid-session **dead pool** is detected and
//! **rebuilt** (it must not become a silent SPOF — INV-5). A Settings relay-set change is an
//! **atomic build-and-swap**, not an in-place removal (there is no `remove_relay`).
//!
//! The get-or-connect control flow lives in [`get_or_connect`], generic over a [`Pool`] seam, so the
//! concurrency invariants (exactly-one-connect, relay-removal rebuild, dead-pool reconnect) are
//! unit-tested with a counting fake — the riskiest code in M12 is the most-tested.

use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use hb_core::Identity;
use hb_net::RelayClient;
use tokio::sync::RwLock;

use crate::store::DataStore;

/// Handshake/fetch timeout for a relay connection.
pub const RELAY_TIMEOUT: Duration = Duration::from_secs(10);

/// Curated default seed relays a fresh install rides until the user customises their set. These are
/// public Nostr relays — there is **no Hoardbook-run SPOF** (spec §Relay Model) — chosen from the
/// set the launch survey (`RELAY_DEPLOY.md` §2) verified accept the Hoardbook kinds + brand-new
/// `npub`s + retention with no PoW. The user can remove/replace any of them in Settings; clearing
/// them all simply falls back here again, so the app is never left with zero relays. The list itself
/// lives in `ui/src/lib/default_relays.json` — the **single source of truth** shared with
/// `ui/src/lib/relays.ts` (audit I-2: one config file, no hand-mirrored Rust/TS constants).
pub static DEFAULT_RELAYS: LazyLock<Vec<String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../ui/src/lib/default_relays.json"))
        .expect("default_relays.json is a JSON array of relay URL strings")
});

/// Managed state: the one persistent shared client, or `None` before first network use. An
/// `Arc<RelayClient>` is handed out per call; the outer `Arc<RwLock<…>>` is cloned into background
/// tasks. Mirrors `SharedIdentity`.
pub type SharedRelay = Arc<RwLock<Option<Arc<RelayClient>>>>;

/// A fresh, empty shared-relay slot (lazily filled on first network use).
pub fn new_shared() -> SharedRelay {
    Arc::new(RwLock::new(None))
}

/// The effective relay set (seed + write). An **empty** persisted set falls back to
/// [`DEFAULT_RELAYS`] so the app is never stranded with zero relays — this is reached two ways and
/// neither must brick it: a **fresh install** (no settings file) OR a settings file created by a
/// **non-relay path** (`acknowledge_privacy_notice`, the update marker) that persisted
/// `Settings::default()`, whose `relay_urls` is `[]`. The Settings UI *shows* `DEFAULT_RELAYS`
/// (reachable, green) but only *persists* them when the user explicitly saves the Relays section, so
/// before the devtest-2026-06-25 #1 fix any other first write left `relay_urls = []` and every
/// command then failed "No relays configured" even with relays connected. A **configured** non-empty
/// set is honoured verbatim. (Supersedes the M12 "honour a deliberately-empty set" behaviour: going
/// dark by clearing every relay is not a Hoardbook feature — INV-5 says spread relays, never zero.)
pub fn relay_urls(store: &DataStore) -> Vec<String> {
    let configured = store.load_settings().ok().flatten().map(|s| s.relay_urls).unwrap_or_default();
    if configured.is_empty() {
        return DEFAULT_RELAYS.clone();
    }
    configured
}

/// The seam over *building + introspecting* a relay pool, so the shared-client concurrency logic
/// ([`get_or_connect`]) is unit-testable with a counting fake. Futures are `+ Send` (RPITIT) so
/// `get_or_connect` stays `Send` inside Tauri command futures.
pub(crate) trait Pool {
    type Client: Send + Sync + 'static;
    /// Build + connect a client for exactly `relays`.
    fn connect(&self, relays: &[String]) -> impl Future<Output = Result<Self::Client>> + Send;
    /// Whether a stored client's pool is still live (false ⇒ rebuild — dead-pool reconnect).
    fn is_live(&self, client: &Self::Client) -> impl Future<Output = bool> + Send;
    /// The **configured** relay set the client was built for (peer-outbox relays added later via
    /// `ensure_relays` are not reported), so a Settings change is detected and triggers an atomic
    /// rebuild while a transient peer-relay addition does not.
    fn relays_of(&self, client: &Self::Client) -> Vec<String>;
}

/// Order-insensitive relay-set equality (chorus round-1, Gemini): `relay_urls(store)` and
/// `RelayClient::relays()` come from the same source so they *should* share order, but assuming it is
/// fragile — a reorder would otherwise make `==` fail every call and reconnect on every command. A
/// set comparison rebuilds only on a genuine membership change (a reorder is harmless).
fn same_relay_set(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a: Vec<&String> = a.iter().collect();
    let mut b: Vec<&String> = b.iter().collect();
    a.sort();
    b.sort();
    a == b
}

/// Get the live shared client for `want`, building it once if needed. The whole point of M12 W1:
///
/// 1. **fast path:** clone any stored `Arc` **out** of the read guard and **drop the guard** before
///    awaiting `is_live` (chorus round-1: never hold a lock across an await, even a cheap one) — a
///    live client whose configured set equals `want` is returned without reconnect.
/// 2. **slow path** (write lock): **double-check** the Option is still stale (a racing caller may
///    have just (re)built it — this prevents two `connect`s / TOCTOU), then build once and
///    **atomic-swap** it in (the old `Arc` drops when its last reader finishes — covers both a
///    relay *removal*, where the set changed, and a dead-pool *reconnect*).
///
/// **Bounded-blocking trade-off (chorus round-1):** the write lock IS held across `pool.connect()`
/// (a handshake up to `RELAY_TIMEOUT`). This serializes concurrent callers behind one connect — the
/// intended exactly-one-connect (OnceCell-like) behaviour. It only blocks callers when there is **no
/// usable client** (lazy init, a dead pool, or a relay-set change), i.e. exactly when every caller
/// must wait for a client anyway — so block-until-ready is correct here, not a hang.
pub(crate) async fn get_or_connect<P: Pool>(
    shared: &Arc<RwLock<Option<Arc<P::Client>>>>,
    want: &[String],
    pool: &P,
) -> Result<Arc<P::Client>> {
    // Fast path: clone the Arc out, release the read guard, THEN check liveness off-lock.
    let candidate = { shared.read().await.as_ref().map(Arc::clone) };
    if let Some(client) = candidate {
        if same_relay_set(&pool.relays_of(&client), want) && pool.is_live(&client).await {
            return Ok(client);
        }
    }
    let mut guard = shared.write().await;
    // Double-check under the write lock: a racing caller may have filled/refreshed the slot.
    if let Some(client) = guard.as_ref() {
        if same_relay_set(&pool.relays_of(client), want) && pool.is_live(client).await {
            return Ok(Arc::clone(client));
        }
    }
    let client = Arc::new(pool.connect(want).await?);
    *guard = Some(Arc::clone(&client));
    Ok(client)
}

/// The production pool: builds a real [`RelayClient`] against the configured set with the session
/// identity. `is_live` reads nostr-sdk's per-relay status; `relays_of` reports the configured base
/// set (NOT `ensure_relays`-added peer outboxes, so a browse can't trigger a spurious rebuild).
struct RealPool {
    identity: Identity,
    timeout: Duration,
}

impl Pool for RealPool {
    type Client = RelayClient;
    fn connect(&self, relays: &[String]) -> impl Future<Output = Result<RelayClient>> + Send {
        let identity = self.identity.clone();
        let relays = relays.to_vec();
        let timeout = self.timeout;
        async move {
            RelayClient::connect(&identity, &relays, timeout)
                .await
                .map_err(|e| anyhow!("Could not connect to any relay: {e}"))
        }
    }
    fn is_live(&self, client: &RelayClient) -> impl Future<Output = bool> + Send {
        client.is_live()
    }
    fn relays_of(&self, client: &RelayClient) -> Vec<String> {
        client.relays().to_vec()
    }
}

/// The persistent shared [`RelayClient`] for `identity`, lazily built on first use and reused
/// thereafter. Errors (actionably) if no relay is configured. A Settings relay-set change or a dead
/// pool is rebuilt automatically (atomic swap). **Never** `disconnect()`'d per command — the client
/// is dropped once on exit (`RunEvent::ExitRequested`).
pub async fn client(
    identity: &Identity,
    store: &DataStore,
    shared: &SharedRelay,
) -> Result<Arc<RelayClient>> {
    let relays = relay_urls(store);
    if relays.is_empty() {
        return Err(anyhow!("No relays configured. Add a relay in Settings first."));
    }
    let pool = RealPool { identity: identity.clone(), timeout: RELAY_TIMEOUT };
    get_or_connect(shared, &relays, &pool).await
}

/// Drop the shared client so the next [`client`] call rebuilds it — used after a Settings relay-set
/// change (the atomic-swap force path) and as a manual force-reconnect. The old `Arc`'s connections
/// close when its last in-flight reader finishes.
pub async fn reset(shared: &SharedRelay) {
    *shared.write().await = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Settings;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn relay_urls_falls_back_to_defaults_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        // No settings file at all (fresh install) → the public defaults, so the app can reach relays.
        assert_eq!(relay_urls(&store), *DEFAULT_RELAYS);
    }

    #[test]
    fn relay_urls_falls_back_to_defaults_when_empty() {
        // Devtest 2026-06-25 #1: a persisted EMPTY relay set is treated as "unconfigured", not
        // "deliberately dark" — it falls back to the curated defaults so the app is never stranded
        // with zero relays. (Supersedes the M12 "honour a deliberately-empty set" behaviour, which
        // bricked every action the moment a non-relay settings write persisted Settings::default().)
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store.save_settings(&Settings { relay_urls: vec![], ..Default::default() }).unwrap();
        assert_eq!(relay_urls(&store), *DEFAULT_RELAYS);
    }

    #[test]
    fn default_relays_meet_the_inv5_floor() {
        // Audit I-2: the defaults parse from `ui/src/lib/default_relays.json` (the single source of
        // truth shared with relays.ts). Floor asserts: never collapse to ONE relay (INV-5 — spread
        // relays, no SPOF) and never ship a plaintext `ws://` default. Editing the JSON below this
        // floor fails here AND in relays.test.ts.
        assert!(!DEFAULT_RELAYS.is_empty(), "defaults must be non-empty");
        let distinct: std::collections::HashSet<&String> = DEFAULT_RELAYS.iter().collect();
        assert!(distinct.len() >= 2, "INV-5: at least two DISTINCT default relays, never one");
        for r in DEFAULT_RELAYS.iter() {
            assert!(r.starts_with("wss://"), "default relay {r} must be wss:// (no plaintext ws defaults)");
        }
    }

    #[test]
    fn a_non_relay_settings_write_does_not_strand_the_app() {
        // Regression (devtest 2026-06-25 #1): the FIRST settings write through a NON-relay path —
        // acknowledge_privacy_notice / the update marker, i.e. load-default-modify-save with no
        // prior file — persists relay_urls=[]. The app must still resolve working relays afterwards
        // rather than erroring "No relays configured" on every action even with relays connected.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let mut s = store.load_settings().unwrap().unwrap_or_default(); // None ⇒ default (relay_urls=[])
        s.privacy_notice_acknowledged = true;
        store.save_settings(&s).unwrap();
        assert!(
            !relay_urls(&store).is_empty(),
            "a settings file created by a non-relay path must not leave the app with zero relays"
        );
    }

    #[test]
    fn relay_urls_uses_configured_set_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_settings(&Settings { relay_urls: vec!["wss://my.relay".into()], ..Default::default() })
            .unwrap();
        assert_eq!(relay_urls(&store), vec!["wss://my.relay".to_string()]);
    }

    // ── The shared-client concurrency seam (chorus round-1: the riskiest code) ──────────────────

    /// A fake client: its configured relay set + a flippable liveness flag, with a per-client id so
    /// "is it the same Arc?" is observable.
    struct FakeClient {
        relays: Vec<String>,
        live: AtomicBool,
        id: usize,
    }

    /// A fake pool that counts how many times `connect` ran — the exact assertion the init-race and
    /// reuse cases need.
    struct FakePool {
        connects: AtomicUsize,
    }

    impl FakePool {
        fn new() -> Self {
            Self { connects: AtomicUsize::new(0) }
        }
    }

    impl Pool for FakePool {
        type Client = FakeClient;
        fn connect(&self, relays: &[String]) -> impl Future<Output = Result<FakeClient>> + Send {
            let n = self.connects.fetch_add(1, Ordering::SeqCst);
            let relays = relays.to_vec();
            async move {
                // A tiny await so concurrent callers actually overlap inside the write lock.
                tokio::task::yield_now().await;
                Ok(FakeClient { relays, live: AtomicBool::new(true), id: n })
            }
        }
        fn is_live(&self, client: &FakeClient) -> impl Future<Output = bool> + Send {
            let live = client.live.load(Ordering::SeqCst);
            async move { live }
        }
        fn relays_of(&self, client: &FakeClient) -> Vec<String> {
            client.relays.clone()
        }
    }

    fn set(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn client_is_reused_across_calls_no_reconnect() {
        let shared: Arc<RwLock<Option<Arc<FakeClient>>>> = Arc::new(RwLock::new(None));
        let pool = FakePool::new();
        let want = set(&["wss://a", "wss://b"]);
        let c1 = get_or_connect(&shared, &want, &pool).await.unwrap();
        let c2 = get_or_connect(&shared, &want, &pool).await.unwrap();
        assert!(Arc::ptr_eq(&c1, &c2), "the same client is reused (no reconnect-per-command)");
        assert_eq!(pool.connects.load(Ordering::SeqCst), 1, "connect ran exactly once");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn init_race_connects_exactly_once() {
        // chorus TOCTOU: many concurrent first-callers must yield exactly ONE connect (the
        // double-check under the write lock, not two open connections).
        let shared: Arc<RwLock<Option<Arc<FakeClient>>>> = Arc::new(RwLock::new(None));
        let pool = Arc::new(FakePool::new());
        let want = set(&["wss://a"]);
        let mut handles = Vec::new();
        for _ in 0..16 {
            let shared = Arc::clone(&shared);
            let pool = Arc::clone(&pool);
            let want = want.clone();
            handles.push(tokio::spawn(async move { get_or_connect(&shared, &want, &*pool).await.map(|_| ()) }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(pool.connects.load(Ordering::SeqCst), 1, "exactly one connect under a concurrent first-use race");
    }

    #[tokio::test]
    async fn reordered_same_set_does_not_reconnect() {
        // chorus round-1 (Gemini): a relay set in a different ORDER is the same set → reuse, not a
        // spurious reconnect-every-command. (FakePool returns relays in build order; the wanted set
        // here is the reverse — must still match.)
        let shared: Arc<RwLock<Option<Arc<FakeClient>>>> = Arc::new(RwLock::new(None));
        let pool = FakePool::new();
        let c1 = get_or_connect(&shared, &set(&["wss://a", "wss://b"]), &pool).await.unwrap();
        let c2 = get_or_connect(&shared, &set(&["wss://b", "wss://a"]), &pool).await.unwrap();
        assert!(Arc::ptr_eq(&c1, &c2), "a reordered same set must reuse the client, not reconnect");
        assert_eq!(pool.connects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn relay_removal_rebuilds_via_atomic_swap() {
        // Changing the configured set (a removal in Settings) replaces the client — the old relay
        // is no longer the live client's set. A pure addition would equally rebuild; either way the
        // removed relay is no longer dialed.
        let shared: Arc<RwLock<Option<Arc<FakeClient>>>> = Arc::new(RwLock::new(None));
        let pool = FakePool::new();
        let c1 = get_or_connect(&shared, &set(&["wss://a", "wss://b"]), &pool).await.unwrap();
        let c2 = get_or_connect(&shared, &set(&["wss://a"]), &pool).await.unwrap();
        assert!(!Arc::ptr_eq(&c1, &c2), "a changed relay set rebuilds the client (atomic swap)");
        assert_eq!(c2.relays, set(&["wss://a"]), "the live client dials only the new set — the removed relay is gone");
        assert_eq!(pool.connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dead_pool_reconnects_rather_than_returning_a_corpse() {
        // chorus / Decision A-recovery: a client whose pool died mid-session is rebuilt on next use,
        // never returned as a corpse that fails every command silently (the new INV-5 SPOF mitigation).
        let shared: Arc<RwLock<Option<Arc<FakeClient>>>> = Arc::new(RwLock::new(None));
        let pool = FakePool::new();
        let want = set(&["wss://a"]);
        let c1 = get_or_connect(&shared, &want, &pool).await.unwrap();
        c1.live.store(false, Ordering::SeqCst); // the pool dies
        let c2 = get_or_connect(&shared, &want, &pool).await.unwrap();
        assert!(!Arc::ptr_eq(&c1, &c2), "a dead pool is rebuilt, not reused");
        assert!(c2.live.load(Ordering::SeqCst), "the rebuilt client is live");
        assert_eq!(pool.connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn reset_forces_a_rebuild_on_next_use() {
        let shared: Arc<RwLock<Option<Arc<FakeClient>>>> = Arc::new(RwLock::new(None));
        let pool = FakePool::new();
        let want = set(&["wss://a"]);
        let c1 = get_or_connect(&shared, &want, &pool).await.unwrap();
        *shared.write().await = None; // what net::reset does — force a rebuild on next use
        let c2 = get_or_connect(&shared, &want, &pool).await.unwrap();
        assert!(!Arc::ptr_eq(&c1, &c2), "after reset the next call rebuilds (force-reconnect / settings swap)");
        assert_eq!(c1.id, 0);
        assert_eq!(c2.id, 1);
    }
}
