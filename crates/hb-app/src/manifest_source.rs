//! The owner side's [`ManifestSource`] over real app state (M18 W4).
//!
//! `transport.rs` deliberately holds no store handle — the plane is a protocol, and its tests bind
//! real QUIC endpoints against in-memory fakes. This module is the one place the protocol meets the
//! data directory, and it is small on purpose: three methods, each a thin composition of things that
//! already existed.
//!
//! **What it cannot do is the point.** `payload` takes a slug and returns a [`ManifestPayload`].
//! There is no path parameter and no byte-slice parameter anywhere in the trait, so this
//! implementation could not answer with a collection file even if a future edit tried to — INV-4′
//! mechanism 1 reaching one layer up from the newtype into the seam.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use hb_core::{ContactStanding, Identity, ManifestPayload};

use crate::commands::collection::build_slug_manifest;
use crate::identity_state::SessionBrowseKey;
use crate::store::DataStore;
use crate::transport::{IssuedTicket, ManifestSource};

/// Read `npub`'s **live** standing with this identity, in the order that makes the strictest answer
/// win: an explicit block, then an explicit decline, then contact-hood.
///
/// `Unknown` is returned for a peer that is not a saved contact — deliberately, and deliberately
/// refused by `authorize_redemption`. A redeemer this node can no longer identify is not "probably
/// fine": if the contact was removed since the approval, the human who approved it is no longer in a
/// relationship with the asker, and the ticket should not outlive that.
pub fn contact_standing(store: &DataStore, npub: &str) -> ContactStanding {
    if store.load_dm_blocked().map(|b| b.iter().any(|n| n == npub)).unwrap_or(false) {
        return ContactStanding::Blocked;
    }
    if store.load_dm_declined().map(|d| d.iter().any(|(n, _)| n == npub)).unwrap_or(false) {
        return ContactStanding::Declined;
    }
    let known = store
        .load_contact(&crate::store::CachedPeer::pubkey_hash(npub))
        .map(|c| c.is_some())
        .unwrap_or(false);
    if known { ContactStanding::Good } else { ContactStanding::Unknown }
}

/// The session's [`ManifestSource`]: the data directory plus the two keys needed to author an
/// envelope. Holds owned copies because the plane's accept loop outlives any request handler.
pub struct StoreManifestSource {
    store: DataStore,
    identity: Identity,
    browse_key: SessionBrowseKey,
}

impl StoreManifestSource {
    pub fn new(store: DataStore, identity: Identity, browse_key: SessionBrowseKey) -> Arc<Self> {
        Arc::new(Self { store, identity, browse_key })
    }
}

impl ManifestSource for StoreManifestSource {
    /// What this node issued for `request_id`, with standing re-read now rather than trusted from
    /// the record. `None` for an id we never issued — which is the same refusal a forged ticket gets,
    /// by way of the shared constant in `transport.rs`.
    fn issued(&self, request_id: &str) -> Option<IssuedTicket> {
        let rec = self.store.load_issued_ticket(request_id).ok().flatten()?;
        let standing = contact_standing(&self.store, &rec.redeemer_npub);
        Some(IssuedTicket {
            ticket: rec.ticket,
            standing,
            already_consumed: rec.consumed_at.is_some(),
        })
    }

    /// Build the manifest for an authorized request, from the same pure core `export_manifest`
    /// wraps — so the transport can never serve a tree the export would have described differently.
    /// Sealing is what applies the 8 MiB ceiling; an over-cap collection is refused here, before a
    /// byte moves, by [`ManifestPayload::seal`].
    fn payload(&self, slug: &str) -> Result<ManifestPayload> {
        let envelope =
            build_slug_manifest(slug, &self.store, &self.identity, self.browse_key.bytes())
                .map_err(|e| anyhow!("{e}"))?;
        ManifestPayload::seal(&envelope).map_err(|e| anyhow!("{e}"))
    }

    /// Persist the receipt. Best-effort by signature (the trait returns nothing) but not by
    /// consequence: a lost write means a replay of this ticket would be served again, so the failure
    /// is logged loudly rather than swallowed.
    fn record_consumed(&self, receipt: &hb_core::ticket::ConsumedTicket) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Err(e) =
            self.store.mark_ticket_consumed(receipt.request_id(), receipt.delivered_bytes(), now)
        {
            tracing::error!(
                request_id = receipt.request_id(),
                "failed to persist a manifest receipt — this ticket could be redeemed twice: {e}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::IssuedTicketRecord;
    use hb_core::TransportTicket;

    fn store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path()).unwrap();
        (dir, store)
    }

