//! The multi-relay Nostr client (spec §Relay Model, §Discovery). Ports and hardens the M0
//! spike's proven `Client::builder → add_relay → try_connect → send_event → fetch_events`
//! sequence into the production client `hb-it` drives now and `hb-app` will drive in M4.
//!
//! Two disciplines from M0 are load-bearing: `connect()` returns *before* every relay's
//! websocket handshake (the first relay's handshake gates the return, QURATOR-168; the rest
//! keep connecting in the background), and it still refuses to proceed if NO relay came up;
//! and a relay's per-event accept/reject is surfaced (the `Output.success`/`failed` split) so a
//! silent drop or an explicit `OK: false` is observable (AB8), never swallowed.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hb_core::{Identity, RelayRateLimiter};
use nostr_sdk::prelude::*;
use serde::Serialize;

use crate::error::NetError;

/// Ceiling on any single throttle sleep, so the sleep-and-retry loop always re-checks the bucket
/// promptly (with the production refill a real wait is under a second; this is a floor against a
/// mis-set constant asking for an implausibly long single sleep — [`RelayRateLimiter::new`] already
/// clamps the config, so this is belt-and-suspenders).
const MAX_THROTTLE_SLEEP: Duration = Duration::from_secs(2);

/// Poll cadence of the first-success race in [`RelayClient::connect`] (QURATOR-168): a healthy
/// relay's handshake lands in tens of milliseconds, so a 50 ms tick adds at most one tick of
/// latency to the fast path, while the slow path stays bounded by the join's own `timeout`.
const CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A connected multi-relay client.
pub struct RelayClient {
    client: Client,
    relays: Vec<String>,
    /// Ban-avoidance pacing for the write path (spec §Relay Model;
    /// [[large_collection_intent_2026-07-11]]). A token bucket, NOT the announce min-interval —
    /// ordinary writes clear the burst instantly, only a large-collection flood is paced. Behind a
    /// `std::sync::Mutex` because [`publish`](Self::publish) takes `&self` on the shared `Arc` and
    /// the critical section is pure arithmetic held **only** across `try_acquire` (never across the
    /// sleep or the network send), so it needs no async lock.
    ///
    /// **Per-client, by design (accepted residual — Chorus).** A fresh full burst is minted on every
    /// `RelayClient::connect`; a rebuild (Settings relay-set change, or a dead-pool reconnect) resets
    /// the bucket. Rebuilds are rare, and the one edge — a huge publish interrupted mid-stream by a
    /// drop then resumed on a fresh burst — is low-risk (a relay would have to track rate across the
    /// reconnect). Not worth a session-lifetime singleton; the un-bypassable per-write chokepoint is
    /// the property that matters.
    limiter: Mutex<RelayRateLimiter>,
    /// Monotonic anchor: `start.elapsed()` is the `now` fed to the limiter, so a wall-clock jump
    /// cannot skew pacing.
    start: Instant,
}

/// Per-relay accept/reject split for a single publish.
#[derive(Debug, Clone)]
pub struct PublishOutcome {
    /// Relays that accepted the event (`OK: true`).
    pub accepted: Vec<String>,
    /// Relays that rejected it, with the reason string they returned.
    pub rejected: Vec<(String, String)>,
}

/// Live per-relay reachability on the data path (M12 W1, Decision D) — so a "–"/Offline read can
/// say **why** (rate-limited vs unreachable vs connecting), not just fail identically. Serialized
/// camelCase for the Settings relay list + the chip "why" hint.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayHealth {
    pub url: String,
    /// A stable lowercase status label (`connected` / `connecting` / `disconnected` / …).
    pub status: String,
    /// Whether the relay is currently connected (the green/grey dot).
    pub connected: bool,
    /// A human-readable last error, when the pool surfaces one (else `None` — the status label is
    /// the primary signal; nostr-sdk's stats carry no error string in this version).
    pub last_error: Option<String>,
}

/// A stable lowercase label for a [`RelayStatus`] (the wire contract for the Settings relay rows).
fn status_label(s: RelayStatus) -> &'static str {
    match s {
        RelayStatus::Initialized => "initialized",
        RelayStatus::Pending => "pending",
        RelayStatus::Connecting => "connecting",
        RelayStatus::Connected => "connected",
        RelayStatus::Disconnected => "disconnected",
        RelayStatus::Terminated => "terminated",
        RelayStatus::Banned => "banned",
        RelayStatus::Sleeping => "sleeping",
    }
}

/// Canonical form of a relay URL for logging + equality: lowercase, trailing slash stripped.
///
/// QURATOR-66: the v0.12.9 bug was a raw `===` on relay URLs — `wss://a/` and `wss://a` compared
/// unequal and a presence beacon published to one was read from the other as "not sent". Logging
/// the canonical form alongside the raw one (as every connect/publish site now does) makes a
/// normalization mismatch visible in a log the user pastes. This is NOT used for relay-pool
/// equality (that is order-insensitive set comparison in `hb-app::net`); it is a logging aid.
fn canonical_relay_url(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    lower.trim_end_matches('/').to_string()
}

