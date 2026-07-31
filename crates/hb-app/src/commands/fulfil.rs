//! The fulfil verb (M18 W4): the two commands that turn an inert "please send me the full list" DM
//! into a manifest that actually arrives.
//!
//! Two halves, one per side of the plane:
//!
//! - [`send_full_list`] — the **owner** side. Behind an explicit human click, always: build the
//!   manifest, bind the plane, mint a ticket for this one request, record it, DM it. Nothing here is
//!   reachable from a timer or a relay event, which is M17 ruling #4 (the app never auto-sends) kept
//!   structural rather than remembered.
//! - [`redeem_manifest_ticket`] — the **asker** side. Dial, fetch, and hand the bytes to the
//!   *existing* M16 W4 gate. Redemption is immediate and has no defer affordance (owner ruling
//!   2026-07-30): there is no "redeem later" entry point for a button to bind to.
//!
//! **Export stays reachable and stays the fallback.** `export_manifest` is untouched by this module;
//! when the plane cannot connect, [`send_full_list`] fails with an error that names export rather than
//! leaving the hoarder with a dead button and no second route.

use std::sync::Arc;

use hb_core::{ManifestPayload, TransportTicket};
use tauri::State;

use crate::commands::browse::accept_manifest_bytes;
use crate::commands::collection::build_slug_manifest;
use crate::error::CmdResult;
use crate::identity_state::SharedIdentity;
use crate::manifest_source::StoreManifestSource;
use crate::net::SharedRelay;
use crate::store::{DataStore, IssuedTicketRecord};
use crate::transport::{fetch_manifest, issue_ticket};
use crate::transport_state::{ensure_endpoint, SharedEndpoint};

fn cmd_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mint a request id: 128 bits of randomness, hex.
///
/// **Not derived from the slug, the asker, or the clock.** A request id is the ticket's primary
/// binding, so a guessable one would let a peer present a ticket for an approval it was never sent —
/// and a derived one collides across two approvals for the same collection, which would make the
/// second silently reuse the first's spent-bit.
fn new_request_id() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

/// **The fulfil verb.** Approve one request for one collection: build the manifest, bind the plane,
/// mint a ticket bound to this approval, persist it, and DM it to the asker.
///
/// Ordering is load-bearing at three points:
///
/// 1. **The manifest is built first**, before the endpoint binds or a ticket exists. Sealing applies
///    the 8 MiB ceiling, so an over-cap collection is refused here — before a promise is made, rather
///    than after the asker has a ticket that can only ever fail.
/// 2. **The record is persisted before the DM.** The reverse order hands a peer a ticket this node
///    cannot authorize, which is indistinguishable from a forgery. An orphaned record — the DM then
///    failed — is inert, because nobody holds the matching ticket.
/// 3. **The payload built here is discarded.** It exists to prove the manifest is producible and
///    within the ceiling; the plane rebuilds it at redeem time, from the same pure core, so the asker
///    gets the tree as it is *then* rather than a snapshot frozen at approval.
#[tauri::command]
pub async fn send_full_list(
    npub: String,
    slug: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<()> {
    let recipient = crate::commands::chat::parse_recipient(&npub)?;
    let (id_clone, browse_key, transport_key, own_npub) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone(), id.transport_key.clone(), id.npub())
    };
    if crate::commands::chat::is_self_send(&recipient, &id_clone.public_key()) {
        return Err("You can't send a full list to yourself.".into());
    }

    // (1) Prove the manifest exists and fits, before anything is promised. `seal` is the ceiling.
    let envelope = build_slug_manifest(&slug, &store, &id_clone, browse_key.bytes())?;
    ManifestPayload::seal(&envelope).map_err(|e| {
        format!(
            "This collection's full list is too large to send over the connection ({e}). \
             Export it instead: Home → ⋯ → Export, then hand the file over."
        )
    })?;

    let source: Arc<dyn crate::transport::ManifestSource> = StoreManifestSource::new(
        (*store).clone(),
        id_clone.clone(),
        browse_key.clone(),
    );
    let ep = ensure_endpoint(&endpoint, &own_npub, &transport_key, source).await.map_err(|e| {
        format!(
            "Could not start the transport ({e}). Export the list instead: Home → ⋯ → Export."
        )
    })?;

    let request_id = new_request_id();
    let ticket = issue_ticket(&ep, &request_id, &slug, now_secs()).map_err(cmd_err)?;

    // (2) Record before the DM, so a redeemer always presents a ticket we can recognise.
    store
        .record_issued_ticket(&IssuedTicketRecord {
            ticket: ticket.clone(),
            redeemer_npub: npub.clone(),
            consumed_at: None,
            delivered_bytes: None,
        })
        .map_err(cmd_err)?;

    let body = serde_json::to_string(&ticket).map_err(cmd_err)?;
    let own = crate::net::relay_urls(&store);
    let client = crate::net::client(&id_clone, &store, &relay).await.map_err(cmd_err)?;
    crate::commands::chat::send_dm_inner(
        &client,
        &id_clone,
        &recipient,
        &body,
        &own,
        crate::net::RELAY_TIMEOUT,
    )
    .await
    .map_err(cmd_err)?;
    Ok(())
}