    fn npub_of(id: &Identity) -> String {
        use nostr::prelude::ToBech32;
        id.public_key().to_bech32().unwrap()
    }

    /// The core of the durable half: a ticket issued, then marked consumed, is reported as spent —
    /// which is what makes a replay after a restart refuse instead of serving a second manifest.
    #[test]
    fn a_consumed_ticket_reads_back_as_already_consumed() {
        let (_dir, store) = store();
        let id = Identity::generate();
        let npub = npub_of(&id);
        let ticket = TransportTicket::issue("req-a", "slug-a", "addr", 1_700_000_000);
        store
            .record_issued_ticket(&IssuedTicketRecord {
                ticket,
                redeemer_npub: npub,
                consumed_at: None,
                delivered_bytes: None,
            })
            .unwrap();

        let before = store.load_issued_ticket("req-a").unwrap().unwrap();
        assert!(before.consumed_at.is_none(), "a freshly issued ticket is unspent");

        store.mark_ticket_consumed("req-a", 4096, 1_700_000_100).unwrap();
        let after = store.load_issued_ticket("req-a").unwrap().unwrap();
        assert_eq!(after.consumed_at, Some(1_700_000_100));
        assert_eq!(after.delivered_bytes, Some(4096));
    }

    /// An invented request id must be indistinguishable from a forged ticket — the source reports
    /// nothing, and `transport.rs`'s single refusal constant says the same thing either way.
    #[test]
    fn an_unissued_request_id_is_unknown() {
        let (_dir, store) = store();
        assert!(store.load_issued_ticket("never-issued").unwrap().is_none());
    }

    /// Standing is read at redeem time, so blocking a contact after approval refuses the redemption
    /// **without** spending the ticket (owner ruling 2026-07-30: restoring the contact restores the
    /// approval). This asserts the standing half; `hb_core::ticket` owns the no-spend half.
    #[test]
    fn blocking_a_contact_after_approval_downgrades_its_standing() {
        let (_dir, store) = store();
        let npub = npub_of(&Identity::generate());
        assert_eq!(
            contact_standing(&store, &npub),
            ContactStanding::Unknown,
            "a peer that was never a contact is Unknown, not Good"
        );
        store.save_dm_blocked(std::slice::from_ref(&npub)).unwrap();
        assert_eq!(contact_standing(&store, &npub), ContactStanding::Blocked);
    }

    /// A block outranks a decline, and both outrank contact-hood — the strictest answer wins, so a
    /// blocked peer that is also still a saved contact does not read as `Good`.
    #[test]
    fn a_block_outranks_a_decline() {
        let (_dir, store) = store();
        let npub = npub_of(&Identity::generate());
        store.save_dm_declined(&[(npub.clone(), 1_700_000_000)]).unwrap();
        assert_eq!(contact_standing(&store, &npub), ContactStanding::Declined);
        store.save_dm_blocked(std::slice::from_ref(&npub)).unwrap();
        assert_eq!(contact_standing(&store, &npub), ContactStanding::Blocked);
    }

    /// The cap bounds the audit tail, never the live approvals: over-cap pruning evicts consumed
    /// records only, because evicting an unspent one silently revokes an approval a human gave and
    /// the asker would then see the refusal reserved for a forgery.
    #[test]
    fn pruning_never_evicts_an_unspent_ticket() {
        let mut m = std::collections::HashMap::new();
        for i in 0..(crate::store::ISSUED_TICKET_CAP + 50) {
            let id = format!("req-{i}");
            // Every record is UNSPENT and older than the last, so a naive LRU would evict the oldest.
            m.insert(
                id.clone(),
                IssuedTicketRecord {
                    ticket: TransportTicket::issue(&id, "slug", "addr", i as u64),
                    redeemer_npub: "npub-x".into(),
                    consumed_at: None,
                    delivered_bytes: None,
                },
            );
        }
        let before = m.len();
        crate::store::prune_issued_tickets(&mut m);
        assert_eq!(m.len(), before, "no unspent approval may be dropped to make room");
    }
}