/// Log the per-relay outcome of a publish (QURATOR-66). strfry rejects future-dated and NIP-40-
/// expired events at the **write** boundary, and that rejection was previously invisible — the
/// caller saw `PublishOutcome.rejected` only as an error string when ALL relays rejected. Logging
/// each relay's accept/reject at DEBUG (accept) / WARN (reject) with the relay's own reason makes a
/// partial failure (one relay accepted, one rejected the future-dated event) visible in the log.
///
/// Per-iteration of a hot loop this is bounded: a publish touches a fixed relay set, not every
/// event in a large collection (the split publish goes through one `publish`/`publish_to` call per
/// part, but the relay set is the same each time, so this is one INFO/WARN line per relay per call —
/// readable at the default level for an ordinary session).
fn log_publish_outcome(kind: u64, outcome: &PublishOutcome) {
    let accepted = outcome.accepted.len();
    let rejected = outcome.rejected.len();
    if rejected == 0 {
        tracing::debug!(kind, accepted, "publish accepted by all relays");
    } else if accepted == 0 {
        // All-reject is an error path (the caller returns Err); log at WARN with the reasons.
        tracing::warn!(kind, rejected, "publish rejected by every relay");
        for (url, why) in &outcome.rejected {
            tracing::warn!(relay = %canonical_relay_url(url), reason = %why, "publish rejected");
        }
    } else {
        // Partial: some accepted, some rejected. This is the strfry-write-boundary case the ticket
        // names — log the rejects so a future-dated/expired event that one relay refuses while
        // others accept is not silently swallowed.
        tracing::info!(kind, accepted, rejected, "publish: partial accept across relays");
        for (url, why) in &outcome.rejected {
            tracing::warn!(relay = %canonical_relay_url(url), reason = %why, "publish rejected by this relay");
        }
    }
}

/// Whether a relay pool is still **live** (M12 W1, Decision A-recovery): live if it holds at least
/// one relay that is **not** in a dead terminal state (`Terminated`/`Banned` — "no retry will
/// occur"). `Disconnected` is transient (nostr-sdk auto-reconnects — "another attempt will occur
/// soon"), so it counts as live. A fully-terminated pool (e.g. after `disconnect()` on exit, or
/// every relay banned) is dead → `net::client` rebuilds it rather than returning a corpse. Pure, so
/// the dead-pool classification is unit-tested without a relay.
pub fn pool_is_live(statuses: &[RelayStatus]) -> bool {
    !statuses.is_empty()
        && statuses
            .iter()
            .any(|s| !matches!(s, RelayStatus::Terminated | RelayStatus::Banned))
}

/// Whether **any** relay is currently connected (`Connected` only — the state that can actually
/// serve a REQ). The gap between this and [`pool_is_live`] is exactly the relay-outage hole
/// (QURATOR-80's class, transient form): `pool_is_live` deliberately counts `Disconnected` as live
/// because nostr-sdk *will retry*, which is the right call for rebuild policy — but during that
/// retry window a fetch is issued against a pool where no relay can answer, and nostr-relay-pool
/// (0.43.1) resolves that as `Ok(no events)`, not an error: `stream_events_targeted` logs-and-drops
/// every per-relay failure (`tracing::error!`, never propagated) and an all-dropped fetch yields an
/// empty stream. Zero connected relays ⇒ the empty answer carries no evidence any relay was even
/// asked. Pure, so the classification is unit-tested without a relay.
fn any_relay_connected(statuses: &[RelayStatus]) -> bool {
    statuses.iter().any(|s| matches!(s, RelayStatus::Connected))
}

/// The per-tick decision of [`RelayClient::connect`]'s first-success race (QURATOR-168). Pure,
/// so the proceed-vs-keep-waiting policy is unit-testable without standing up a relay.
#[derive(Debug, PartialEq, Eq)]
enum ConnectRaceDecision {
    /// A relay reached `Connected` — proceed now; stragglers keep connecting in the background.
    Proceed,
    /// No relay is up yet and the join is still driving handshakes — sleep and re-check.
    KeepWaiting,
    /// The join resolved without any relay observed `Connected` — await it for the full
    /// per-relay diagnostics path.
    AwaitOutcome,
}

/// Priority is load-bearing: `Proceed` must outrank a finished join, because a relay can land
/// `Connected` in the very tick the join resolves — testing `join_finished` first would take the
/// slow path and re-pay exactly the stragglers' wait the race exists to skip.
fn connect_race_tick(statuses: &[RelayStatus], join_finished: bool) -> ConnectRaceDecision {
    if any_relay_connected(statuses) {
        ConnectRaceDecision::Proceed
    } else if join_finished {
        ConnectRaceDecision::AwaitOutcome
    } else {
        ConnectRaceDecision::KeepWaiting
    }
}

