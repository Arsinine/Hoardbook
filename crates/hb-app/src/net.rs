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

use crate::store::{DataStore, Settings};

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

/// (QURATOR-208) Pure core of the additive default-relay sync: given the persisted relay set, the
/// known-defaults watermark (see [`Settings::known_default_relays`]) and the CURRENT default list,
/// return the new persisted set (persisted order kept, genuinely-new defaults appended after it)
/// and the new watermark (the full current default list — a default some future release drops
/// from the shipped set is harmlessly forgotten). A default already in `persisted` is never
/// duplicated; a default in the watermark is never appended — which is exactly what makes a
/// user's deletion stick across upgrades.
pub(crate) fn merge_default_relays(
    persisted: &[String],
    watermark: &[String],
    defaults: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut merged = persisted.to_vec();
    for d in defaults {
        let offered = watermark.iter().any(|w| w == d);
        let present = merged.iter().any(|r| r == d);
        if !offered && !present {
            merged.push(d.clone());
        }
    }
    (merged, defaults.to_vec())
}

/// (QURATOR-208 — owner devtest v0.20.0 #2: "Settings shows 2 relays, don't we have 4 now? Not
/// synced with backend".) Additive default-relay sync, run **once at app startup** — never in
/// [`relay_urls`], which sits on hot paths (DM poll, presence, online/nav polls) and must stay
/// read-only; a write-on-read there would re-write `settings.json` on every poll. Append the
/// current defaults the user has never been offered to the persisted set and persist the
/// watermark; see [`Settings::known_default_relays`] for what an ABSENT watermark means (the
/// one-time-union ruling). Never-fail by contract: a settings read/write error logs and returns,
/// leaving the previous file untouched — a failed sync must not brick startup, and the next
/// launch simply retries it.
pub fn reconcile_default_relays(store: &DataStore) {
    let mut settings = match store.load_settings() {
        Ok(Some(s)) => s,
        // No settings file yet (fresh install, or only a non-relay write ever happened): start
        // from the serde defaults — empty persisted set AND empty watermark — so the union below
        // materialises the defaults as the explicitly-configured set and records the offering.
        Ok(None) => Settings::default(),
        Err(e) => {
            tracing::warn!(error = %e, "default-relay sync: could not load settings; skipping");
            return;
        }
    };
    let (merged, watermark) =
        merge_default_relays(&settings.relay_urls, &settings.known_default_relays, &DEFAULT_RELAYS);
    if merged == settings.relay_urls && watermark == settings.known_default_relays {
        return; // nothing new offered — leave settings.json byte-identical (no gratuitous rewrite)
    }
    settings.relay_urls = merged;
    settings.known_default_relays = watermark;
    if let Err(e) = store.save_settings(&settings) {
        tracing::warn!(error = %e, "default-relay sync: could not save settings; will retry next launch");
    }
}

/// SSRF guard on **user-supplied** relay URLs (audit I-11): reject any scheme other than
/// `ws://`/`wss://`, and any host that is loopback (127.0.0.0/8, ::1, localhost), private
/// (10/8, 172.16/12, 192.168/16, fc00::/7), link-local (169.254/16, fe80::/10), or another
/// non-global class (chorus M13 #2: CGNAT 100.64.0.0/10, benchmarking 198.18/15, multicast,
/// broadcast, documentation, unspecified) — including IPv4-mapped/-compatible IPv6
/// (`::ffff:127.0.0.1`, `::10.0.0.5`) and bracketed hosts with ports. Hostnames are checked
/// **literally only** (`localhost`, `*.localhost`, mDNS `*.local`) — there is deliberately NO DNS
/// resolution here, so a public name that rebinds to a private IP is an accepted residual
/// (prosumer tier; resolving would add a blocking lookup + TOCTOU without closing the hole).
///
/// The implementation lives in `hb_net::client::validate_relay_url` since QURATOR-196 (it now also
/// guards peer-advertised relay URLs on the browse path, so it had to be reachable from hb-net);
/// this wrapper keeps the `net::validate_relay_url` call sites and the app-layer tests unchanged,
/// asserting against the ONE implementation.
pub fn validate_relay_url(url: &str) -> Result<(), String> {
    hb_net::validate_relay_url(url)
}

/// Whether `ip` is non-globally-routable — the `IpAddr` dispatch over hb-net's
/// `ipv4_non_global`/`ipv6_non_global` for callers holding a bare address (e.g. a socket address
/// from a peer-authored dial target, QURATOR-113 #20). Delegates to the one implementation in
/// hb-net since QURATOR-196; no new classification.
pub(crate) fn ip_non_global(ip: std::net::IpAddr) -> bool {
    hb_net::ip_non_global(ip)
}

