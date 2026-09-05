//! The manifest plane's session lifecycle (M18 W4): one endpoint, one accept loop, bound lazily.
//!
//! `transport.rs` is the protocol and owns no state. This module owns the *process* side: when the
//! endpoint exists, who serves connections on it, and how many at once.
//!
//! **Why lazy rather than at startup.** A node address is only obtainable from a bound endpoint, so
//! the owner cannot issue a ticket without one — binding on the first fulfil is therefore not a
//! shortcut, it is the earliest honest moment. A user who never sends *and never receives* a full
//! list never binds a QUIC endpoint or talks to a relay server, which is the right default for a
//! directory app.
//!
//! **An upgrade closes the old binding, and may abort an in-flight redemption — deliberately.** A
//! lease/drain protocol was tried and removed: admission and draining could not be made atomic
//! without a full binding-state machine (a lease could be taken just after the drain observed zero),
//! and its 30 s deadline was shorter than the protocol's own 120 s ACK window, so it *still* aborted
//! slow transfers while adding racy machinery. The consequence of an abort is bounded and already
//! handled: the ticket stays unspent, so the asker retries. Simpler and honest beats a lease that
//! only looks safe.
//!
//! **Two kinds of binding, not one** ([`Role`], owner ruling ③ 2026-07-31). Fulfilling must LISTEN.
//! Redeeming only needs to DIAL, and binding a listening endpoint for it would leave the redeemer
//! answering anyone who holds its permanently-stable node id — see [`crate::transport::bind_client_endpoint`].
//!
//! **But a ticket outlives the session that issued it** (tickets are valid until redeemed, by
//! owner ruling), and the transport secret is persisted precisely so the address in an old ticket
//! still resolves. So [`rebind_if_tickets_outstanding`] runs at startup and binds when this node
//! holds anything servable — see [`has_servable_content`]. That basis has moved twice as the
//! approval machinery was dismantled: the issued-ticket ledger answered "is a ticket unspent?"
//! until Option E deleted it (QURATOR-177); the standing-grant map answered "did I ever approve
//! serving?" until QURATOR-164 deleted approvals entirely. **Public collections need no approval,
//! so there is no promise to look up** — only content to serve. It over-binds rather than
//! under-binds, which is the safe direction: an unnecessary listener costs a relay connection, a
//! missing one makes this node silently unreachable to a peer holding a valid ticket.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::identity_state::SessionTransportKey;
use crate::transport::{bind_client_endpoint, bind_endpoint, ManifestPlane, ManifestSource};

/// What a caller needs the endpoint FOR (owner ruling ③, 2026-07-31).
///
/// A redeemer only dials, and a listening endpoint would leave it answering anyone holding its
/// (permanently stable) node id for the rest of the session — see [`bind_client_endpoint`] for why
/// that matters. Only fulfilling needs to serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Redeeming a ticket: dial out, advertise nothing, accept nothing.
    DialOnly,
    /// Fulfilling a request, or honouring an outstanding ticket after a restart.
    Listen,
}

/// The session's endpoint plus the npub that bound it. `None` until something needs it.
///
/// **The npub is not decoration.** Identity can change mid-session — generate, import, restore from
/// backup, wipe — and the source the accept loop serves from carries a *snapshot* of the signing key
/// and browse-key, because `ManifestSource` is a synchronous trait and cannot await a live handle. A
/// bare `Option<Endpoint>` would therefore keep serving manifests signed by the identity the user just
/// switched away from, on a node key that identity minted. Keying the binding to its npub is what
/// makes [`ensure_endpoint`] notice and rebind.
pub type SharedEndpoint = Arc<RwLock<PlaneState>>;

/// The session's plane plus a **generation counter**.
///
/// The counter closes a race the lock alone does not: `wipe_data` sets the identity to `None` and
/// calls [`close_plane`], but a fulfil task that read the *old* identity and is still queued for the
/// write lock would then bind a fresh listening endpoint **after** the wipe — resurrecting the
/// liveness surface the close exists to remove, under credentials the user just destroyed.
/// [`ensure_endpoint`] captures the generation on entry and refuses to publish if it moved.
#[derive(Default)]
pub struct PlaneState {
    pub bound: Option<BoundPlane>,
    pub generation: u64,
}

/// A bound endpoint and the identity it belongs to.
pub struct BoundPlane {
    pub endpoint: iroh::Endpoint,
    /// The npub whose keys the running accept loop serves from.
    pub owner_npub: String,
    /// Whether this binding **listens** (advertises the ALPN and runs an accept loop) or is
    /// dial-only. A redeemer needs only to dial (owner ruling ③), and a client-only binding must be
    /// upgraded before it can serve.
    pub listening: bool,
}