impl RelayClient {
    /// Connect to `relays`, returning as soon as the **first** relay completes its websocket
    /// handshake (QURATOR-168), or up to `timeout` if none does — in which case this fails with
    /// the per-relay reasons. Tradeoff, accepted by the 2026-09-02 owner ruling ("fix the
    /// plumbing, not the bound"): slower-but-healthy relays may still be handshaking when the
    /// first publish goes out, and nostr-relay-pool surfaces that per relay as an
    /// `Output.failed` "relay not connected" entry (the M0 finding) — a straggler's first
    /// publish can be dropped exactly as M0 described. The spawned join keeps driving those
    /// stragglers to completion in the background, so later publishes reach them; and at least
    /// one relay is always fully `Connected` before we return, so nothing is published into a
    /// fully-dead pool. The alternative — waiting for every relay — cost the full `timeout`
    /// whenever one relay in the set was dead, which R1 measured at 43s against a 40s bound.
    pub async fn connect(
        identity: &Identity,
        relays: &[String],
        timeout: Duration,
    ) -> Result<Self, NetError> {
        if relays.is_empty() {
            return Err(NetError::NoRelayConnected("no relays configured".into()));
        }
        tracing::info!(relay_count = relays.len(), timeout_ms = timeout.as_millis() as u64, "relay client: connecting");
        let client = Client::builder().signer(identity.keys().clone()).build();
        for r in relays {
            // QURATOR-66: log the canonical form alongside the raw URL. The v0.12.9 bug was a raw
            // `===` comparison on relay URLs (trailing slash, etc.) that made two "same" relays
            // compare unequal, and the diagnostics panel could not show it. Logging both makes a
            // URL-normalization mismatch visible in the log a user pastes.
            let canon = canonical_relay_url(r);
            tracing::debug!(raw_url = %r, canonical_url = %canon, "relay client: add_relay");
            client
                .add_relay(r.as_str())
                .await
                .map_err(|e| NetError::Client(format!("add_relay({r}): {e}")))?;
        }
        // QURATOR-168 (owner ruling 2026-09-02: fix the plumbing, not the bound): nostr-relay-pool
        // 0.43.1's `try_connect` joins EVERY per-relay handshake, so one blackholing relay cost the
        // full `timeout` even when a healthy relay finished in milliseconds (the WAN-R R1 row
        // measured 43.01s against a 40s bound). Race the join against a first-success poller and
        // return the moment any relay reaches `Connected`.
        //
        // The join is SPAWNED, not `select!`ed inline: dropping an in-flight `try_connect`
        // strands its relays in `Connecting`, and 0.43.1's `RelayStatus::can_connect()` covers
        // only {Initialized, Terminated, Sleeping} — nothing would ever pick them up again (the
        // pool monitor is notification-only). Spawned, the detached join keeps driving every
        // handshake to completion, so on the early win the stragglers still come up for later
        // publishes and the pool never rides a single relay.
        let join = tokio::spawn({
            let client = client.clone();
            async move { client.try_connect(timeout).await }
        });
        loop {
            let relays_now = client.relays().await;
            let statuses: Vec<RelayStatus> = relays_now.values().map(|r| r.status()).collect();
            match connect_race_tick(&statuses, join.is_finished()) {
                ConnectRaceDecision::Proceed => {
                    // Contract preserved (QURATOR-66): the per-connected-relay info! line names the
                    // relays that are up, in the same canonical form the failure path logs.
                    let connected: Vec<String> = relays_now
                        .iter()
                        .filter(|(_, r)| matches!(r.status(), RelayStatus::Connected))
                        .map(|(url, _)| canonical_relay_url(&url.to_string()))
                        .collect();
                    for url in &connected {
                        tracing::info!(relay = %url, "relay client: connected");
                    }
                    tracing::info!(
                        connected = connected.len(),
                        total = statuses.len(),
                        "relay client: first relay connected, proceeding (stragglers keep connecting)"
                    );
                    return Ok(Self {
                        client,
                        relays: relays.to_vec(),
                        limiter: Mutex::new(RelayRateLimiter::relay_writes()),
                        start: Instant::now(),
                    });
                }
                ConnectRaceDecision::KeepWaiting => {}
                ConnectRaceDecision::AwaitOutcome => break,
            }
            tokio::time::sleep(CONNECT_POLL_INTERVAL).await;
        }
        let conn = match join.await {
            Ok(conn) => conn,
            Err(e) => {
                // A panicked join must not surface as a bare NoRelayConnected with an empty failed
                // set — name the join error so the log cannot be mistaken for relay diagnostics.
                tracing::error!(error = %e, "relay client: try_connect join panicked");
                return Err(NetError::NoRelayConnected(format!("try_connect join failed: {e}")));
            }
        };
        // Log the per-relay connect outcome: which relays came up vs failed. The failed set carries a
        // reason string per relay — currently surfaced only in the error path; logging it here means a
        // partial connect (some relays up, some down) is visible without a full failure.
        for url in &conn.success {
            tracing::info!(relay = %canonical_relay_url(&url.to_string()), "relay client: connected");
        }
        for (url, why) in &conn.failed {
            tracing::warn!(relay = %canonical_relay_url(&url.to_string()), reason = %why, "relay client: connect failed");
        }
        if conn.success.is_empty() {
            tracing::error!(
                tried = relays.len(),
                failed = conn.failed.len(),
                "relay client: no relay completed the handshake"
            );
            return Err(NetError::NoRelayConnected(format!("{:?}", conn.failed)));
        }
        Ok(Self {
            client,
            relays: relays.to_vec(),
            limiter: Mutex::new(RelayRateLimiter::relay_writes()),
            start: Instant::now(),
        })
    }

    /// Block until the write governor grants a token, then return so the caller may send. A token
    /// bucket, so a full burst returns immediately (no interactive write is paced — owner ruling
    /// 2026-07-12); only a sustained flood sleeps, and it always *sends* (never rejects). Each
    /// iteration takes the lock **only** for the pure decision (via [`throttle_step`], dropped before
    /// the `.await`) so it holds no lock across the sleep.
    async fn throttle(&self) {
        let mut paced = false;
        while let Some(sleep) = throttle_step(&self.limiter, self.start.elapsed().as_secs_f64()) {
            if !paced {
                // Observability (Chorus gemini/opencode): a large paced publish must not stall
                // silently. Logged once per publish that actually waits — the common burst path never
                // reaches here, so no cost on ordinary interactive writes. debug ⇒ off by default.
                tracing::debug!(
                    sleep_ms = sleep.as_millis() as u64,
                    "relay-write governor engaged (burst spent) — pacing this publish to stay under relay rate limits"
                );
                paced = true;
            }
            tokio::time::sleep(sleep).await;
        }
    }