/// Local NAT classification inferred from the observed local address and, when available, the
/// mapped/public address learned from outside (a STUN-like or relay-reported observation). The
/// decision is **pure** and answers the ticket's offline-testable questions:
///
/// - **`NoNat`** — `mapped == local`. The host's idea of its own address is what the outside world
///   sees, so there is no translation in between.
/// - **`BehindNat`** — `local` is in an RFC 1918 private range (`10/8`, `192.168/16`, `172.16/12`)
///   and the mapped address (if any) differs. This is answerable **offline** from `local` alone;
///   the mapped address only confirms it.
/// - **`BehindCgnat`** — the mapped address is in `100.64.0.0/10` (RFC 6598). This is NOT
///   answerable offline; it requires the outside view. `100.64/10` is a **strong signal, not
///   proof** — some ISPs put CGNAT customers in other ranges, and double-NAT (RFC 1918 behind a
///   CGNAT, or RFC 1918 behind another RFC 1918) exists. Treat the variant as "the observed
///   outside address is a CGNAT face", not as a certainty about the operator's whole topology.
/// - **`Unknown`** — no mapped address has been observed AND `local` is not an RFC 1918 private
///   address (cold start or fully offline on a public-looking local). **This must never render as a
///   confident negative** (cf. QURATOR-67, where unknown is RED): the absence of an outside
///   observation is not the presence of "no NAT". An RFC 1918 local with no mapped does NOT land
///   here — it is `BehindNat` offline-answerable.
///
/// **Privacy (INV — "presence carries no address or node key"):** the classification carries **no
/// address data** in its variants or their `Debug`/`Display` output. The observed mapped address is
/// **local-display-only** — it must never be published, never enter a presence event or listing,
/// never leave the machine, and never be written to the log. The classification itself is the
/// loggable thing; the addresses are not. Returning a bare classification from this function makes
/// leaking the raw mapped address as a side-effect of asking for the classification structurally
/// impossible.
///
/// **Future evolution:** the enum is `#[non_exhaustive]`. A future `Symmetric` variant (mapped
/// address differing from the local one in a way only a STUN-binding-style probe can distinguish
/// from cone NAT) is **additive**: existing match arms are already required to carry a `_` case, so
/// adding it does not break them. `Symmetric` detection is an open owner question and is out of
/// scope here — the type merely leaves room for it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatClassification {
    /// `mapped == local`: the host's own address is the outside address.
    NoNat,
    /// `local` is RFC 1918 private and the outside view differs (or is unseen but the private local
    /// is itself the tell). Answerable offline from `local`.
    BehindNat,
    /// The mapped/public address sits in `100.64.0.0/10` (RFC 6598). A strong CGNAT signal, not
    /// proof — some ISPs use other ranges, and double-NAT exists.
    BehindCgnat,
    /// No mapped address AND a non-RFC-1918 local (the only genuinely undecided case). NOT a
    /// confident negative.
    Unknown,
}

impl NatClassification {
    /// Lowercase one-word rendering for log lines and diagnostics, e.g. `"cgnat"`, `"unknown"`.
    /// Kept deliberately short and free of any address data (see the type-level privacy note).
    pub fn as_log_token(self) -> &'static str {
        match self {
            NatClassification::NoNat => "no-nat",
            NatClassification::BehindNat => "nat",
            NatClassification::BehindCgnat => "cgnat",
            NatClassification::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for NatClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_log_token())
    }
}

/// True iff `ip` is an IPv4 in an RFC 1918 private range (`10/8`, `192.168/16`, `172.16/12`). This
/// is the offline-testable half of NAT detection: a host with a private local address is behind
/// *some* NAT by definition, because private space is not routable on the public Internet. IPv6 has
/// no equivalent — a ULA (`fc00::/7`) is conventionally private but IPv6 hosts routinely have a
/// global address alongside, so we don't treat ULA-alone as a NAT signal here.
fn is_rfc1918_private(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 10 || (o[0] == 192 && o[1] == 168) || (o[0] == 172 && (o[1] & 0xF0) == 16)
        }
        std::net::IpAddr::V6(_) => false,
    }
}

/// True iff `ip` is IPv4 in `100.64.0.0/10` (RFC 6598 CGNAT space). This is the outside-view half
/// of CGNAT detection: only seeing this range on the *mapped* address means the operator's NAT is
/// a carrier-grade one. Seeing it on the *local* address is unusual but harmless (it would just
/// trip `is_rfc1918_private`-style logic on the local side; we don't treat it as a local-private
/// signal for `BehindNat` — RFC 6598 is not RFC 1918).
fn is_rfc6598_cgnat(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xC0) == 64
        }
        std::net::IpAddr::V6(_) => false,
    }
}