/// The rebind decision: whether a bound plane belongs to a **different** identity than the one being
/// bound for now.
///
/// The accept loop's [`ManifestSource`] carries a *snapshot* of the signing key — it is a
/// synchronous trait and cannot await a live handle — so a binding must be keyed to the npub whose
/// keys it snapshotted. Identity can change mid-session (generate, import, restore from backup,
/// wipe), and a binding that outlives its identity would keep serving manifests signed by a key the
/// user walked away from. This is the comparison that makes [`ensure_endpoint`] close and replace
/// such a binding instead of reusing it. `true` ⇒ the existing binding must be closed and rebound;
/// `false` ⇒ it still belongs to this identity and may be reused.
///
/// This is **not** a race guard — identity change is serialized by a wipe-first discipline (all
/// other write sites hard-refuse an existing identity, and `wipe_data` is the only caller of
/// `close_plane`, wiping the store before clearing the identity). But "not a race" is not
/// "covered", which is why the decision is extracted and pinned here.
pub(crate) fn should_rebind_for_owner(
    bound_owner_npub: &str,
    current_owner_npub: &str,
) -> bool {
    bound_owner_npub != current_owner_npub
}

/// How many redemptions may be served concurrently.
///
/// The plane's in-flight set stops one *ticket* being served twice; it does not stop N distinct
/// peers each holding a handler. Each handler can hold up to
/// [`hb_core::transport_payload::MANIFEST_MAX_TRANSPORT_BYTES`] of sealed manifest, so an
/// unbounded accept loop is an unbounded allocation with extra steps. Over-limit connections wait
/// for a permit rather than being refused — the deadlines in `transport.rs` mean a waiter cannot
/// stall forever, and a queued redemption is a better answer than a refusal indistinguishable from
/// "your ticket is forged".
///
/// **3, set by owner ruling 2026-09-01, on upload bandwidth rather than memory.** The previous 8 was
/// chosen against an 8 MiB ceiling; when that doubled to 16 MiB (QURATOR-106 follow-up, 2026-08-19)
/// the same 8 permits silently doubled peak serve-side allocation to ~128 MiB, and nobody revisited
/// it. But memory was never the binding constraint: **most peers serve from residential connections
/// whose UPLOAD is the scarce resource**, and 8 concurrent manifest serves saturate a typical one —
/// making every transfer slow rather than a few fast. 3 is the owner's number. It also brings peak
/// allocation to ~48 MiB, below even the original 8 × 8 MiB budget, which is a side effect and not
/// the reason.
pub const MAX_CONCURRENT_REDEMPTIONS: usize = 3;

/// How long [`ensure_endpoint`] will wait for the iroh `Endpoint::bind()` to resolve before giving up
/// with a loud error rather than hanging the caller (the fulfil/redeem click) forever.
///
/// `iroh::Endpoint::builder(presets::N0).bind()` performs relay discovery, DNS resolution, and the
/// initial relay handshake before returning; under NAT flapping or a dead relay it can block
/// indefinitely. This is deliberately longer than [`crate::net::RELAY_TIMEOUT`] (10 s, a single Nostr
/// relay handshake) because iroh's bind composes several setup steps, and shorter than the 120 s
/// protocol-level ACK window in `transport.rs` (which bounds an established transfer, not the bind).
/// 30 s gives NAT traversal a real chance while ensuring the UI recovers and surfaces an error.
pub const ENDPOINT_BIND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub fn new_shared_endpoint() -> SharedEndpoint {
    Arc::new(RwLock::new(PlaneState::default()))
}