    /// Publish a pre-signed hb-core event to every write-relay, returning the per-relay
    /// accept/reject split. Errors only if **no** relay accepted (an all-reject / all-drop).
    pub async fn publish(&self, event: &Event) -> Result<PublishOutcome, NetError> {
        self.throttle().await;
        let kind = event.kind.as_u16() as u64;
        let output = self
            .client
            .send_event(event)
            .await
            .map_err(|e| NetError::Client(format!("send_event(kind {}): {e}", event.kind.as_u16())))?;
        let outcome = PublishOutcome {
            accepted: output.success.iter().map(|u| u.to_string()).collect(),
            rejected: output.failed.iter().map(|(u, why)| (u.to_string(), why.clone())).collect(),
        };
        log_publish_outcome(kind, &outcome);
        if outcome.accepted.is_empty() {
            return Err(NetError::PublishRejected(format!("{:?}", outcome.rejected)));
        }
        Ok(outcome)
    }

    /// Publish a pre-signed event to a **targeted** subset of relays (M12 W2, Decision F). The
    /// persistent shared client accretes relays over a session (peer outboxes from prior browses),
    /// so a bare [`publish`](Self::publish) would broadcast a gift-wrap DM to **every** connected
    /// relay — unnecessary metadata spread. Delivery targets `relays` only (the recipient's
    /// read-relays ∪ your write/seed). The caller `ensure_relays`'s the set first so it is connected.
    /// Errors only if **no** targeted relay accepted (mirrors [`publish`](Self::publish)).
    pub async fn publish_to(&self, event: &Event, relays: &[String]) -> Result<PublishOutcome, NetError> {
        if relays.is_empty() {
            return Err(NetError::NoRelayConnected("no target relays for publish_to".into()));
        }
        self.throttle().await;
        let kind = event.kind.as_u16() as u64;
        let output = self
            .client
            .send_event_to(relays.iter().map(|s| s.as_str()), event)
            .await
            .map_err(|e| NetError::Client(format!("send_event_to(kind {}): {e}", event.kind.as_u16())))?;
        let outcome = PublishOutcome {
            accepted: output.success.iter().map(|u| u.to_string()).collect(),
            rejected: output.failed.iter().map(|(u, why)| (u.to_string(), why.clone())).collect(),
        };
        log_publish_outcome(kind, &outcome);
        if outcome.accepted.is_empty() {
            return Err(NetError::PublishRejected(format!("{:?}", outcome.rejected)));
        }
        Ok(outcome)
    }

    /// Whether this client's pool is still **live** (M12 W1, Decision A-recovery): at least one relay
    /// not in a dead terminal state. `net::client` rebuilds a dead client rather than returning a
    /// corpse that fails every command silently.
    pub async fn is_live(&self) -> bool {
        let relays = self.client.relays().await;
        let statuses: Vec<RelayStatus> = relays.values().map(|r| r.status()).collect();
        pool_is_live(&statuses)
    }

    /// Live per-relay reachability for the **configured** relay set (M12 W1, Decision D) — one
    /// [`RelayHealth`] per configured relay (peer-outbox relays added by `ensure_relays` are NOT
    /// reported here; the Settings list shows the user's own set). A configured relay missing from
    /// the live pool reports `disconnected`.
    pub async fn relay_status(&self) -> Vec<RelayHealth> {
        let live = self.client.relays().await;
        self.relays
            .iter()
            .map(|url| {
                let want = url.trim_end_matches('/');
                let found = live.iter().find(|(u, _)| u.to_string().trim_end_matches('/') == want);
                let (status, connected) = match found {
                    Some((_, r)) => {
                        let s = r.status();
                        // QURATOR-66: a relay dropping to a dead terminal state (Terminated/Banned)
                        // is the SPOF signal — log it at WARN so a user report shows which relay died
                        // and when. The transient Disconnected is DEBUG (nostr-sdk auto-reconnects).
                        if matches!(s, RelayStatus::Terminated | RelayStatus::Banned) {
                            tracing::warn!(
                                relay = %canonical_relay_url(url),
                                status = status_label(s),
                                "relay reached a dead terminal state — it will not auto-reconnect"
                            );
                        } else if s == RelayStatus::Disconnected {
                            tracing::debug!(
                                relay = %canonical_relay_url(url),
                                "relay disconnected (transient — nostr-sdk will reconnect)"
                            );
                        }
                        (status_label(s).to_string(), r.is_connected())
                    }
                    None => ("disconnected".to_string(), false),
                };
                RelayHealth { url: url.clone(), status, connected, last_error: None }
            })
            .collect()
    }

    /// Fetch events by `filter`, **deduped by event id** across the relay set (a peer's event
    /// pulled from two relays collapses to one). A filter constraining nothing is refused before
    /// the query — an unbounded fetch is never issued.
    ///
    /// An all-relays-unreachable fetch is an **Err, not an empty Ok**. nostr-relay-pool 0.43.1
    /// swallows per-relay failures (`stream_events_targeted` logs them and returns a stream that
    /// yields nothing), so a fetch against a pool where every relay dropped resolves
    /// `Ok(vec![])` — indistinguishable from a genuine "nothing matches". Callers that cache by
    /// result (the Topics directory paint, QURATOR-145) would then overwrite a warm tree with the
    /// empty state during an outage, the exact failure [`pool_is_live`]'s `Disconnected`-is-live
    /// ruling leaves open on a shared warm client. The post-fetch status check closes it: with
    /// **zero** relays `Connected` the answer cannot have come from any relay, so it is refused.
    /// One-or-more connected relays returning nothing stays `Ok(vec![])` — that IS a genuine
    /// empty, and turning it into an Err would be the opposite bug.
    pub async fn fetch(&self, filter: Filter, timeout: Duration) -> Result<Vec<Event>, NetError> {
        if filter.is_empty() {
            return Err(NetError::EmptyFilter);
        }
        let events = self
            .client
            .fetch_events(filter, timeout)
            .await
            .map_err(|e| NetError::Client(e.to_string()))?;
        // The check runs on the EMPTY result only, and after the fetch: a non-empty answer proves a
        // relay answered (and a connected pool that went down mid-fetch still returned its events),
        // so only `Ok(vec![])` can be the outage masquerading as success.
        if events.is_empty() {
            let relays = self.client.relays().await;
            let statuses: Vec<RelayStatus> = relays.values().map(|r| r.status()).collect();
            if !any_relay_connected(&statuses) {
                return Err(NetError::NoRelayConnected(format!(
                    "{} relay(s) configured, none connected during the fetch",
                    statuses.len()
                )));
            }
        }
        Ok(dedup_by_id(events))
    }

