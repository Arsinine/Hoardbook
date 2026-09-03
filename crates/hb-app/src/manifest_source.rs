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
use hb_core::{Identity, ManifestPayload};

use crate::commands::collection::build_slug_manifest;
use crate::identity_state::SessionBrowseKey;
use crate::manifest_cache;
use crate::store::DataStore;
use crate::transport::{IssuedTicket, ManifestSource};

/// Unix seconds now — the cache's LRU recency stamp on a re-serve read.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// `contact_standing(store, npub) -> ContactStanding` (strictest-wins: blocked → declined →
// contact-hood → Unknown) was deleted 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only
// block interaction i.e. chats, it should not meaningfully affect other traffic."*). Its only two
// production callers were the standing reads this change removed from `issued()` below and from
// `auto_approve`'s step (2). Blocking still gates chat/DM acceptance — `commands/chat.rs` reads
// `dm_blocked` directly and never used this vocabulary.

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
    /// What this node issued for `request_id`. `None` for an id we never issued — which is the same
    /// refusal a forged ticket gets, by way of the shared constant in `transport.rs`. Deliberately
    /// NOT read here: any live contact standing — blocking, removing, or declining the redeemer
    /// changes nothing on the serve path (owner ruling 2026-09-03, QURATOR-177: blocking gates
    /// chat/DM interaction only). The ticket itself and the spent bit are the whole decision.
    fn issued(&self, request_id: &str) -> Option<IssuedTicket> {
        let rec = self.store.load_issued_ticket(request_id).ok().flatten()?;
        Some(IssuedTicket {
            ticket: rec.ticket,
            already_consumed: rec.consumed_at.is_some(),
        })
    }

    /// Build the manifest for an authorized request, from the same pure core `export_manifest`
    /// wraps — so the transport can never serve a tree the export would have described differently.
    ///
    /// **Built from the collection as it is NOW, not as it was at approval** (owner ruling ②,
    /// 2026-07-31). Tickets do not expire, so this can deliver a newer list than the owner reviewed.
    /// Ratified as intended: see `send_full_list`'s note for why freezing it would be worse.
    /// Sealing is what applies the 16 MiB ceiling; an over-cap collection is refused here, before a
    /// byte moves, by [`ManifestPayload::seal`].
    ///
    /// The parameter is the `request_id`, not the slug: the record keyed by it says WHICH kind of
    /// serve this is. `ticket.author_npub: None` (or a record without `served_fingerprint`) is the
    /// owner-issued case above, unchanged — the slug is recovered from the record and the manifest
    /// rebuilt from the collection as it is NOW. `author_npub: Some` AND `served_fingerprint:
    /// Some` is a carrier-4 re-serve (QURATOR-79): the exact `(author_npub, slug, fingerprint)`
    /// cache entry the human approved when `send_cached_manifest` minted this ticket is fetched,
    /// with NO fallback — resolution happened once, at mint; serve time replays it. A cache MISS on
    /// a re-serve errors rather than falling through to `build_slug_manifest`, because a
    /// "helpful" fallback would silently serve C's OWN same-slug collection as if it were A's
    /// (slugs are bare names; C may hold B's «roms» and A's «roms» both).
    fn payload(&self, request_id: &str) -> Result<ManifestPayload> {
        let rec = self
            .store
            .load_issued_ticket(request_id)
            .map_err(|e| anyhow!("look up the issued-ticket record: {e}"))?
            .ok_or_else(|| anyhow!("no issued-ticket record for '{request_id}'"))?;

        // The cache-serving branch: BOTH fields must be present. `author_npub` alone is not enough
        // (a record written before `served_fingerprint` existed loads as `None`, and that IS the
        // owner-issued case for it), and `served_fingerprint` alone would resolve an authorless
        // cache key. Read-only use of the cache — `manifest_cache::put` has exactly one production
        // writer, in `commands/browse.rs`, pinned by a CI sweep.
        if let (Some(author_npub), Some(fingerprint)) =
            (&rec.ticket.author_npub, &rec.served_fingerprint)
        {
            let json = manifest_cache::get(
                &self.store.manifest_cache_dir(),
                author_npub,
                &rec.ticket.slug,
                fingerprint,
                now_secs(),
            )
            .ok_or_else(|| {
                anyhow!(
                    "the approved copy of '{slug}' by {author} is no longer in the manifest cache — \
                     ask the owner to re-send it",
                    slug = rec.ticket.slug,
                    author = crate::logging::trunc_npub(author_npub),
                )
            })?;
            let envelope = hb_core::manifest::ManifestEnvelope::from_json(&json)
                .map_err(|e| anyhow!("the cached manifest is not readable: {e}"))?;
            return ManifestPayload::seal(&envelope).map_err(|e| anyhow!("{e}"));
        }

        // The owner-issued path, byte-for-byte what it always did: rebuild from the collection as
        // it is NOW (owner ruling ②).
        let envelope = build_slug_manifest(
            &rec.ticket.slug,
            &self.store,
            &self.identity,
            self.browse_key.bytes(),
        )
        .map_err(|e| anyhow!("{e}"))?;
        ManifestPayload::seal(&envelope).map_err(|e| anyhow!("{e}"))
    }

    /// Persist the receipt, and **report failure** rather than logging past it. A lost write leaves
    /// the ticket unspent on disk, so the plane needs to know in order to fail closed — it poisons
    /// the request for the rest of the session instead of allowing a second delivery.
    fn record_consumed(&self, receipt: &hb_core::ticket::ConsumedTicket) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.store
            .mark_ticket_consumed(receipt.request_id(), receipt.delivered_bytes(), now)
            .map_err(|e| anyhow!("persist the manifest receipt: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::IssuedTicketRecord;
    use hb_core::{Collection, DirectoryItem, ItemType, TransportTicket, Visibility};

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
        let ticket = TransportTicket::issue("req-a", "slug-a", "addr", 1_700_000_000, Some("n1"));
        store
            .record_issued_ticket(&IssuedTicketRecord {
                ticket,
                redeemer_npub: npub,
                consumed_at: None,
                delivered_bytes: None,
                served_fingerprint: None,
            })
            .unwrap();

        let before = store.load_issued_ticket("req-a").unwrap().unwrap();
        assert!(before.consumed_at.is_none(), "a freshly issued ticket is unspent");

        store.mark_ticket_consumed("req-a", 4096, 1_700_000_100).unwrap();
        let after = store.load_issued_ticket("req-a").unwrap().unwrap();
        assert_eq!(after.consumed_at, Some(1_700_000_100));
        assert_eq!(after.delivered_bytes, Some(4096));
    }

    /// **One ask admits ONE ticket — the defect that survived two rounds of fixes.**
    ///
    /// Binding the ticket to a nonce was not enough, and neither was releasing the ask on failure.
    /// The peer being asked is the party that *receives* the nonce, so it could send ticket A to an
    /// address that times out, then B, C, D… each with a fresh owner-chosen `request_id` and a fresh
    /// node address. Every failure released the ask and the next ticket claimed it — an automatic
    /// dial per attempt, no user action, to any endpoint the peer chose.
    ///
    /// The claim is durable and taken BEFORE the dial, so a retry must be the same ticket and
    /// anything else needs a fresh ask.
    #[test]
    fn one_ask_admits_one_ticket_and_a_retry_must_be_that_same_ticket() {
        use crate::store::AskClaim;
        let (_dir, store) = store();
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp", "2026-01-01T00:00:00Z", "n-1")
            .unwrap();

        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", "req-A").unwrap(),
            AskClaim::Granted
        );
        // The same ticket may retry after a failed dial — that costs nothing and must stay possible.
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", "req-A").unwrap(),
            AskClaim::Granted
        );
        // A DIFFERENT ticket echoing the same nonce is refused, however many the peer invents.
        for invented in ["req-B", "req-C", "req-D"] {
            assert_eq!(
                store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", invented).unwrap(),
                AskClaim::ClaimedByAnother,
                "a peer must not turn one ask into a dial per ticket"
            );
        }

        // Answered → spent, durably, so a restart cannot resurrect the authorization.
        store.spend_manifest_ask("npub1a", "npub1a", "criterion", "n-1").unwrap();
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", "req-A").unwrap(),
            AskClaim::Spent
        );

        // A fresh ask is a fresh authorization: new nonce, claim and spent flags cleared.
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp", "2026-01-02T00:00:00Z", "n-2")
            .unwrap();
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-2", "req-E").unwrap(),
            AskClaim::Granted
        );
        // …and the OLD nonce is dead.
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "n-1", "req-A").unwrap(),
            AskClaim::Unsolicited
        );
    }

    /// A wrong nonce, an unknown peer, and a pre-ruling ask (empty stored nonce) are all
    /// `Unsolicited` — fail closed by construction rather than by a branch.
    #[test]
    fn a_ticket_that_answers_no_ask_of_ours_is_never_granted() {
        use crate::store::AskClaim;
        let (_dir, store) = store();
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp", "2026-01-01T00:00:00Z", "n-1")
            .unwrap();
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "wrong", "r").unwrap(),
            AskClaim::Unsolicited
        );
        assert_eq!(
            store.claim_manifest_ask("npub1zzz", "npub1zzz", "criterion", "n-1", "r").unwrap(),
            AskClaim::Unsolicited
        );
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "other", "n-1", "r").unwrap(),
            AskClaim::Unsolicited
        );
        // An empty nonce on either side never matches.
        assert_eq!(
            store.claim_manifest_ask("npub1a", "npub1a", "criterion", "", "r").unwrap(),
            AskClaim::Unsolicited
        );
    }

    /// **A completing ticket must not spend a NEWER ask.** There is one trace entry per
    /// `(npub, slug)`, so a re-ask overwrites it. If ticket A is in flight, the user re-asks (nonce
    /// B), and A then completes, an unconditional write marks B spent — and the legitimate answer to
    /// the new ask afterwards is refused, with no way for the user to see why.
    #[test]
    fn spending_an_ask_is_conditional_on_the_nonce_that_was_answered() {
        let (_dir, store) = store();
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp", "2026-01-01T00:00:00Z", "nonce-A")
            .unwrap();
        // The user re-asks while ticket A is still in flight — same key, new nonce.
        store
            .record_manifest_ask("npub1a", "npub1a", "criterion", "fp", "2026-01-01T00:05:00Z", "nonce-B")
            .unwrap();

        // Ticket A now completes. It must NOT spend B's ask.
        store.spend_manifest_ask("npub1a", "npub1a", "criterion", "nonce-A").unwrap();
        let asks = store.load_manifest_asks().unwrap();
        let kept = asks.get("npub1a|npub1a|criterion").expect("the newer ask is still there");
        assert_eq!(kept.nonce, "nonce-B");
        assert!(!kept.spent, "an older completion must not spend the newer ask");

        // And the ask that IS answered is spent.
        store.spend_manifest_ask("npub1a", "npub1a", "criterion", "nonce-B").unwrap();
        assert!(
            store.load_manifest_asks().unwrap()["npub1a|npub1a|criterion"].spent,
            "one ask, one auto-dial — the answered ask is spent"
        );
    }

    /// An invented request id must be indistinguishable from a forged ticket — the source reports
    /// nothing, and `transport.rs`'s single refusal constant says the same thing either way.
    #[test]
    fn an_unissued_request_id_is_unknown() {
        let (_dir, store) = store();
        assert!(store.load_issued_ticket("never-issued").unwrap().is_none());
    }

    // DELETED 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only block interaction i.e.
    // chats, it should not meaningfully affect other traffic."*): `blocking_a_contact_after_
    // approval_downgrades_its_standing` and `a_block_outranks_a_decline` pinned `contact_standing`
    // itself — the strictest-wins vocabulary that fed the serve-path and auto-approve standing gates
    // this change removes. The block-vs-decline ordering they exercised is still real for CHAT
    // acceptance, but chat reads `dm_blocked`/`dm_declined` directly (`commands/chat.rs`) and never
    // went through this fn, so there is nothing left for these tests to pin. Replaced by the test
    // directly below, which pins the new seam behaviour: blocking changes nothing on the serve path.

    /// **Blocking does not gate the serve path at its seam with real app state** (owner ruling
    /// 2026-09-03, QURATOR-177). The owner issues a ticket; the redeemer is then BLOCKED (and,
    /// independently, declined); `StoreManifestSource::issued` still returns the ticket with the
    /// same spent bit as an unblocked redeemer — because the serve decision is now exactly the
    /// ticket + the spent bit, with no standing read anywhere between the data directory and the
    /// plane. This is the seam-level twin of `transport.rs`'s
    /// `a_blocked_redeemer_with_a_valid_ticket_is_still_served` (which proves the same thing through
    /// real QUIC), and it exists because the plane-level test cannot see WHICH store state the
    /// decision ignored — only this one, backed by the real `DataStore`, can.
    ///
    /// MUTATION (P-10) — the orchestrator applies one of these and must see this test red:
    ///   (a) In `StoreManifestSource::issued` (this file), re-introduce a standing gate, e.g.
    ///       `if self.store.load_dm_blocked().map(|b| b.iter().any(|n| n == &rec.redeemer_npub))
    ///       .unwrap_or(false) { return None; }` after the `load_issued_ticket` line — the
    ///       blocked assertion reds (issued() returns None).
    ///   (b) In `transport.rs`'s `serve_manifest_stream`, refuse after a successful
    ///       `issued()` on any store-derived hint — the plane-level twin test reds instead. This
    ///       test's purpose is to name WHICH seam the mutation must hit to be caught.
    #[test]
    fn a_blocked_redeemers_ticket_is_still_issued() {
        let (_dir, store) = store();
        let npub = npub_of(&Identity::generate());
        store
            .record_issued_ticket(&IssuedTicketRecord {
                ticket: TransportTicket::issue("req-a", "slug-a", "addr", 1_700_000_000, Some("n1")),
                redeemer_npub: npub.clone(),
                consumed_at: None,
                delivered_bytes: None,
                served_fingerprint: None,
            })
            .unwrap();

        // Sanity: the block and the decline really are in the store — without this the test would
        // pass vacuously through a store that never persisted them.
        store.save_dm_blocked(std::slice::from_ref(&npub)).unwrap();
        store.save_dm_declined(&[(npub.clone(), 1_700_000_000)]).unwrap();
        assert!(
            store.load_dm_blocked().unwrap().iter().any(|n| n == &npub),
            "precondition: the redeemer really is blocked"
        );

        // The seam: `issued` still hands back the ticket, unspent — the same answer as for an
        // unblocked redeemer. There is no `standing` field left to compare, and that absence IS
        // the pinned property.
        let src = StoreManifestSource::new(
            store.clone(),
            Identity::generate(),
            SessionBrowseKey::new([7u8; 32]),
        );
        let issued = src.issued("req-a").expect(
            "a blocked redeemer holding a valid unspent ticket must still be issued for serving",
        );
        assert!(
            !issued.already_consumed,
            "the spent bit is still reported faithfully — blocking did not masquerade as spending"
        );

        // And the twin check on the OTHER standing the old fn conflated: an UNKNOWN redeemer (never
        // a contact at all) is served the same way. The old `Unknown` arm refused; it must not
        // return under any name.
        store
            .record_issued_ticket(&IssuedTicketRecord {
                ticket: TransportTicket::issue("req-u", "slug-a", "addr", 1_700_000_000, Some("n2")),
                redeemer_npub: "npub1unknown".into(),
                consumed_at: None,
                delivered_bytes: None,
                served_fingerprint: None,
            })
            .unwrap();
        assert!(
            src.issued("req-u").is_some(),
            "an unknown redeemer (never a contact) is served too — the old Unknown refusal is gone"
        );
    }

    /// Owner ruling 2026-09-01: the issued-ticket map is UNBOUNDED — no record is ever evicted,
    /// spent or unspent. Eviction was never load-bearing (an unknown `request_id` is refused exactly
    /// like a forgery, see [`StoreManifestSource::issued`]); the old 512 cap only discarded audit
    /// history, and dropping an *unspent* one would have silently revoked a human's approval.
    ///
    /// Driven through the real `record_issued_ticket` path rather than a bare map, so it ends where
    /// production ends: a reinstated prune anywhere in that call chain reds this test.
    ///
    /// MUTATION (P-10) — resolved by containing function: in `DataStore::record_issued_ticket`
    /// (`store.rs`), re-add a truncation after the insert, e.g.
    /// `if m.len() > 512 { let k = m.keys().next().cloned().unwrap(); m.remove(&k); }`.
    /// The count assertion below reds.
    #[test]
    fn no_issued_ticket_record_is_ever_evicted() {
        let (_dir, store) = store();
        const N: usize = 600; // comfortably past the 512 the removed cap used to enforce
        for i in 0..N {
            let id = format!("req-{i}");
            store
                .record_issued_ticket(&IssuedTicketRecord {
                    ticket: TransportTicket::issue(&id, "slug", "addr", i as u64, None),
                    redeemer_npub: "npub-x".into(),
                    // Half are CONSUMED — under the old cap these were exactly the eviction candidates.
                    consumed_at: (i % 2 == 0).then_some(1_700_000_000),
                    delivered_bytes: None,
                    served_fingerprint: None,
                })
                .unwrap();
        }
        // EVERY record, not a sample: an eviction policy drops arbitrary keys, so spot-checking a
        // handful passes by luck. (Observed 2026-09-01 — the sampled version of this test survived
        // a mutation that was evicting ~88 of the 600.)
        let missing: Vec<usize> = (0..N)
            .filter(|i| store.load_issued_ticket(&format!("req-{i}")).unwrap().is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "{} of {N} records were evicted (first few: {:?}) — the map must retain every record, \
             consumed or not",
            missing.len(),
            &missing[..missing.len().min(5)]
        );
    }

    // ── QURATOR-79 Carrier 4 — the cache-serving branch of `payload` ─────────────────────────────

    fn a_record(request_id: &str, author_npub: Option<String>, served_fingerprint: Option<String>) -> IssuedTicketRecord {
        let mut ticket = TransportTicket::issue(request_id, "roms", "addr", 1_700_000_000, None);
        ticket.author_npub = author_npub;
        IssuedTicketRecord {
            ticket,
            redeemer_npub: "npub1x".into(),
            consumed_at: None,
            delivered_bytes: None,
            served_fingerprint,
        }
    }

    /// Required test 1 — an owner-issued `request_id` (NO `served_fingerprint`) never falls through
    /// to the cache. The dangerous shape is `author_npub` PRESENT with `served_fingerprint` absent
    /// (a pre-Carrier-4 record, or a hand-crafted one): a sloppy branch that engages on the author
    /// ALONE would re-resolve "newest for (author, slug)" and serve the cache; the correct code
    /// takes the owner branch and rebuilds from the collection as it is NOW (ruling ②).
    ///
    /// MUTATION (reds this test): in `StoreManifestSource::payload`
    /// (crates/hb-app/src/manifest_source.rs, the `impl ManifestSource for StoreManifestSource`
    /// block), weaken the two-field guard `if let (Some(author_npub), Some(fingerprint)) = ...` to
    /// engage on the author ALONE — `if let Some(author_npub) = &rec.ticket.author_npub` — and
    /// supply the `manifest_cache::get` fingerprint argument by re-resolving "newest for (author,
    /// slug)" at serve time (e.g. via the same scan `send_cached_manifest_inner`'s
    /// `newest_cached_for` performs). That is exactly the serve-time re-guess the design forbids:
    /// the branch engages for the record below (author set, fingerprint None), the cache DOES hold
    /// an entry for `(that author, "roms")`, the mutant serves it — and `unwrap_err()` panics.
    #[test]
    fn an_owner_issued_request_id_never_falls_through_to_the_cache() {
        let (_dir, store) = store();
        let source = StoreManifestSource::new(
            store.clone(),
            Identity::generate(),
            SessionBrowseKey::new([7u8; 32]),
        );
        // The bait: a cache entry EXISTS for (author, "roms") — and no draft for the slug. The
        // owner branch names the missing DRAFT; the cache branch would silently serve the entry.
        let author = Identity::generate();
        let author_npub = npub_of(&author);
        let env = hb_core::build_manifest_envelope(
            &author,
            "roms",
            &[9u8; 32],
            "fp-bait",
            1_700_000_000,
            &[r#"{"slug":"roms","entries":[]}"#.to_string()],
        )
        .unwrap();
        crate::manifest_cache::put(
            &store.manifest_cache_dir(),
            &author_npub,
            "roms",
            "fp-bait",
            &env.to_json().unwrap(),
            1_700_000_000,
            crate::manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .unwrap();
        // `author_npub` set but `served_fingerprint` ABSENT: an owner-issued record for the same
        // slug the cache happens to hold. Only the fingerprint's absence keeps this on the owner
        // path — which is exactly the optional-field condition TICKET_V's exemption rests on.
        store
            .record_issued_ticket(&a_record("req-owner", Some(author_npub), None))
            .unwrap();
        let err = source.payload("req-owner").unwrap_err().to_string();
        assert!(
            err.contains("No draft found"),
            "an owner-issued record must take the build_slug_manifest branch, got: {err}"
        );
    }

    /// Required test 2 — the LOAD-BEARING one. A `request_id` whose `served_fingerprint` names a
    /// cache-MISS must ERROR, never fall through to `build_slug_manifest` or any other source. A
    /// "helpful" fallback would silently serve C's own same-slug collection as if it were A's.
    ///
    /// MUTATION (reds this test): in `StoreManifestSource::payload` (same containing impl block),
    /// turn the cache-miss into a FALLTHROUGH — nest the serve inside the hit so a miss skips it
    /// and execution reaches the `build_slug_manifest` call below the `if let`:
    /// change `let json = manifest_cache::get(...).ok_or_else(|| ...)?` to
    /// `if let Some(json) = manifest_cache::get(...) { ...return the sealed payload...; }` —
    /// the "helpful fallback" the design names. With the draft saved below, the mutant serves
    /// C's own «roms» envelope as if it were A's, the call returns `Ok`, and `unwrap_err()` panics.
    #[test]
    fn a_cache_miss_on_a_re_serve_errors_it_never_falls_through() {
        let (_dir, store) = store();
        let source = StoreManifestSource::new(
            store.clone(),
            Identity::generate(),
            SessionBrowseKey::new([7u8; 32]),
        );
        // The trap: C DOES own a same-slug collection (the draft exists), and the request carries
        // an author + fingerprint that resolves to NO cache entry. Serving C's own «roms» here is
        // exactly the cross-tenant collision the fence exists to prevent.
        store
            .save_collection_draft(&Collection {
                slug: "roms".into(),
                path_alias: "roms".into(),
                description: None,
                item_count: 1,
                est_size: None,
                content_types: vec!["video".into()],
                tags: vec![],
                languages: vec![],
                visibility: Visibility::Public,
                sorted: false,
                last_updated: chrono::Utc::now(),
                listing: vec![DirectoryItem {
                    name: "C's own rom".into(),
                    item_type: ItemType::File,
                    size: None,
                    format: None,
                    year: None,
                    tags: vec![],
                    note: None,
                    children: vec![],
                }],
            })
            .unwrap();
        store
            .record_issued_ticket(&a_record(
                "req-re-serve",
                Some("npub1author".into()),
                Some("fp-not-in-the-cache".into()),
            ))
            .unwrap();
        let err = source.payload("req-re-serve").unwrap_err().to_string();
        assert!(
            err.contains("no longer in the manifest cache"),
            "a cache miss on a re-serve must error naming the cache, never serve a fallback: {err}"
        );
    }

    /// Required test 3 — an envelope failing `verify_author(requested_author)` is refused BEFORE
    /// `seal`, at MINT time: the §2 C-side provenance fence, driven through the real
    /// `send_cached_manifest_inner` (the same seam the WAN harness uses). Without it a D asking for
    /// `(author = A, slug = s)` could be served B's same-slug envelope and cost C a spurious ticket
    /// spend.
    ///
    /// MUTATION (reds this test): in `send_cached_manifest_inner`
    /// (crates/hb-app/src/commands/fulfil.rs), delete the
    /// `envelope.verify_author(&expected_author).map_err(...)?` call — the mutated mint proceeds to
    /// bind a real endpoint and mint a ticket instead of refusing at the fence, so the error below
    /// stops matching "could not be verified as this peer's" and the assert fails.
    #[tokio::test]
    async fn an_envelope_failing_the_requested_author_is_refused_at_mint() {
        use crate::identity_state::AppIdentity;
        use crate::transport_state::new_shared_endpoint;

        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path()).unwrap();
        let identity: crate::identity_state::SharedIdentity =
            std::sync::Arc::new(tokio::sync::RwLock::new(Some(AppIdentity::generate())));

        // The attack shape: an envelope AUTHORED by B sitting in the cache under A's key — the
        // key/author mismatch the §2 fence exists to catch (B relaying its own same-slug envelope
        // into an A-scoped ask). The cache key resolves; the signature does not.
        let b = Identity::generate();
        let b_npub = npub_of(&b);
        let env = hb_core::build_manifest_envelope(
            &b,
            "roms",
            &[9u8; 32],
            "fp-mint",
            1_700_000_000,
            &[r#"{"slug":"roms","entries":[]}"#.to_string()],
        )
        .unwrap();
        let a = Identity::generate();
        let a_npub = npub_of(&a);
        let d = Identity::generate();
        let d_npub = npub_of(&d);
        assert_ne!(a_npub, b_npub, "the fixture needs two distinct authors");
        assert_ne!(env.author_npub, a_npub, "the envelope must be B's, not A's");
        crate::manifest_cache::put(
            &store.manifest_cache_dir(),
            &a_npub,
            "roms",
            "fp-mint",
            &env.to_json().unwrap(),
            1_700_000_000,
            crate::manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .unwrap();

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::commands::fulfil::send_cached_manifest_inner(
                d_npub.clone(),
                a_npub.clone(),
                "roms".into(),
                None,
                &identity,
                &store,
                &crate::net::new_shared(),
                &new_shared_endpoint(),
            ),
        )
        .await
        .expect("the call must resolve — a hang is a finding")
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("could not be verified as this peer's"),
            "B's envelope under an A-scoped ask must be refused at the mint-time author fence, got: {err}"
        );
    }
}