/// Bind the endpoint and spawn its accept loop if this session has not already, then return a handle.
///
/// Double-checked under the write lock: two concurrent fulfils must not bind two endpoints on the
/// same secret (the second would fail, and which one failed would be a coin toss).
///
/// A binding belonging to a **different** `owner_npub` is closed and replaced, not reused — see
/// [`SharedEndpoint`] for why reuse would keep serving the previous identity's manifests.
pub async fn ensure_endpoint(
    shared: &SharedEndpoint,
    owner_npub: &str,
    live_npub: &crate::identity_state::SharedIdentity,
    transport_key: &SessionTransportKey,
    source: Arc<dyn ManifestSource>,
    need: Role,
) -> Result<iroh::Endpoint> {
    // A listening binding satisfies a redeemer too; a client-only one does not satisfy a fulfiller.
    // A binding that has been closed is never reusable, however well it otherwise matches — a failed
    // rebind used to leave exactly that in place and hand it to the next caller.
    let satisfies = |b: &BoundPlane| {
        !should_rebind_for_owner(&b.owner_npub, owner_npub)
            && (b.listening || need == Role::DialOnly)
            && !b.endpoint.is_closed()
    };
    let generation = {
        let st = shared.read().await;
        if let Some(bound) = st.bound.as_ref() {
            if satisfies(bound) {
                return Ok(bound.endpoint.clone());
            }
        }
        st.generation
    };

    let mut st = shared.write().await;
    // Re-check: another task may have bound while we waited for the write lock.
    if let Some(bound) = st.bound.as_ref() {
        if satisfies(bound) {
            return Ok(bound.endpoint.clone());
        }
    }
    // **Two fences, because one is not enough.**
    //
    // The generation catches a call that started BEFORE a `close_plane` — it captured the old value
    // and must not publish. But a caller snapshots the identity before invoking us, so a *stale
    // fulfil* can enter afterwards, capture the NEW generation, and pass this check. So the second
    // fence re-reads the LIVE identity here, under the lock, and refuses if the npub we were asked to
    // bind for is no longer the session's. A wiped session has no identity at all, which fails it.
    if st.generation != generation {
        return Err(anyhow::anyhow!("the transport was shut down while binding"));
    }
    let live = live_npub.read().await.as_ref().map(|id| id.npub());
    if live.as_deref() != Some(owner_npub) {
        return Err(anyhow::anyhow!(
            "the identity changed while binding the transport; refusing to publish a plane for a              session that is no longer current"
        ));
    }
    // Take the old binding OUT before closing it: if the bind below fails, the state must be empty,
    // not holding a closed endpoint that the satisfaction check would hand to the next caller.
    if let Some(old) = st.bound.take() {
        old.endpoint.close().await;
    }
    let listening = need == Role::Listen;
    let endpoint = if listening {
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
            role = "listen",
            "iroh: binding a LISTENING manifest endpoint (serving role)"
        );
        let ep = match tokio::time::timeout(
            ENDPOINT_BIND_TIMEOUT,
            bind_endpoint(transport_key.bytes()),
        )
        .await
        {
            Ok(Ok(ep)) => ep,
            Ok(Err(e)) => {
                tracing::warn!(
                    owner = %crate::logging::trunc_npub(owner_npub),
                    role = "listen",
                    "iroh: LISTENING manifest endpoint bind FAILED: {e}"
                );
                return Err(e);
            }
            Err(_) => {
                tracing::warn!(
                    owner = %crate::logging::trunc_npub(owner_npub),
                    role = "listen",
                    timeout_secs = ENDPOINT_BIND_TIMEOUT.as_secs(),
                    "iroh: LISTENING manifest endpoint bind TIMED OUT (no result in {} s)",
                    ENDPOINT_BIND_TIMEOUT.as_secs()
                );
                return Err(anyhow::anyhow!(
                    "the transport endpoint did not bind within {} s (relay/STUN unreachable?)",
                    ENDPOINT_BIND_TIMEOUT.as_secs()
                ));
            }
        };
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
            role = "listen",
            "iroh: manifest endpoint bound — accept loop running"
        );
        spawn_accept_loop(ep.clone(), ManifestPlane::new(source));
        ep
    } else {
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
            role = "dial",
            "iroh: binding a DIAL-ONLY manifest endpoint (redeeming role — no accept loop)"
        );
        let ep = match tokio::time::timeout(
            ENDPOINT_BIND_TIMEOUT,
            bind_client_endpoint(transport_key.bytes()),
        )
        .await
        {
            Ok(Ok(ep)) => ep,
            Ok(Err(e)) => {
                tracing::warn!(
                    owner = %crate::logging::trunc_npub(owner_npub),
                    role = "dial",
                    "iroh: dial-only manifest endpoint bind FAILED: {e}"
                );
                return Err(e);
            }
            Err(_) => {
                tracing::warn!(
                    owner = %crate::logging::trunc_npub(owner_npub),
                    role = "dial",
                    timeout_secs = ENDPOINT_BIND_TIMEOUT.as_secs(),
                    "iroh: dial-only manifest endpoint bind TIMED OUT (no result in {} s)",
                    ENDPOINT_BIND_TIMEOUT.as_secs()
                );
                return Err(anyhow::anyhow!(
                    "the transport endpoint did not bind within {} s (relay/STUN unreachable?)",
                    ENDPOINT_BIND_TIMEOUT.as_secs()
                ));
            }
        };
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
            role = "dial",
            "iroh: dial-only endpoint bound"
        );
        ep
    };
    st.bound = Some(BoundPlane {
        endpoint: endpoint.clone(),
        owner_npub: owner_npub.to_string(),
        listening,
    });
    Ok(endpoint)
}

/// Shut the plane down and forget it. Called when the session stops having an identity (sign-out /
/// wipe): a plane that outlives its identity is a node still answering redemptions with manifests
/// signed by a key the user just walked away from.
pub async fn close_plane(shared: &SharedEndpoint) {
    let mut st = shared.write().await;
    // Bump FIRST: any `ensure_endpoint` already queued for this lock captured the old generation and
    // must refuse to publish rather than resurrect the plane after a wipe.
    st.generation = st.generation.wrapping_add(1);
    if let Some(bound) = st.bound.take() {
        bound.endpoint.close().await;
    }
}

/// Serve the plane until the endpoint closes. One task per connection, gated by a semaphore.
fn spawn_accept_loop(endpoint: iroh::Endpoint, plane: Arc<ManifestPlane>) {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_REDEMPTIONS));
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let plane = plane.clone();
            let permits = permits.clone();
            tokio::spawn(async move {
                // Held for the whole redemption, dropped when this task ends — including on the
                // error paths, which is why it is a guard and not a manual release.
                let _permit = match permits.acquire().await {
                    Ok(p) => p,
                    // The semaphore is never closed while the loop runs; if that changes, refusing
                    // to serve is the safe answer.
                    Err(_) => return,
                };
                match incoming.await {
                    Ok(conn) => {
                        if let Err(e) = plane.serve(conn).await {
                            // A refused request is `Ok(None)` — reaching here means the protocol
                            // itself failed, which is worth a line but never fatal to the loop.
                            tracing::warn!("manifest plane: serving a connection failed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("manifest plane: an inbound connection failed: {e}"),
                }
            });
        }
        tracing::info!("manifest plane: the endpoint closed; no longer accepting redemptions");
    });
}