    /// Fetch events by `filter` from a **targeted** subset of relays only (M16 W2 — the big-relay
    /// carrier). The full-listing family the owner publishes to their big relay shares its `d=slug`
    /// with the truncated paywall teaser on the public relays; a pool-wide [`fetch`](Self::fetch)
    /// would pull both and the renderer would prefer the plain-unsplit teaser (its `is_plain_unsplit`
    /// guard). Reading the big relay exclusively keeps the split family intact. Deduped by id; an
    /// empty relay set or a constraint-free filter is refused before the query.
    pub async fn fetch_from(
        &self,
        relays: &[String],
        filter: Filter,
        timeout: Duration,
    ) -> Result<Vec<Event>, NetError> {
        if relays.is_empty() {
            return Err(NetError::NoRelayConnected("no target relays for fetch_from".into()));
        }
        if filter.is_empty() {
            return Err(NetError::EmptyFilter);
        }
        let events = self
            .client
            .fetch_events_from(relays.iter().map(|s| s.as_str()), filter, timeout)
            .await
            .map_err(|e| NetError::Client(e.to_string()))?;
        Ok(dedup_by_id(events))
    }

    /// The relay set passed to `connect`. (Relays added later via `ensure_relays` are connected on
    /// the underlying client but not recorded here — this getter reports the initial configured set.)
    pub fn relays(&self) -> &[String] {
        &self.relays
    }

    /// Ensure the client is connected to every relay in `relays`, adding + connecting any not in the
    /// configured set. This is how the browse flow **acts on** NIP-65 resolution — connecting to a
    /// peer's advertised outbox before fetching their events, so a peer who publishes only to their
    /// own relays is still reachable. Best-effort and idempotent: a relay that fails to connect is
    /// skipped (existing connections keep working); `add_relay` is a no-op for already-known relays.
    pub async fn ensure_relays(&self, relays: &[String], timeout: Duration) -> Result<(), NetError> {
        let mut added = false;
        for r in relays {
            if !self.relays.contains(r) && self.client.add_relay(r.as_str()).await.is_ok() {
                tracing::debug!(relay = %canonical_relay_url(r), "ensure_relays: added peer relay to the pool");
                added = true;
            }
        }
        if added {
            // Connect the newly-added relays; already-connected ones are unaffected.
            let _ = self.client.try_connect(timeout).await;
        }
        Ok(())
    }

    /// Close all relay connections.
    pub async fn disconnect(self) {
        tracing::info!("relay client: disconnecting (session end)");
        self.client.disconnect().await;
    }
}

/// One iteration of [`RelayClient::throttle`], factored out so the lock / sleep-cap / fail-open
/// logic is unit-tested without a live relay. Returns `Some(sleep)` (already clamped to
/// [`MAX_THROTTLE_SLEEP`]) to wait then retry, or `None` when a token was granted / the lock is
/// poisoned (fail open — pacing must never wedge a publish). The `now` is monotonic seconds.
///
/// The wait is clamped **as an `f64` before** constructing the `Duration`: `try_acquire` is pure
/// math (hb-net owns bounding the sleep, per the crate split) and can legitimately return an
/// astronomically large wait for a degenerate limiter config — and `Duration::from_secs_f64`
/// *panics* above ~1.8e19s. Clamping post-construction (Chorus codex + gemini) would never run.
fn throttle_step(limiter: &Mutex<RelayRateLimiter>, now: f64) -> Option<Duration> {
    match limiter.lock() {
        Ok(mut lim) => lim
            .try_acquire(now)
            .map(|secs| Duration::from_secs_f64(secs.clamp(0.0, MAX_THROTTLE_SLEEP.as_secs_f64()))),
        Err(_) => None,
    }
}

/// Collapse events sharing an id to a single occurrence, preserving first-seen order — the
/// multi-relay dedup invariant (a hostile or redundant relay returning a duplicate can't inflate
/// results). Pure, so it is unit-tested without a relay.
pub fn dedup_by_id<I>(events: I) -> Vec<Event>
where
    I: IntoIterator<Item = Event>,
{
    let mut seen: HashSet<EventId> = HashSet::new();
    events.into_iter().filter(|e| seen.insert(e.id)).collect()
}

/// Relay-fetch headroom over the client-side `SEARCH_CAP` (M20 W3). The relay filter is an **OR**
/// over `tags ∪ content_types`; the strict AND-tag match runs client-side (`teaser_matches`), so the
/// relay's response window is mostly loose single-tag matches that get discarded. This explicit
/// `.limit()` makes the fetch budget **ours**, not the relay's internal default (strfry
/// `maxFilterLimit` defaults to 500 — without a declared limit that cap silently decided which teasers
/// came back, and a full-AND-match teaser older than 500 loose hits was evicted before the client
/// filter ever saw it). Sized at 10× the visible cap so even after the AND filter discards the bulk of
/// loose matches, the strict survivors comfortably exceed `SEARCH_CAP`. A relay that clamps below this
/// simply returns fewer (the "showing first N" affordance surfaces that).
pub const TEASER_SEARCH_FETCH_LIMIT: usize = 1000;

