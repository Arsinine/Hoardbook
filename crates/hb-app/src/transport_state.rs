//! The manifest plane's session lifecycle (M18 W4): one endpoint, one accept loop, bound lazily.
//!
//! `transport.rs` is the protocol and owns no state. This module owns the *process* side: when the
//! endpoint exists, who serves connections on it, and how many at once.
//!
//! **Why lazy rather than at startup.** A node address is only obtainable from a bound endpoint, so
//! the owner cannot issue a ticket without one — binding on the first fulfil is therefore not a
//! shortcut, it is the earliest honest moment. A user who never shares a full list never binds a QUIC
//! endpoint or talks to a relay server, which is the right default for a directory app.
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
use crate::transport::{bind_endpoint, ManifestPlane, ManifestSource};

/// The session's endpoint plus the npub that bound it. `None` until something needs it.
///
/// **The npub is not decoration.** Identity can change mid-session — generate, import, restore from
/// backup, wipe — and the source the accept loop serves from carries a *snapshot* of the signing key
/// and browse-key, because `ManifestSource` is a synchronous trait and cannot await a live handle. A
/// bare `Option<Endpoint>` would therefore keep serving manifests signed by the identity the user just
/// switched away from, on a node key that identity minted. Keying the binding to its npub is what
/// makes [`ensure_endpoint`] notice and rebind.
pub type SharedEndpoint = Arc<RwLock<Option<BoundPlane>>>;

/// A bound endpoint and the identity it belongs to.
pub struct BoundPlane {
    pub endpoint: iroh::Endpoint,
    /// The npub whose keys the running accept loop serves from.
    pub owner_npub: String,
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
    Arc::new(RwLock::new(None))
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
    transport_key: &SessionTransportKey,
    source: Arc<dyn ManifestSource>,
) -> Result<iroh::Endpoint> {
    if let Some(bound) = shared.read().await.as_ref() {
        if bound.owner_npub == owner_npub {
            return Ok(bound.endpoint.clone());
        }
    }
    let mut guard = shared.write().await;
    // Re-check: another task may have bound while we waited for the write lock.
    if let Some(bound) = guard.as_ref() {
        if bound.owner_npub == owner_npub {
            return Ok(bound.endpoint.clone());
        }
        // A stale binding from a previous identity. Closing the endpoint ends its accept loop (the
        // `while let Some(..) = endpoint.accept()` returns `None`), which drops the old source.
        bound.endpoint.close().await;
    }
    let endpoint = bind_endpoint(transport_key.bytes()).await?;
    spawn_accept_loop(endpoint.clone(), ManifestPlane::new(source));
    *guard = Some(BoundPlane { endpoint: endpoint.clone(), owner_npub: owner_npub.to_string() });
    Ok(endpoint)
}

/// Shut the plane down and forget it. Called when the session stops having an identity (sign-out /
/// wipe): a plane that outlives its identity is a node still answering redemptions with manifests
/// signed by a key the user just walked away from.
pub async fn close_plane(shared: &SharedEndpoint) {
    if let Some(bound) = shared.write().await.take() {
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
    if let Err(e) = ensure_endpoint(shared, owner_npub, transport_key, source).await {
        tracing::warn!("manifest plane: could not rebind for an outstanding ticket: {e}");
    }
}