/// **The asker's half.** Redeem a ticket that arrived by DM: dial the address it carries, fetch the
/// manifest, and hand it to the M16 W4 gate.
///
/// The gate is [`accept_manifest_bytes`] — the same function the file/paste import calls, not a
/// transport-flavoured copy of it. So the author is still pinned to the browsed peer, the signature
/// still verified before the decrypt, the slug still bound, and completeness still required. The one
/// thing this path adds is `expected_slug` coming from the **ticket** rather than from the UI: the
/// ticket is what the owner signed off on, so it is the stronger statement of what was approved.
///
/// A failure here does **not** spend the ticket — the owner only records a receipt on the asker's
/// acknowledgement, so a dial that never connects can simply be retried.
#[tauri::command]
pub async fn redeem_manifest_ticket(
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<crate::commands::browse::ImportedManifest> {
    let ticket: TransportTicket = serde_json::from_str(&ticket_json)
        .map_err(|_| "That message is not a readable transport ticket.".to_string())?;
    ticket.verify_shape().map_err(cmd_err)?;

    let (id_clone, browse_key, transport_key, own_npub) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone(), id.transport_key.clone(), id.npub())
    };

    // The asker needs an endpoint to dial *from*. `ensure_endpoint` also arms the accept loop, which
    // is correct rather than incidental: redeeming and fulfilling are the same node's two roles, and
    // the source it serves from is this node's own collections either way.
    let source: Arc<dyn crate::transport::ManifestSource> =
        StoreManifestSource::new((*store).clone(), id_clone, browse_key);
    let ep = ensure_endpoint(&endpoint, &own_npub, &transport_key, source).await.map_err(cmd_err)?;

    // The gate runs INSIDE `fetch_manifest`, before the acknowledgement — see its doc comment. If it
    // ran out here the ACK would already have been sent, and a manifest we reject (wrong author,
    // undecryptable, incomplete) would still have burned the ticket, forcing the human to ask again
    // and the owner to approve again.
    let mut imported: Option<crate::commands::browse::ImportedManifest> = None;
    fetch_manifest(&ep, &ticket, |payload| {
        let raw = std::str::from_utf8(payload.as_bytes())
            .map_err(|_| anyhow::anyhow!("the manifest that arrived was not text"))?;
        imported = Some(
            accept_manifest_bytes(
                &npub,
                Some(&ticket.slug),
                raw,
                newest_fingerprint.as_deref(),
                &store,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
        Ok(())
    })
    .await
    .map_err(cmd_err)?;
    // Unreachable unless the gate returned `Ok` without setting this — a `fetch_manifest` that
    // acknowledged without running the gate. Stated rather than unwrapped.
    imported.ok_or_else(|| "The manifest was acknowledged but never accepted.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request id must be unguessable and unique per approval — it is the ticket's primary binding,
    /// so a collision would make one approval's spent-bit govern another's.
    #[test]
    fn request_ids_are_unique_and_128_bit() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32, "16 bytes, hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A ticket DM that is not a ticket must be refused as unreadable, not mis-parsed into a ticket
    /// with empty bindings — `verify_shape` is the second line, but serde is the first.
    #[test]
    fn a_non_ticket_body_is_not_a_ticket() {
        assert!(serde_json::from_str::<TransportTicket>(r#"{"hb":"manifest_request"}"#).is_err());
        // A structurally complete body with the WRONG discriminator parses, then fails the shape
        // check — which is how a `manifest_request` can never be redeemed as a ticket.
        let wrong = serde_json::json!({
            "hb": "manifest_request",
            "ticket_v": 1,
            "request_id": "r",
            "slug": "s",
            "node_addr": "a",
            "issued_at": 1
        });
        let t: TransportTicket = serde_json::from_value(wrong).unwrap();
        assert!(t.verify_shape().is_err(), "the discriminator is checked, not assumed");
    }
}