/// Build a teaser tag-search filter. Refused before any query (DISC4) when it constrains
/// nothing — empty tags **and** empty content-types. The relay returns the OR-union of all
/// `#t` terms; the caller intersects tags / unions content-types client-side (DISC1). An explicit
/// `.limit()` ([`TEASER_SEARCH_FETCH_LIMIT`]) declares the fetch budget so the relay's own response
/// cap can't silently evict strict-AND matches (M20 W3 — the eviction regression).
pub fn teaser_search_filter(
    tags: &[String],
    content_types: &[String],
) -> Result<Filter, NetError> {
    if tags.is_empty() && content_types.is_empty() {
        return Err(NetError::EmptyFilter);
    }
    let all: Vec<String> = tags.iter().chain(content_types).cloned().collect();
    Ok(Filter::new()
        .kind(Kind::from_u16(hb_core::event::KIND_TEASER))
        .hashtags(all)
        .limit(TEASER_SEARCH_FETCH_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::event::{build_teaser, Teaser};

    fn ev(name: &str) -> Event {
        let id = Identity::generate();
        build_teaser(
            &id,
            &Teaser {
                display_name: name.into(),
                bio: String::new(),
                tags: vec!["anime".into()],
                content_types: vec!["video".into()],
                picture: None,
            },
            true,
        )
        .unwrap()
    }

    #[test]
    fn dedup_collapses_same_id_across_relays() {
        let a = ev("a");
        let b = ev("b");
        // The same event fetched from two relays + a distinct one → two unique.
        let deduped = dedup_by_id(vec![a.clone(), a.clone(), b.clone()]);
        assert_eq!(deduped.len(), 2);
        assert!(deduped.iter().any(|e| e.id == a.id));
        assert!(deduped.iter().any(|e| e.id == b.id));
    }

    #[test]
    fn dedup_preserves_first_seen_order() {
        let a = ev("a");
        let b = ev("b");
        let deduped = dedup_by_id(vec![a.clone(), b.clone(), a.clone()]);
        assert_eq!(deduped[0].id, a.id);
        assert_eq!(deduped[1].id, b.id);
    }

    #[test]
    fn empty_filter_rejected_before_query() {
        // DISC4: empty tags AND empty content-types is refused before any relay query.
        assert!(matches!(teaser_search_filter(&[], &[]), Err(NetError::EmptyFilter)));
    }

    #[test]
    fn teaser_filter_constrains_kind_and_tags() {
        let f = teaser_search_filter(&["anime".into()], &["video".into()]).unwrap();
        assert!(!f.is_empty(), "a constrained filter is not empty");
    }

    #[test]
    fn teaser_filter_declares_an_explicit_limit_m20_w3() {
        // M20 W3 eviction regression: without an explicit `.limit()` the relay's own response cap
        // (strfry default 500) decided which teasers came back, so a full-AND-match teaser older than
        // 500 loose single-tag matches was dropped before the client-side AND filter ever saw it. The
        // filter must now declare the budget — it is ours, not the relay's.
        let f = teaser_search_filter(&["anime".into()], &["video".into()]).unwrap();
        assert_eq!(f.limit, Some(TEASER_SEARCH_FETCH_LIMIT), "fetch budget is declared explicitly");
    }

    #[test]
    fn pool_live_when_any_relay_not_terminal() {
        // M12 W1 Decision A-recovery: a pool is live if ANY relay is recoverable. Connected,
        // Connecting, and even Disconnected (transient — nostr-sdk auto-reconnects) are all "live".
        assert!(pool_is_live(&[RelayStatus::Connected]));
        assert!(pool_is_live(&[RelayStatus::Connecting]));
        assert!(pool_is_live(&[RelayStatus::Disconnected]), "Disconnected is transient, not dead");
        assert!(pool_is_live(&[RelayStatus::Terminated, RelayStatus::Connected]), "one live relay keeps the pool live");
    }

    #[test]
    fn pool_dead_when_all_terminal_or_empty() {
        // A fully-terminated/banned pool (e.g. after disconnect() on exit) is dead → net::client
        // rebuilds it rather than returning a corpse. An empty pool is dead too.
        assert!(!pool_is_live(&[RelayStatus::Terminated]));
        assert!(!pool_is_live(&[RelayStatus::Terminated, RelayStatus::Banned]));
        assert!(!pool_is_live(&[]), "no relays = not live");
    }

    // ── The outage-vs-empty discriminator at the fetch seam (QURATOR-145 review, HIGH) ─────────

    #[test]
    fn any_relay_connected_false_for_every_non_connected_state() {
        // The outage shape: a warm shared client whose relays ALL dropped. `pool_is_live` still
        // says live (Disconnected is transient, nostr-sdk retries) so no rebuild happens — yet not
        // one relay can serve a REQ. Also pinned: Terminated/Banned/empty, and the never-connected
        // states (Initialized/Pending/Connecting), which all reach the same "no answer possible".
        for statuses in [
            vec![],
            vec![RelayStatus::Disconnected],
            vec![RelayStatus::Disconnected, RelayStatus::Disconnected],
            vec![RelayStatus::Terminated],
            vec![RelayStatus::Banned, RelayStatus::Terminated],
            vec![RelayStatus::Initialized],
            vec![RelayStatus::Pending],
            vec![RelayStatus::Connecting],
        ] {
            assert!(
                !any_relay_connected(&statuses),
                "no Connected relay in {statuses:?} ⇒ nothing can answer a fetch"
            );
        }
    }

    #[test]
    fn any_relay_connected_true_when_at_least_one_relay_is_connected() {
        // A pool with one live connection CAN have answered — even if other relays are down or
        // dead. This is the case that must stay `Ok(vec![])` (a genuine empty), so the
        // discriminator has to return true here or genuine empties become false errors.
        for statuses in [
            vec![RelayStatus::Connected],
            vec![RelayStatus::Connected, RelayStatus::Disconnected],
            vec![RelayStatus::Disconnected, RelayStatus::Connected, RelayStatus::Terminated],
            vec![RelayStatus::Connected, RelayStatus::Connecting],
        ] {
            assert!(any_relay_connected(&statuses), "{statuses:?} holds a connected relay");
        }
    }

    #[test]
    fn outage_pool_is_live_but_cannot_answer() {
        // The seam the QURATOR-145 review found, pinned as one assertion: the all-Disconnected
        // pool is simultaneously LIVE (rebuild policy must not churn it — nostr-sdk retries) and
        // UNABLE to answer a fetch (no Connected relay). `fetch` therefore has to discriminate on
        // `any_relay_connected`, NOT on `pool_is_live`; gating the Err on liveness would either
        // never fire (Disconnected is live) or force a rebuild churn the M12 ruling forbids.
        let outage = [RelayStatus::Disconnected, RelayStatus::Disconnected];
        assert!(pool_is_live(&outage), "Disconnected is transient — the pool must not be rebuilt");
        assert!(!any_relay_connected(&outage), "yet no relay can serve the REQ");
    }

    // ── The first-success race at the connect seam (QURATOR-168) ─────────────────────────────
    //
    // MUTATION (for the orchestrator's red proof): in `connect_race_tick`, swap the branch order
    // so the join check runs first —
    //     `if any_relay_connected(statuses) {`  →  `if join_finished {` returning AwaitOutcome,
    //     `} else if join_finished {`            →  `} else if any_relay_connected(statuses) {`
    // This still COMPILES and must redden `connect_race_tick_proceed_outranks_a_finished_join`
    // (the tick would answer AwaitOutcome for [Connected, Terminated] with the join finished).
    // A second, independent mutation: make the `join_finished` branch return KeepWaiting instead
    // of AwaitOutcome — that reddens `connect_race_tick_awaits_outcome_when_the_join_resolves…`
    // and hangs the async `connect_still_fails…` test (never reaching the diagnostics path).

    #[test]
    fn connect_race_tick_proceeds_the_instant_any_relay_is_connected() {
        // The race's whole point (the R1 shape: one healthy relay up, one blackholing): the tick
        // must say Proceed, never KeepWaiting, so connect() never pays the straggler's wait.
        assert_eq!(
            connect_race_tick(&[RelayStatus::Connected, RelayStatus::Connecting], false),
            ConnectRaceDecision::Proceed
        );
        assert_eq!(
            connect_race_tick(&[RelayStatus::Terminated, RelayStatus::Connected], false),
            ConnectRaceDecision::Proceed
        );
    }

    #[test]
    fn connect_race_tick_keeps_waiting_while_no_relay_is_up_and_the_join_runs() {
        // Stragglers mid-handshake (Connecting/Initialized/Pending) are NOT connected: returning
        // on them would recreate the half-connected publish the M0 finding forbids. The join is
        // still driving them, so the only honest answer is keep waiting.
        assert_eq!(
            connect_race_tick(&[RelayStatus::Connecting, RelayStatus::Initialized], false),
            ConnectRaceDecision::KeepWaiting
        );
        assert_eq!(
            connect_race_tick(&[RelayStatus::Disconnected, RelayStatus::Terminated], false),
            ConnectRaceDecision::KeepWaiting
        );
    }

    #[test]
    fn connect_race_tick_awaits_outcome_when_the_join_resolves_with_nothing_connected() {
        // The timeout-expiry shape: the join finished, nothing connected. Only now does connect()
        // take the full-diagnostics path (per-relay failure reasons → NoRelayConnected).
        assert_eq!(
            connect_race_tick(&[RelayStatus::Terminated, RelayStatus::Terminated], true),
            ConnectRaceDecision::AwaitOutcome
        );
        assert_eq!(
            connect_race_tick(&[], true),
            ConnectRaceDecision::AwaitOutcome
        );
    }

    #[test]
    fn connect_race_tick_proceed_outranks_a_finished_join() {
        // Load-bearing priority: a relay can land `Connected` in the very tick the join resolves
        // (status is set to Connected inside the join's own execution, before it completes).
        // Testing `join_finished` first would take the slow path and re-pay the stragglers' wait —
        // exactly what the race exists to skip.
        assert_eq!(
            connect_race_tick(&[RelayStatus::Connected, RelayStatus::Terminated], true),
            ConnectRaceDecision::Proceed
        );
    }

    /// The timeout path through the REAL `connect` loop (QURATOR-168): no relay ever reaches
    /// `Connected`, so the poller never fires the early return; the spawned join resolves at the
    /// timeout, the tick says AwaitOutcome, and the caller still gets the per-relay
    /// `NoRelayConnected` with the QURATOR-66 diagnostics logged. Uses `ws://127.0.0.1:1`
    /// (connection refused, nothing listens) so it needs no relay server and no network egress —
    /// the same trick as `fetch_errors_when_no_relay_is_connected` above.
    #[tokio::test]
    async fn connect_still_fails_with_diagnostics_when_no_relay_ever_connects() {
        let id = Identity::generate();
        // Destructured rather than `expect_err`, which would require `RelayClient: Debug` —
        // deriving that on a production struct holding the pool and the rate limiter, purely to
        // satisfy a test, widens what a stray `{:?}` can print at the network edge.
        let outcome =
            RelayClient::connect(&id, &["ws://127.0.0.1:1".into()], Duration::from_millis(500))
                .await;
        let err = match outcome {
            Ok(_) => panic!("no relay can come up; connect must refuse, never half-proceed"),
            Err(e) => e,
        };
        assert!(
            matches!(err, NetError::NoRelayConnected(_)),
            "the outage must be reported as NoRelayConnected, got {err:?}"
        );
    }

    /// The warm-client outage, driven through the REAL `fetch` (QURATOR-145 review, HIGH). A
    /// hand-built client whose only relay is unreachable is the exact shape the review traced:
    /// `connect`-time reachability is not assumed (this client never had a Connected relay, but
    /// the production pool had one and lost it — both land in "no relay can answer"), and
    /// nostr-relay-pool 0.43.1 resolves the REQ as `Ok(no events)` because the per-relay failure
    /// is logged-and-dropped in `stream_events_targeted`. Without the gate this is a successful
    /// empty answer; with it, the caller can finally tell an outage from a genuine empty.
    ///
    /// Uses `ws://127.0.0.1:1` (connection refused, nothing listens) so the test needs no relay
    /// server and no real network egress. The relay lands in Disconnected/reconnect-backoff states,
    /// never Connected — which is precisely the state the gate refuses on.
    #[tokio::test]
    async fn fetch_errors_when_no_relay_is_connected() {
        let url = "ws://127.0.0.1:1";
        let client = Client::builder().build();
        client.add_relay(url).await.expect("add_relay of a URL is offline bookkeeping");
        // The handshake fails (port 1 refuses); try_connect reports the failure and the relay
        // settles into Disconnected with reconnect backoff — the transient state pool_is_live
        // deliberately tolerates.
        let conn = client.try_connect(Duration::from_millis(500)).await;
        assert!(
            conn.success.is_empty(),
            "the unreachable relay must not report a completed handshake"
        );
        let rc = RelayClient {
            client,
            relays: vec![url.to_string()],
            limiter: Mutex::new(RelayRateLimiter::relay_writes()),
            start: Instant::now(),
        };
        let filter = Filter::new().kind(Kind::from_u16(hb_core::event::KIND_TEASER)).limit(1);
        let err = rc
            .fetch(filter, Duration::from_secs(2))
            .await
            .expect_err("a fetch no relay could answer must be an Err, never an empty Ok");
        assert!(
            matches!(err, NetError::NoRelayConnected(_)),
            "the outage must be reported as NoRelayConnected, got {err:?}"
        );
    }

    // ── The write governor wired into publish/publish_to (ban-avoidance pacing) ────────────────

    #[test]
    fn throttle_step_never_paces_a_full_burst() {
        // The usability floor: an ordinary interactive write (and a small listing's handful of
        // part events) drains the burst with zero sleep — throttle() returns without ever awaiting.
        let lim = Mutex::new(RelayRateLimiter::new(hb_core::RELAY_WRITE_BURST, hb_core::RELAY_WRITE_REFILL_PER_SEC));
        for i in 0..(hb_core::RELAY_WRITE_BURST as usize) {
            assert!(throttle_step(&lim, 0.0).is_none(), "burst write {i} must not sleep");
        }
        // Only once the burst is spent does the loop ask for a sleep (a large-collection flood).
        assert!(throttle_step(&lim, 0.0).is_some(), "past the burst, pacing engages");
    }

    #[test]
    fn throttle_step_caps_a_pathological_wait() {
        // A tiny refill would ask for a 10s single wait; the loop caps each sleep so it re-checks the
        // bucket promptly rather than sleeping an implausibly long time on one iteration.
        let lim = Mutex::new(RelayRateLimiter::new(1.0, 0.1));
        assert!(throttle_step(&lim, 0.0).is_none(), "the one burst token passes");
        let sleep = throttle_step(&lim, 0.0).expect("now empty → must pace");
        assert!(sleep <= MAX_THROTTLE_SLEEP, "each sleep is clamped to the cap, got {sleep:?}");
    }

    #[test]
    fn throttle_step_does_not_panic_on_an_astronomical_wait() {
        // Chorus (codex + gemini): a degenerate config makes `try_acquire` return a wait far past
        // `Duration::from_secs_f64`'s ~1.8e19s panic threshold (here ~1/f64::MIN_POSITIVE ≈ 4.5e307).
        // Clamping the f64 *before* constructing the Duration must keep this a bounded sleep, not a
        // panic. (Pre-fix this line panicked.)
        let lim = Mutex::new(RelayRateLimiter::new(1.0, f64::MIN_POSITIVE));
        assert!(throttle_step(&lim, 0.0).is_none(), "the one burst token passes");
        let sleep = throttle_step(&lim, 0.0).expect("now empty → must pace");
        assert_eq!(sleep, MAX_THROTTLE_SLEEP, "an astronomical wait clamps to the cap, no panic");
    }

    #[test]
    fn relay_health_serializes_camelcase_for_the_settings_rows() {
        let h = RelayHealth {
            url: "wss://relay.example".into(),
            status: "connecting".into(),
            connected: false,
            last_error: None,
        };
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"lastError\":null"), "camelCase last_error: {json}");
        assert!(json.contains("\"connected\":false"));
    }
}