/// Pure NAT classification from the observed `local` address and an optional outside-observed
/// `mapped` address. No network I/O, no global state — the hard part is testable with table-driven
/// cases and no sockets (QURATOR-68 core scope).
///
/// **Decision order** (see [`NatClassification`] for the per-variant semantics):
/// 1. `mapped == Some(local)` → [`NatClassification::NoNat`] (the outside agrees with the inside).
/// 2. `mapped` is present and `is_rfc6598_cgnat(mapped)` → [`NatClassification::BehindCgnat`]
///    (strong signal, not proof).
/// 3. `is_rfc1918_private(local)` → [`NatClassification::BehindNat`] — this is the
///    **offline-answerable** path: a private local address is behind *some* NAT whether or not we
///    yet have a mapped address.
/// 4. `mapped == None` → [`NatClassification::Unknown`] (non-private local AND no outside view —
///    genuinely undecided; must not render as a confident negative).
/// 5. otherwise → [`NatClassification::BehindNat`] (local is public, mapped differs — translation
///    is implied, even though neither the RFC 1918 nor RFC 6598 tells fired).
pub fn classify_nat(local: std::net::IpAddr, mapped: Option<std::net::IpAddr>) -> NatClassification {
    if let Some(mapped) = mapped {
        if mapped == local {
            return NatClassification::NoNat;
        }
        if is_rfc6598_cgnat(mapped) {
            return NatClassification::BehindCgnat;
        }
    }
    if is_rfc1918_private(local) {
        return NatClassification::BehindNat;
    }
    match mapped {
        Some(_) => NatClassification::BehindNat, // public local + differing mapped ⇒ translation
        None => NatClassification::Unknown,      // no outside view, no private tell ⇒ undecided
    }
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
    use crate::store::tests::test_store_unroutable_relays;
    use crate::store::Settings;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn relay_urls_of_an_unroutable_test_store_never_falls_back_to_the_real_defaults() {
        // Guard for QURATOR-179 slice 2: `test_store_unroutable_relays` exists so a test that
        // reaches relay I/O fails loudly against a sentinel host instead of quietly dialling the
        // four real public DEFAULT_RELAYS. This pins that the helper actually keeps relay_urls()
        // off the fallback path — without it, a bug in the helper (e.g. saving an empty relay set)
        // would silently re-open the exact hole this ticket closes.
        //
        // MUTATION (must red this test): in `relay_urls` above, change the guard condition from
        // `if configured.is_empty()` to `if true` — relay_urls would then return DEFAULT_RELAYS for
        // every store, including this one. (This also reds the existing
        // `relay_urls_uses_configured_set_when_present` test below, which pins the same line.)
        let (_dir, store) = test_store_unroutable_relays();
        let urls = relay_urls(&store);
        assert_ne!(urls, *DEFAULT_RELAYS, "an unroutable test store must never fall back to the real defaults");
        assert_eq!(urls, vec!["wss://hoardbook-test-sentinel.invalid".to_string()]);
    }

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
    fn reconcile_gains_new_defaults_for_a_set_frozen_on_an_old_default_list() {
        // QURATOR-208 acceptance 1 — the owner's own case (devtest v0.20.0 #2): the persisted set
        // is exactly the first two entries of a PRIOR shipped default list (nos.lol +
        // relay.primal.net, the Aug-2026 pair) with NO watermark (their settings.json predates the
        // field, so serde loads it as []). Absent watermark = "never offered ANY default" (see
        // Settings::known_default_relays for the ruling) → the first reconcile is a one-time
        // union and the install is un-stranded at the full current set. The second call pins
        // convergence: once the watermark covers the defaults, reconcile is a no-op.
        //
        // MUTATION (P-10): in `merge_default_relays` (the `for d in defaults` loop body), gate the
        // `merged.push(d.clone());` on `false` (or delete the push) — nothing is ever appended,
        // the owner stays stranded at 2, and every assert below reds.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let frozen: Vec<String> = DEFAULT_RELAYS.iter().take(2).cloned().collect();
        store.save_settings(&Settings { relay_urls: frozen, ..Default::default() }).unwrap();
        reconcile_default_relays(&store);
        reconcile_default_relays(&store); // idempotent: the watermark written by run 1 stops run 2
        assert_eq!(
            relay_urls(&store), *DEFAULT_RELAYS,
            "the owner's 2-relay frozen set must reach the 4 current defaults"
        );
        let saved = store.load_settings().unwrap().unwrap();
        assert_eq!(saved.relay_urls, *DEFAULT_RELAYS);
        assert_eq!(
            saved.known_default_relays, *DEFAULT_RELAYS,
            "the watermark must now cover every current default"
        );
    }

    #[test]
    fn reconcile_never_resurrects_a_default_the_user_removed() {
        // QURATOR-208 acceptance 2 — the ruling the ticket exists to enforce: additive sync means
        // a relay the user DELETED stays deleted across upgrades. The watermark covers the current
        // defaults (they were offered them); the persisted set is those minus the last one
        // (removed); the NEXT release ships one genuinely-new default. Only the new one may be added.
        //
        // MUTATION (P-10): in `merge_default_relays`, change the loop guard
        // `if !offered && !present` to `if !present` — the watermark no longer suppresses
        // anything, the removed default is re-added, and the `!merged.contains(&removed)` assert reds.
        let removed = DEFAULT_RELAYS.last().unwrap().clone();
        let kept: Vec<String> = DEFAULT_RELAYS.iter().filter(|r| *r != &removed).cloned().collect();
        let mut next_release = DEFAULT_RELAYS.clone();
        next_release.push("wss://new-default.example".to_string());
        let (merged, watermark) = merge_default_relays(&kept, &DEFAULT_RELAYS, &next_release);
        assert!(!merged.contains(&removed), "a user-removed default must NOT come back on upgrade");
        assert_eq!(
            merged.last().unwrap(),
            "wss://new-default.example",
            "the genuinely-new default is the only addition"
        );
        assert_eq!(merged.len(), kept.len() + 1);
        assert_eq!(watermark, next_release, "the watermark advances to the new release's defaults");
    }

    #[test]
    fn reconcile_preserves_the_inv5_floor_and_never_removes_a_configured_relay() {
        // QURATOR-208 acceptance 3 — INV-5 says spread relays, never a SPOF: the sync may only
        // GROW the effective set. A reconcile that dropped configured relays could take an
        // install below the two-distinct-relay floor. Worst case for that bug: nothing new is
        // offered (watermark already covers the defaults), so the merge must be an EXACT no-op —
        // the user's custom relay and their one kept default survive untouched.
        //
        // MUTATION (P-10): in `merge_default_relays`, change
        // `let mut merged = persisted.to_vec();` to `let mut merged = Vec::new();` — with
        // everything already offered the loop appends nothing, so the result is empty: the
        // no-op equality assert reds, and the distinct-count assert names the INV-5 consequence.
        let persisted = vec!["wss://custom.example".to_string(), DEFAULT_RELAYS[0].clone()];
        let watermark = DEFAULT_RELAYS.clone();
        let (merged, _) = merge_default_relays(&persisted, &watermark, &DEFAULT_RELAYS);
        assert_eq!(
            merged, persisted,
            "nothing new offered: exact no-op — the custom relay and the kept default survive"
        );
        let distinct: std::collections::HashSet<&String> = merged.iter().collect();
        assert!(distinct.len() >= 2, "INV-5: the sync must never take an install below two distinct relays");
    }

    #[test]
    fn reconciled_set_is_what_the_settings_ui_will_display() {
        // QURATOR-208 acceptance 4 — frontend and backend must resolve the SAME effective set.
        // Backend half: after reconcile the persisted set is non-empty, so `relay_urls()` returns
        // it VERBATIM (no fallback) — exactly the list the Settings page displays via
        // `effectiveRelays` (relays.ts), which returns a non-empty configured set verbatim too
        // (pinned in relays.test.ts). Both sides' default list is the same default_relays.json
        // (audit I-2, pinned in both suites), so verbatim-on-both-ends ⇒ identical effective sets.
        // The scenario is the owner's plus a custom relay, so the expected set differs from
        // DEFAULT_RELAYS and the verbatim property is actually load-bearing here.
        //
        // MUTATION (P-10): in `reconcile_default_relays`, delete the
        // `settings.relay_urls = merged;` assignment — the watermark is persisted but the relay
        // set never gains the new defaults, and the `relay_urls` assert reds (the persisted
        // custom-plus-frozen-pair set would be returned instead of the union).
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let mut frozen_with_custom = vec!["wss://custom.example".to_string()];
        frozen_with_custom.extend(DEFAULT_RELAYS.iter().take(2).cloned());
        store
            .save_settings(&Settings { relay_urls: frozen_with_custom, ..Default::default() })
            .unwrap();
        reconcile_default_relays(&store);
        let mut expected = vec!["wss://custom.example".to_string()];
        expected.extend(DEFAULT_RELAYS.iter().cloned());
        assert_eq!(relay_urls(&store), expected);
        assert_ne!(relay_urls(&store), *DEFAULT_RELAYS, "the custom relay must survive the sync");
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
    fn default_relays_are_the_owner_ruled_set() {
        // Owner ruling 2026-09-25: relay.damus.io and nostr.mom are defaults again (replacing
        // offchain.pub, which refuses TCP 443). This SUPERSEDES the 2026-08-08 ruling that dropped
        // damus for unreliability — the owner reversed it knowingly, after being shown that ruling.
        // Pinning the exact set keeps an accidental edit of default_relays.json loud.
        let expected = [
            "wss://nos.lol",
            "wss://relay.primal.net",
            "wss://relay.snort.social",
            "wss://relay.damus.io",
            "wss://nostr.mom",
        ];
        assert_eq!(*DEFAULT_RELAYS, expected, "default relay set drifted from the owner ruling");
        assert!(
            !DEFAULT_RELAYS.iter().any(|r| r.contains("offchain.pub")),
            "offchain.pub refuses connections and must not ship as a default"
        );
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

    // ── The SSRF guard on user-supplied relay URLs (audit I-11) ─────────────────────────────────

    #[test]
    fn relay_url_guard_rejects_non_ws_schemes() {
        for url in ["http://relay.damus.io", "https://relay.damus.io", "file:///etc/passwd", "ftp://1.2.3.4"] {
            assert!(validate_relay_url(url).is_err(), "{url} must be rejected (not ws/wss)");
        }
        assert!(validate_relay_url("not a url at all").is_err(), "garbage must be rejected");
    }

    #[test]
    fn relay_url_guard_rejects_loopback_and_localhost() {
        for url in [
            "ws://127.0.0.1:7777",
            "ws://127.8.9.10",
            "wss://localhost",
            "ws://LOCALHOST:7777",
            "ws://foo.localhost",
            "ws://printer.local:7777",
            "ws://0.0.0.0:7777",
        ] {
            let res = validate_relay_url(url);
            assert!(res.is_err(), "{url} must be rejected");
            let err = res.unwrap_err();
            assert!(err.contains("private/loopback"), "error must be actionable, got: {err}");
        }
    }

    #[test]
    fn relay_url_guard_rejects_private_and_link_local_ranges() {
        for url in [
            "ws://10.0.0.5:7777",
            "ws://172.16.0.1",
            "ws://172.31.255.255:7777",
            "ws://192.168.1.20:7777",
            "ws://169.254.1.1",
        ] {
            assert!(validate_relay_url(url).is_err(), "{url} must be rejected (private/link-local)");
        }
        // The 172.16/12 boundary: outside the block is public.
        assert!(validate_relay_url("ws://172.15.255.255:7777").is_ok());
        assert!(validate_relay_url("ws://172.32.0.1:7777").is_ok());
    }

    #[test]
    fn relay_url_guard_rejects_ipv6_forms() {
        for url in [
            "ws://[::1]:7777",
            "wss://[::1]",
            "ws://[fe80::1]:7777",
            "ws://[fc00::1]",
            "ws://[fd12:3456::1]:7777",
            "ws://[::ffff:127.0.0.1]:7777",
            "ws://[::ffff:10.0.0.5]",
            "ws://[::]:7777",
        ] {
            assert!(validate_relay_url(url).is_err(), "{url} must be rejected (IPv6 non-global)");
        }
        assert!(validate_relay_url("wss://[2606:4700::6810:84e5]:443").is_ok(), "public IPv6 is fine");
    }

    #[test]
    fn relay_url_guard_accepts_public_relays() {
        for url in ["wss://relay.damus.io", "ws://8.8.8.8:7777", "wss://nos.lol/", "  wss://relay.primal.net  "] {
            assert!(validate_relay_url(url).is_ok(), "{url} must pass the guard");
        }
    }

    #[test]
    fn relay_url_guard_rejects_the_chorus_flagged_edge_ranges() {
        // Chorus M13 finding #2: non-global ranges beyond the audit's loopback/private/link-local
        // wording. The guard's promise is "no non-public network", so cover the lot.
        for url in [
            "ws://100.64.1.5:7777",      // CGNAT 100.64.0.0/10
            "ws://198.18.0.1:7777",      // benchmarking 198.18.0.0/15
            "ws://224.0.0.1:7777",       // IPv4 multicast
            "ws://255.255.255.255:7777", // broadcast
            "ws://192.0.2.10:7777",      // documentation TEST-NET-1
            "ws://[::10.0.0.5]:7777",    // deprecated IPv4-compatible embedding a private v4
            "ws://[ff02::1]:7777",       // IPv6 multicast
            "ws://[2001:db8::1]:7777",   // IPv6 documentation
        ] {
            assert!(validate_relay_url(url).is_err(), "{url} must be rejected");
        }
        // /10 boundary: 100.128.0.0 sits OUTSIDE CGNAT and is plain public space.
        assert!(validate_relay_url("ws://100.128.0.1:7777").is_ok(), "just past the CGNAT /10 is public");
    }

    #[test]
    fn default_relays_all_pass_the_ssrf_guard() {
        // The guard must never brick the curated defaults — a fresh install rides these.
        for r in DEFAULT_RELAYS.iter() {
            assert!(validate_relay_url(r).is_ok(), "default relay {r} must pass the SSRF guard");
        }
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

    // ── Pure NAT classification (QURATOR-68 core: address→class, no network) ────────────────────
    //
    // The decision table below pins every branch of `classify_nat` against RFC 1918, RFC 6598 and
    // public space. Each row is red on a deliberate mutation of the branch it names (verified in
    // the mutation probes below — the project rule is "a green test proves nothing until you have
    // seen it red").

    /// `(local, mapped, expected)` — the one-line form of the decision table, consumed by the
    /// per-branch tests so the table itself is the single source of truth.
    fn nat_cases() -> Vec<(std::net::IpAddr, Option<std::net::IpAddr>, NatClassification)> {
        use std::net::IpAddr;
        let v = |s: &str| s.parse::<IpAddr>().unwrap();
        vec![
            // NoNat: mapped == local, public on both sides (no translation in between).
            (v("203.0.113.10"), Some(v("203.0.113.10")), NatClassification::NoNat),
            // NoNat also holds for IPv6.
            (v("2606:4700::1"), Some(v("2606:4700::1")), NatClassification::NoNat),
            // BehindNAT: RFC 1918 local, mapped differs (the offline-answerable case).
            (v("10.0.0.5"),  Some(v("203.0.113.10")), NatClassification::BehindNat),
            (v("192.168.1.20"), Some(v("203.0.113.11")), NatClassification::BehindNat),
            (v("172.16.0.1"), Some(v("203.0.113.12")), NatClassification::BehindNat),
            // BehindNAT answerable OFFLINE: RFC 1918 local with NO mapped yet is still BehindNAT,
            // because private space is not routable on the public Internet.
            (v("10.0.0.5"),  None, NatClassification::BehindNat),
            (v("192.168.0.2"), None, NatClassification::BehindNat),
            // BehindCgnat: mapped is in 100.64.0.0/10 (RFC 6598). Strong signal, not proof.
            (v("10.0.0.5"),  Some(v("100.64.1.5")),  NatClassification::BehindCgnat),
            (v("192.168.0.2"), Some(v("100.127.255.254")), NatClassification::BehindCgnat),
            // Unknown: no mapped AND non-private local (the only undecided case). Not a confident
            // negative.
            (v("203.0.113.10"), None, NatClassification::Unknown),
            // Catch-all BehindNAT: local is public, mapped is a different public — translation is
            // happening, even though neither RFC 1918 nor RFC 6598 fired.
            (v("203.0.113.10"), Some(v("198.51.100.20")), NatClassification::BehindNat),
        ]
    }

    #[test]
    fn classify_nat_decision_table() {
        for (i, (local, mapped, expected)) in nat_cases().into_iter().enumerate() {
            let got = classify_nat(local, mapped);
            assert_eq!(
                got, expected,
                "row {i}: classify_nat({local}, {mapped:?}) => {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn no_nat_requires_mapped_equal_to_local() {
        // Pin the NoNat branch in isolation so a mutation that drops the equality check reds here.
        let local: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        assert_eq!(classify_nat(local, Some(local)), NatClassification::NoNat);
        // A different mapped must NOT be NoNat.
        let other: std::net::IpAddr = "198.51.100.20".parse().unwrap();
        assert_ne!(classify_nat(local, Some(other)), NatClassification::NoNat);
    }

    #[test]
    fn behind_nat_is_answerable_offline_from_rfc1918_local() {
        // Ticket: "Am I behind a NAT?" is answerable offline from the local address — RFC 1918.
        // So an RFC 1918 local with NO mapped is BehindNAT, not Unknown. A mutation that requires
        // a mapped address before returning BehindNAT reds here.
        for local in ["10.0.0.5", "192.168.1.20", "172.16.0.1", "172.31.255.254"] {
            let local = local.parse::<std::net::IpAddr>().unwrap();
            assert_eq!(
                classify_nat(local, None),
                NatClassification::BehindNat,
                "RFC 1918 local {local} with no mapped must still be BehindNAT (offline-answerable)"
            );
        }
    }

    #[test]
    fn unknown_branch_fires_only_when_no_mapped_and_non_private_local() {
        // Pin the Unknown branch: Unknown fires exactly when there is no mapped AND the local is
        // not RFC 1918 (the only genuinely undecided case). This is the "unknown must not render as
        // a confident negative" rule from the ticket (same lesson as QURATOR-67): a cold/offline
        // start on a public-looking local is NOT "NoNat".
        for local in ["203.0.113.10", "198.51.100.1", "100.64.1.5", "2606:4700::1"] {
            let local = local.parse::<std::net::IpAddr>().unwrap();
            assert_eq!(
                classify_nat(local, None),
                NatClassification::Unknown,
                "non-private local {local} with no mapped must be Unknown, not a confident negative"
            );
        }
        // The presence of ANY mapped on the same public locals must NOT be Unknown.
        let mapped: std::net::IpAddr = "203.0.113.99".parse().unwrap();
        for local in ["203.0.113.10", "100.64.1.5"] {
            let local = local.parse::<std::net::IpAddr>().unwrap();
            assert_ne!(
                classify_nat(local, Some(mapped)),
                NatClassification::Unknown,
                "a non-equal mapped must not be Unknown"
            );
        }
    }

    #[test]
    fn cgnat_branch_fires_on_rfc6598_mapped_address() {
        // Pin the CGNAT branch: a mapped address in 100.64.0.0/10 ⇒ BehindCgnat. The /10 boundary
        // is the load-bearing edge: 100.64.0.0 is the first byte inside, 100.127.255.255 the last
        // inside, 100.128.0.0 the first outside (matching the SSRF guard's existing boundary test).
        let local: std::net::IpAddr = "10.0.0.5".parse().unwrap();
        for mapped in ["100.64.0.0", "100.64.1.5", "100.127.255.255"] {
            let mapped = mapped.parse::<std::net::IpAddr>().unwrap();
            assert_eq!(
                classify_nat(local, Some(mapped)),
                NatClassification::BehindCgnat,
                "mapped {mapped} is inside 100.64.0.0/10 ⇒ CGNAT"
            );
        }
        // Just outside the /10: not CGNAT. Falls through to BehindNAT (RFC 1918 local).
        let outside: std::net::IpAddr = "100.128.0.0".parse().unwrap();
        assert_ne!(
            classify_nat(local, Some(outside)),
            NatClassification::BehindCgnat,
            "mapped {outside} sits outside the /10 and must not be classified CGNAT"
        );
        assert_eq!(
            classify_nat(local, Some(outside)),
            NatClassification::BehindNat,
            "RFC 1918 local with a non-CGNAT mapped ⇒ BehindNAT"
        );
    }

    #[test]
    fn behind_nat_branch_fires_on_rfc1918_local_with_differing_mapped() {
        // Pin the BehindNAT branch: RFC 1918 local + differing mapped ⇒ BehindNAT. The /12 and /16
        // boundary edges are checked so a mutation that widens or narrows the private test reds.
        let public: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        for local in ["10.0.0.5", "10.255.255.254", "192.168.0.1", "192.168.255.254", "172.16.0.1", "172.31.255.254"] {
            let local = local.parse::<std::net::IpAddr>().unwrap();
            assert_eq!(
                classify_nat(local, Some(public)),
                NatClassification::BehindNat,
                "RFC 1918 local {local} with a differing mapped ⇒ BehindNAT"
            );
        }
        // Just outside the three blocks: these are NOT RFC 1918. With a differing mapped they hit
        // the catch-all BehindNAT (translation still implied), so the discriminator here is that
        // they must not be CGNAT or Unknown — and with mapped==local they would be NoNat (pinned
        // by no_nat_requires_mapped_equal_to_local).
        for boundary in ["11.0.0.1", "172.15.255.255", "172.32.0.0", "192.167.0.1", "193.168.0.1"] {
            let local = boundary.parse::<std::net::IpAddr>().unwrap();
            assert_eq!(
                classify_nat(local, Some(public)),
                NatClassification::BehindNat,
                "boundary {local} is not RFC 1918 but translation is still implied (catch-all)"
            );
        }
    }

    #[test]
    fn ipv4_mapped_ipv6_equality_still_yields_no_nat() {
        // `::ffff:203.0.113.10` and `203.0.113.10` are NOT `==` as `IpAddr`, so this case documents
        // that the function treats them as differing addresses (BehindNAT), NOT as NoNat. If a
        // future change normalises IPv4-mapped IPv6 before comparing, this test reds — deliberately,
        // because normalising would be a behaviour change the owner should sign off on.
        let v4: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        let mapped_v6: std::net::IpAddr = "::ffff:203.0.113.10".parse().unwrap();
        assert_ne!(v4, mapped_v6, "sanity: IpAddr equality does not normalise v4-mapped-v6");
        assert_eq!(
            classify_nat(v4, Some(mapped_v6)),
            NatClassification::BehindNat,
            "v4 and its v4-mapped-v6 form are distinct addresses to classify_nat"
        );
    }

    #[test]
    fn classification_variants_carry_no_address_data() {
        // INV ("presence carries no address or node key"): the classification is the loggable thing,
        // the raw mapped address is not. The variants carry no data, so Debug/Display cannot leak
        // an address by construction — this test pins that structurally. If a future variant grows
        // a payload, this test must be revisited (and the payload almost certainly must not be the
        // raw address).
        let cases = [
            (NatClassification::NoNat, "no-nat"),
            (NatClassification::BehindNat, "nat"),
            (NatClassification::BehindCgnat, "cgnat"),
            (NatClassification::Unknown, "unknown"),
        ];
        for (class, token) in cases {
            let dbg = format!("{class:?}");
            let disp = format!("{class}");
            let log = class.as_log_token();
            assert_eq!(disp, token, "Display for {class:?} must be the bare token");
            assert_eq!(log, token, "as_log_token for {class:?} must be the bare token");
            for needle in ["203", "100.64", "10.0", "192.168", "::", "addr", "ip"] {
                assert!(
                    !dbg.contains(needle),
                    "Debug output {dbg:?} leaked an address-shaped substring ({needle}) for {class:?}"
                );
            }
        }
    }

    #[test]
    fn classify_nat_does_no_network_io() {
        // The function is pure: feeding it a dead (TEST-NET) and a multicast address must return
        // promptly with a classification rather than ever attempting a socket. This is a
        // source-level behavioural smoke check — the no-IO promise is also enforced by the function
        // signature taking `IpAddr`/`Option<IpAddr>` and returning `NatClassification` (no `async`,
        // no error type, no future, no `Read`/`Write`).
        let local: std::net::IpAddr = "192.0.2.1".parse().unwrap(); // TEST-NET-1
        let mapped: std::net::IpAddr = "100.64.1.5".parse().unwrap(); // RFC 6598
        let got = classify_nat(local, Some(mapped));
        assert_eq!(got, NatClassification::BehindCgnat, "CGNAT signal dominates a non-RFC-1918 local");
    }

    // ── Mutation probes: each branch red on a deliberate break, then revert ────────────────────
    //
    // The project's most-repeated lesson: a green test proves nothing until you have seen it red.
    // These are NOT #[test] (they would mutate production code at test time); they are the
    // documentation of the probes I ran manually during development, one per branch, recorded so a
    // future reader can see WHICH test reds WHICH mutation:
    //
    // PROBE-1 (breaks NoNat): comment out the `mapped == local` early return in `classify_nat`.
    //   REDS: no_nat_requires_mapped_equal_to_local (the equality case stops returning NoNat) and
    //         rows 0/1 of classify_nat_decision_table.
    // PROBE-2 (breaks CGNAT): change `is_rfc6598_cgnat` to `false`. REDS: cgnat_branch_fires...
    //   and the two CGNAT rows of the table.
    // PROBE-3 (breaks BehindNAT — RFC 1918 half): make `is_rfc1918_private` return `false`. REDS:
    //   behind_nat_is_answerable_offline_from_rfc1918_local (RFC 1918 + None falls to Unknown),
    //   behind_nat_branch_fires_on_rfc1918_local_with_differing_mapped (the in-block rows), and
    //   table rows where local is RFC 1918. Note: the boundary cases in the latter test are
    //   catch-all-only and stay green under this probe alone — they need PROBE-4 to red.
    // PROBE-4 (breaks BehindNAT — catch-all): change the final `match` to always return
    //   `Unknown`. REDS: behind_nat_branch_fires...'s boundary cases, the catch-all table row, and
    //   ipv4_mapped_ipv6_equality_still_yields_no_nat.
    // PROBE-5 (breaks Unknown): change the final `match` to always return `BehindNat` (i.e. drop
    //   the None ⇒ Unknown arm). REDS: unknown_branch_fires_only_when_no_mapped_and_non_private_local
    //   and the Unknown row of classify_nat_decision_table.
    //
    // After each probe the production code was reverted and the full net:: suite re-run green. The
    // final scan for live-mutation / fixme / hack markers in this file came back empty (see report).
}