/// At startup, bind the plane **only if** this node has ever issued an approval.
///
/// The asymmetry is deliberate. Binding unconditionally would give every user a QUIC endpoint and a
/// relay connection they may never need; binding never would make a ticket issued in a previous
/// session undialable, which is a silent broken promise rather than an honest error. Until
/// QURATOR-177 Option E the issued-ticket store answered exactly the question that decided it
/// ("any unspent ticket?"); the ledger was deleted and the standing-grant map became the basis.
/// **QURATOR-164 (2026-09-04) deleted the grant map too**, and with it the last durable record of
/// "someone may dial me" — because under the ruling that record is meaningless: every node serves
/// the public collections it holds, in the background, without approval. The honest question is no
/// longer *"did I promise anyone anything?"* but *"do I hold anything servable?"*, which is what
/// [`has_servable_content`] answers.
pub async fn rebind_if_tickets_outstanding(
    shared: &SharedEndpoint,
    owner_npub: &str,
    live_npub: &crate::identity_state::SharedIdentity,
    transport_key: &SessionTransportKey,
    source: Arc<dyn ManifestSource>,
    store: &crate::store::DataStore,
) {
    if !has_servable_content(store) {
        return;
    }
    if let Err(e) =
        ensure_endpoint(shared, owner_npub, live_npub, transport_key, source, Role::Listen).await
    {
        tracing::warn!("manifest plane: could not rebind a listener for servable content: {e}");
    }
}

