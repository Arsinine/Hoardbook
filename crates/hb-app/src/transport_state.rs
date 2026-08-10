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
//! **But an unspent ticket outlives the session that issued it** (tickets are valid until redeemed,
//! by owner ruling), and the transport secret is persisted precisely so the address in an old ticket
//! still resolves. So [`rebind_if_tickets_outstanding`] runs at startup and binds *only* when this
//! node has an unspent approval on the books — otherwise a peer redeeming yesterday's ticket would
//! find nothing listening and see a failure with no cause a human could act on.

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
/// peers each holding a handler. Each handler can hold up to 8 MiB of sealed manifest, so an
/// unbounded accept loop is an unbounded allocation with extra steps. Over-limit connections wait
/// for a permit rather than being refused — the deadlines in `transport.rs` mean a waiter cannot
/// stall forever, and a queued redemption is a better answer than a refusal indistinguishable from
/// "your ticket is forged".
pub const MAX_CONCURRENT_REDEMPTIONS: usize = 8;

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
            "iroh: binding a LISTENING manifest endpoint (serving role)"
        );
        let ep = bind_endpoint(transport_key.bytes()).await?;
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
            "iroh: manifest endpoint bound — accept loop running"
        );
        spawn_accept_loop(ep.clone(), ManifestPlane::new(source));
        ep
    } else {
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
            "iroh: binding a DIAL-ONLY manifest endpoint (redeeming role — no accept loop)"
        );
        let ep = bind_client_endpoint(transport_key.bytes()).await?;
        tracing::info!(
            owner = %crate::logging::trunc_npub(owner_npub),
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

/// At startup, bind the plane **only if** an unspent ticket is outstanding.
///
/// The asymmetry is deliberate. Binding unconditionally would give every user a QUIC endpoint and a
/// relay connection they may never need; binding never would make a ticket issued in a previous
/// session undialable, which is a silent broken promise rather than an honest error. Reading the
/// issued-ticket store answers exactly the question that decides it.
pub async fn rebind_if_tickets_outstanding(
    shared: &SharedEndpoint,
    owner_npub: &str,
    live_npub: &crate::identity_state::SharedIdentity,
    transport_key: &SessionTransportKey,
    source: Arc<dyn ManifestSource>,
    store: &crate::store::DataStore,
) {
    let outstanding = store
        .load_issued_tickets()
        .map(|m| m.values().any(|r| r.consumed_at.is_none()))
        .unwrap_or(false);
    if !outstanding {
        return;
    }
    if let Err(e) =
        ensure_endpoint(shared, owner_npub, live_npub, transport_key, source, Role::Listen).await
    {
        tracing::warn!("manifest plane: could not rebind for an outstanding ticket: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