/// Does this node hold anything a peer could legitimately dial it for?
///
/// Two sources, matching the two serve bodies: **cached manifests** this node browsed (which
/// `send_cached_manifest_inner` re-serves — the Carrier-4 baseline every node participates in) and
/// **its own published collections** (which `send_full_list_inner` builds). Either one means a
/// dial is possible, so the listener is bound.
///
/// ⚠ This is deliberately a CHEAP, IMPRECISE check — a non-empty cache directory, not a parse of
/// its contents. Binding a listener nothing dials costs an idle endpoint; NOT binding one that
/// something does dial costs a silently unreachable node, which is the failure that actually hurts.
/// When in doubt it must bind.
///
/// The directory read itself lives in `manifest_cache::has_any` — this file is inside INV-4′'s
/// swept transport surface and must touch no filesystem. See that function's doc for why the fence
/// is not widened instead.
fn has_servable_content(store: &crate::store::DataStore) -> bool {
    crate::manifest_cache::has_any(&store.manifest_cache_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    // The lifecycle tests reuse the transport suite's loopback fixtures rather than growing a
    // second copy of them here: `TestSource` is the hand-rollable `ManifestSource` and
    // `real_payload_for` builds a real sealed envelope (a hand-written fixture would prove nothing
    // about what production puts on the wire — see its own doc comment).
    use crate::identity_state::AppIdentity;
    use crate::transport::tests::{real_payload_for, TestSource};
    use hb_core::ticket::TransportTicket;

    /// The rebind decision is the untested core of the manifest plane's identity lifecycle: the
    /// accept loop serves from a *snapshot* of the signing key, so a binding keyed to a DIFFERENT
    /// npub than the session's current one must be closed and rebuilt — otherwise it keeps serving
    /// manifests signed by a key the user switched away from. `ensure_endpoint`'s satisfaction
    /// check delegates to [`should_rebind_for_owner`], so this is the comparison that actually runs
    /// on every fulfil/rebind. Inverting it (rebind when the npubs MATCH) must red here.
    #[test]
    fn a_binding_for_a_different_npub_rebinds_and_an_unchanged_one_is_reused() {
        // Same npub — the snapshot still belongs to the current identity: reuse, do not rebind.
        assert!(
            !should_rebind_for_owner("npub-snapshot-a", "npub-snapshot-a"),
            "an unchanged identity must keep its binding"
        );
        // Different npub — the snapshot is stale: close and rebuild.
        assert!(
            should_rebind_for_owner("npub-snapshot-a", "npub-snapshot-b"),
            "a changed identity must close and rebuild the binding"
        );
    }

    /// QURATOR-45: the endpoint bind must be BOUNDED so a stuck relay handshake surfaces an error
    /// rather than hanging the fulfil click forever. `ENDPOINT_BIND_TIMEOUT` is the duration that
    /// wraps the `bind_endpoint` / `bind_client_endpoint` calls inside `ensure_endpoint`. Asserting it
    /// is finite and non-zero here is the regression guard: if someone removes the wrapper or sets the
    /// duration to something enormous (or zero, which would make every bind fail instantly), this reds.
    #[test]
    fn endpoint_bind_timeout_is_finite_and_nonzero() {
        assert!(
            !ENDPOINT_BIND_TIMEOUT.is_zero(),
            "a zero ENDPOINT_BIND_TIMEOUT would make every bind fail instantly"
        );
        // Must be finite — the whole point is it must resolve, so `Duration::MAX` (which would wrap
        // but never elapse) defeats the purpose.
        assert!(
            ENDPOINT_BIND_TIMEOUT.as_secs() > 0,
            "the bind timeout must be a positive, finite duration"
        );
        assert!(
            ENDPOINT_BIND_TIMEOUT.as_secs() <= 120,
            "the bind timeout must be shorter than the protocol ACK window (120 s), not open-ended"
        );
    }

    /// QURATOR-45: prove the timeout MECHANISM surfaces an error on a future that never resolves,
    /// rather than hanging. This is the pattern `ensure_endpoint` applies to the bind — a pending
    /// future wrapped in `tokio::time::timeout(ENDPOINT_BIND_TIMEOUT, …)` must yield `Err(Elapsed)`,
    /// which the production code turns into an `anyhow!` error. If the wrapper is removed this test
    /// still passes (it tests the mechanism in isolation) — the constant test above is the guard that
    /// the wrapper's duration is sound, and both together pin the contract.
    #[tokio::test]
    async fn a_never_returning_bind_surfaces_an_error_rather_than_hanging() {
        // A future that never resolves — mirrors a stuck `bind_endpoint` under NAT/relay flap.
        let pending = std::future::pending::<Result<()>>();
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(50), pending).await;
        assert!(
            outcome.is_err(),
            "a never-returning bind wrapped in tokio::time::timeout MUST surface a timeout error, \
             not hang"
        );
    }

    // ---------------------------------------------------------------------------
    // QURATOR-52 item 3 — the lifecycle itself, not its parts.
    //
    // The tests above pin the rebind DECISION (`should_rebind_for_owner`) and the bind timeout in
    // isolation. Nothing drove the real state machine: bind a plane for identity A, close it the
    // exact way `wipe_data` closes it, then bind for identity B — and the two fences that stop a
    // call which raced the wipe from publishing a plane under the wiped key. The three below go
    // through `ensure_endpoint` / `close_plane` themselves, so a hand-built `PlaneState` is never
    // the thing under test.
    //
    // **On the bind these use.** `ensure_endpoint` calls the production `bind_endpoint` /
    // `bind_client_endpoint` (`presets::N0`) — there is no injection seam, by design, so exercising
    // the real state machine means binding the real thing. That stays loopback-safe: in iroh 1.0
    // `Endpoint::bind()` returns once the local sockets are up, and the relay/DNS/pkarr work runs
    // in spawned background tasks, so **no assertion below can depend on relay reachability**.
    // Nothing dials and nothing is served; the plane is bound, closed, and bound again.
    //
    // The live identity handle is a REAL `AppIdentity` (not a placeholder npub string) because the
    // second fence compares the live handle's npub against the requested owner — a fake string
    // would be refused for the wrong reason and the test would prove nothing about fence 2.
    // ---------------------------------------------------------------------------

    /// A minimal ticket + payload for the `ManifestSource`: nothing dials or serves in these
    /// tests, so the source only has to exist for the bind. `TestSource` is the transport suite's
    /// hand-rollable source — reusing it here keeps one fixture, not two.
    fn lifecycle_source() -> Arc<dyn ManifestSource> {
        let ticket = TransportTicket::issue(
            "req-lifecycle",
            "lifecycle",
            // Never read: the address is only consulted by a peer that dials, and none does.
            "{}",
            1_700_000_000,
            None,
        );
        TestSource::new(ticket, real_payload_for("lifecycle", 1))
    }

    /// **The identity-wipe → plane-rebind path, end to end through the production calls.**
    ///
    /// `wipe_data`'s comment claims three things about `close_plane`: the binding is *forgotten*
    /// (not merely closed), the endpoint is *closed* (a wiped key's node stops answering), and the
    /// *generation* moves (so a call that raced the wipe cannot publish). This is the first test
    /// that checks any of them against the real state machine rather than the pure helpers.
    ///
    /// Why the endpoint-identity comparison matters: `EndpointId` is the node key derived from the
    /// transport secret, so `ep_b.id() != ep_a.id()` proves the rebind minted a NEW plane rather
    /// than resurrecting the old handle — the satisfaction check must have refused the closed
    /// binding, not handed it back.
    #[tokio::test]
    async fn a_wipe_closes_the_plane_and_the_next_identity_gets_a_new_plane() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let shared = new_shared_endpoint();

            // Identity A: npub and transport key extracted before the handle takes ownership
            // (`AppIdentity` is deliberately not `Clone`).
            let id_a = AppIdentity::generate();
            let npub_a = id_a.npub();
            let key_a = id_a.transport_key.clone();
            let live: crate::identity_state::SharedIdentity = Arc::new(RwLock::new(Some(id_a)));

            // DialOnly: no accept loop, no advertised ALPN — lighter, and it exercises exactly the
            // same generation/rebind logic (the role only decides whether a loop spawns).
            let ep_a = ensure_endpoint(
                &shared,
                &npub_a,
                &live,
                &key_a,
                lifecycle_source(),
                Role::DialOnly,
            )
            .await
            .expect("the first bind succeeds");
            {
                let st = shared.read().await;
                let b = st.bound.as_ref().expect("the plane is bound after the first call");
                assert_eq!(b.owner_npub, npub_a, "the binding is keyed to the npub that bound it");
                assert!(
                    !b.listening,
                    "a DialOnly bind must not advertise the listening role"
                );
                assert_eq!(st.generation, 0, "a plain bind does not move the generation counter");
            }
            assert!(!ep_a.is_closed(), "sanity: the freshly bound endpoint is open");

            // The exact call `wipe_data` makes, in the same order (identity already cleared below
            // would be closer still, but the identity half belongs to fence 2's test).
            close_plane(&shared).await;
            {
                let st = shared.read().await;
                assert!(
                    st.bound.is_none(),
                    "the wipe must FORGET the binding — a `Some(closed)` left behind is what a \
                     failed rebind used to hand to the next caller"
                );
                assert_eq!(
                    st.generation, 1,
                    "the wipe bumps the generation exactly once — the counter is fence 1"
                );
            }
            assert!(
                ep_a.is_closed(),
                "the wiped endpoint must be CLOSED, not just dropped from the state: the whole \
                 point of close_plane is that the node key the wiped identity minted stops \
                 answering redemptions"
            );

            // Identity B — a genuinely different key set, not a rename of A.
            let id_b = AppIdentity::generate();
            let npub_b = id_b.npub();
            let key_b = id_b.transport_key.clone();
            assert_ne!(npub_b, npub_a, "sanity: the two identities are distinct");
            *live.write().await = Some(id_b);

            let ep_b = ensure_endpoint(
                &shared,
                &npub_b,
                &live,
                &key_b,
                lifecycle_source(),
                Role::DialOnly,
            )
            .await
            .expect("the new identity must be able to bind its own plane");
            assert_ne!(
                ep_b.id(),
                ep_a.id(),
                "the rebind must produce a NEW endpoint (different node key), not a resurrection \
                 of the closed one"
            );
            assert!(!ep_b.is_closed(), "the new plane is live");
            assert!(
                ep_a.is_closed(),
                "the wiped plane stays dead after the rebind — nothing resurrects it"
            );
            {
                let st = shared.read().await;
                assert_eq!(
                    st.bound.as_ref().expect("identity B's plane is bound").owner_npub,
                    npub_b,
                    "the new binding is keyed to the NEW identity's npub"
                );
                assert_eq!(
                    st.generation, 1,
                    "a rebind is not a wipe: only close_plane moves the generation"
                );
            }

            // Tidy: close B's plane too, so no relay-retry or accept task outlives the test.
            close_plane(&shared).await;
        })
        .await
        .expect("test timed out");
    }

    /// **Fence 1 — a call that started BEFORE the wipe must not publish after it.**
    ///
    /// This is the race the generation counter exists for: a fulfil task reads the old identity,
    /// then queues for the state lock; the wipe closes the plane underneath it; the queued task
    /// must refuse to publish rather than resurrect the liveness surface the close removed.
    ///
    /// The interleave is FORCED, not hoped for. The test holds the write lock, so both tasks park
    /// on it in the order they are spawned — tokio's `RwLock` is documented first-in-first-out —
    /// and each `yield_now().await` lets the spawned task reach its park before the next line
    /// runs. When the gate drops, permit arithmetic alone fixes the sequence: the stale caller is
    /// granted its read (it sees the still-bound plane and captures generation 0), the wiper is
    /// granted its write (identity cleared, generation bumped to 1, plane taken and closed), and
    /// only then is the stale caller's write request — queued behind the wiper's — granted, where
    /// the generation mismatch must stop it. If the ordering machinery ever broke, this test
    /// fails LOUDLY (the stale call would bind and return `Ok`, or refuse via fence 2's different
    /// message); it cannot pass vacuously.
    #[tokio::test(flavor = "current_thread")]
    async fn a_call_that_started_before_the_wipe_must_not_publish_after_it() {
        // A DialOnly binding for identity A exists, and the stale caller wants to UPGRADE it to
        // Listen — the real fulfil shape, and the one that gets past the read-stage satisfaction
        // check (DialOnly does not satisfy Listen) and deep enough to reach the fences.
        let shared = new_shared_endpoint();
        let id_a = AppIdentity::generate();
        let npub_a = id_a.npub();
        let key_a = id_a.transport_key.clone();
        let live: crate::identity_state::SharedIdentity = Arc::new(RwLock::new(Some(id_a)));
        let ep_a = ensure_endpoint(
            &shared,
            &npub_a,
            &live,
            &key_a,
            lifecycle_source(),
            Role::DialOnly,
        )
        .await
        .expect("setup: the pre-wipe bind succeeds");

        // Freeze the state machine so both tasks below queue on the lock, in this order.
        let gate = shared.write().await;

        // (1) The STALE caller — queued FIRST. It parks acquiring the read lock, so it has not
        //     yet captured the generation; when it finally runs, it will capture generation 0.
        let stale = {
            let shared = shared.clone();
            let live = live.clone();
            let npub = npub_a.clone();
            let key = key_a.clone();
            let source = lifecycle_source();
            tokio::spawn(async move {
                ensure_endpoint(&shared, &npub, &live, &key, source, Role::Listen).await
            })
        };
        tokio::task::yield_now().await;

        // (2) The WIPER — queued SECOND, so its generation bump lands between the stale caller's
        //     read and its write. It performs `wipe_data`'s two state halves in production order:
        //     clear the identity, then close the plane.
        let wiper = {
            let shared = shared.clone();
            let live = live.clone();
            tokio::spawn(async move {
                *live.write().await = None;
                close_plane(&shared).await;
            })
        };
        tokio::task::yield_now().await;

        // Release. The FIFO queue now executes: stale reads (generation 0) → wiper wipes
        // (generation 1, plane closed) → stale writes and must hit fence 1.
        drop(gate);

        wiper.await.expect("the wipe completed");
        let outcome = stale.await.expect("the stale call resolves rather than hanging");
        let err = outcome
            .expect_err("a call that captured the pre-wipe generation must not publish a plane");
        assert!(
            err.to_string().contains("the transport was shut down while binding"),
            "the FIRST fence (generation) must be what refuses it — fence 2's message here would \
             mean the generation check is not doing its job. got: {err}"
        );

        let st = shared.read().await;
        assert!(
            st.bound.is_none(),
            "the refused call published nothing — the wiped plane was not resurrected"
        );
        assert_eq!(st.generation, 1, "only the wipe moved the generation");
        drop(st);
        assert!(ep_a.is_closed(), "the wipe closed the pre-wipe endpoint");
    }

    /// **Fence 2 — a call for the wiped identity is refused even with a CURRENT generation.**
    ///
    /// Fence 1 cannot catch a stale fulfil that *enters after* the wipe: it captures the new
    /// generation and passes. The second fence re-reads the LIVE identity under the lock and
    /// refuses when the npub it was asked to bind for is no longer the session's — which is why
    /// this test's call must fail with fence 2's message specifically, not merely "some error".
    #[tokio::test]
    async fn a_call_for_the_wiped_identity_is_refused_even_with_a_current_generation() {
        let shared = new_shared_endpoint();
        let id_a = AppIdentity::generate();
        let npub_a = id_a.npub();
        let key_a = id_a.transport_key.clone();
        let live: crate::identity_state::SharedIdentity = Arc::new(RwLock::new(Some(id_a)));

        // The wipe, in `wipe_data`'s order: identity cleared, then the plane closed.
        *live.write().await = None;
        close_plane(&shared).await;

        // This call enters AFTER the wipe, so it captures generation 1 — fence 1 passes by
        // construction, and only the live-identity re-read can stop it.
        let err = ensure_endpoint(
            &shared,
            &npub_a,
            &live,
            &key_a,
            lifecycle_source(),
            Role::Listen,
        )
        .await
        .expect_err("a call for the wiped identity must not publish a plane");
        assert!(
            err.to_string().contains("the identity changed while binding"),
            "the SECOND fence (live identity) must be what refuses it — this call passes fence 1 \
             by construction. got: {err}"
        );

        let st = shared.read().await;
        assert!(
            st.bound.is_none(),
            "no endpoint was bound under the wiped key's name"
        );
        assert_eq!(
            st.generation, 1,
            "the refused call neither published nor moved the generation"
        );
    }

    /// **The signing-key comparison in the SATISFACTION CHECK — QURATOR-52's headline, and the
    /// piece the wipe-lifecycle test structurally cannot cover.** After a wipe there is no binding
    /// left to satisfy or refuse, so the next bind is fresh. But the owner-change-without-wipe
    /// path (`restore_data`'s shape: `*identity.write().await = Some(..)`, no `close_plane`)
    /// leaves the old plane BOUND, and [`ensure_endpoint`] must notice the owner moved and
    /// close-and-rebind rather than hand back the previous identity's endpoint.
    ///
    /// **Identity A binds with `Listen` on purpose — it is what makes the npub comparison the
    /// ONLY thing this test can fail on.** `satisfies` is a conjunction of three clauses (owner,
    /// role, closed-ness); a `DialOnly` binding under a `Listen` ask fails the ROLE clause
    /// regardless of the owner check, which is exactly how the first draft of this test stayed
    /// green under a mutation that stubbed `should_rebind_for_owner` out of `satisfies`
    /// entirely — the "suite pins attributes, mutation changes shape, everything stays green"
    /// trap. A listening binding satisfies the role clause for ANY ask, so the owner mismatch is
    /// the sole discriminator left.
    #[tokio::test]
    async fn a_bound_plane_for_a_previous_identity_is_rebound_not_reused() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let shared = new_shared_endpoint();

            let id_a = AppIdentity::generate();
            let npub_a = id_a.npub();
            let key_a = id_a.transport_key.clone();
            let live: crate::identity_state::SharedIdentity = Arc::new(RwLock::new(Some(id_a)));
            let ep_a = ensure_endpoint(
                &shared,
                &npub_a,
                &live,
                &key_a,
                lifecycle_source(),
                Role::Listen,
            )
            .await
            .expect("identity A binds its plane");

            // The identity changes WITHOUT a wipe. The old plane is still bound, still listening.
            let id_b = AppIdentity::generate();
            let npub_b = id_b.npub();
            let key_b = id_b.transport_key.clone();
            *live.write().await = Some(id_b);

            let ep_b = ensure_endpoint(
                &shared,
                &npub_b,
                &live,
                &key_b,
                lifecycle_source(),
                Role::Listen,
            )
            .await
            .expect("the new identity gets its own plane");

            assert_ne!(
                ep_b.id(), ep_a.id(),
                "the previous identity's endpoint must NOT be handed to the new one — it is the \
                 node key A minted, serving manifests A's keys signed"
            );
            assert!(
                ep_a.is_closed(),
                "the displaced binding is closed, not left listening under A's node key"
            );
            let st = shared.read().await;
            let b = st.bound.as_ref().expect("a binding exists for identity B");
            assert_eq!(b.owner_npub, npub_b, "the new binding is keyed to the new npub");
            assert!(b.listening, "the rebind honoured the requested role");
            assert_eq!(st.generation, 0, "an owner rebind is not a wipe: no generation bump");
            drop(st);

            close_plane(&shared).await;
        })
        .await
        .expect("test timed out");
    }

    /// **The third clause of the satisfaction check: a binding whose endpoint has died is never
    /// handed back, however well it otherwise matches.** The comment on `satisfies` records this
    /// as a fixed bug — "a failed rebind used to leave exactly that in place and hand it to the
    /// next caller" — and the owner/role clauses above cannot cover it, because it fires when the
    /// npub and the role BOTH still match. Only the closed-ness clause is left to catch it.
    ///
    /// The state is built without touching production internals: the endpoint is closed through
    /// its own public handle, leaving `bound = Some(closed)` behind — exactly what a failed rebind
    /// or an iroh-side teardown leaves. A same-owner, same-role ask must then bind a FRESH
    /// endpoint; handing back the dead one would return `Ok` with an endpoint whose `connect`
    /// can never succeed, i.e. a fulfil click that "works" and then always fails.
    #[tokio::test]
    async fn a_binding_whose_endpoint_died_is_rebound_even_when_the_owner_still_matches() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let shared = new_shared_endpoint();
            let id = AppIdentity::generate();
            let npub = id.npub();
            let key = id.transport_key.clone();
            let live: crate::identity_state::SharedIdentity = Arc::new(RwLock::new(Some(id)));

            let ep_a = ensure_endpoint(
                &shared,
                &npub,
                &live,
                &key,
                lifecycle_source(),
                Role::Listen,
            )
            .await
            .expect("the initial bind succeeds");

            // Kill the ENDPOINT, not the plane: the state still records the binding, and every
            // clause of `satisfies` except closed-ness would pass for a same-owner, same-role ask.
            ep_a.close().await;
            assert!(ep_a.is_closed(), "sanity: the endpoint is dead but still recorded");
            assert!(
                shared.read().await.bound.is_some(),
                "sanity: the closed binding is still in place — the state a failed rebind leaves"
            );

            let ep_b = ensure_endpoint(
                &shared,
                &npub,
                &live,
                &key,
                lifecycle_source(),
                Role::Listen,
            )
            .await
            .expect("a same-owner ask must still get a USABLE endpoint");
            // **Discriminated by closed-ness, not by `EndpointId` — and that is not a
            // compromise.** The id IS the node key derived from the persisted transport secret,
            // so binding under the SAME identity yields the SAME id *by design* (that stability
            // is why the secret is persisted). What distinguishes a fresh endpoint instance from
            // the dead handle is that the fresh one is open: had `satisfies` handed the closed
            // binding back, `ep_b` would BE `ep_a`'s handle and report closed here.
            assert!(
                !ep_b.is_closed(),
                "the dead binding was replaced, not handed back — returning it would be an Ok \
                 whose every future connect fails"
            );
            {
                let st = shared.read().await;
                let b = st.bound.as_ref().expect("the replacement is recorded");
                assert!(
                    !b.endpoint.is_closed(),
                    "the STATE records the live replacement, not the corpse"
                );
                assert_eq!(b.owner_npub, npub, "still keyed to the same, unchanged identity");
                assert_eq!(st.generation, 0, "no wipe happened, so no generation bump");
                // Released before `close_plane` below takes the WRITE lock — holding a read guard
                // across it is a self-deadlock, and it cost this test its first three runs.
            }

            close_plane(&shared).await;
        })
        .await
        .expect("test timed out");
    }
}
