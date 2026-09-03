//! The owner side's [`ManifestSource`] over real app state (M18 W4).
//!
//! `transport.rs` deliberately holds no store handle — the plane is a protocol, and its tests bind
//! real QUIC endpoints against in-memory fakes. This module is the one place the protocol meets the
//! data directory, and it is small on purpose: one method, a thin composition of things that
//! already existed.
//!
//! **The issued-ticket lookup is gone** (owner ruling 2026-09-03, QURATOR-177 Option E): until
//! that ruling this module also answered "what did we issue for `request_id`, and is it spent?"
//! — the serve path's only authorization read, backed by hb-app's issued-ticket ledger.
//! Authorization now happens at ASK time (the standing grant checked under the NIP-17 seal in
//! `auto_approve`), the ticket is address delivery, and `payload` resolves by the ticket's OWN
//! fields — `author_npub` for the branch, `slug` for the key — with no ledger lookup anywhere.
//!
//! **What it cannot do is the point.** `payload` takes a slug and returns a [`ManifestPayload`].
//! There is no path parameter and no byte-slice parameter anywhere in the trait, so this
//! implementation could not answer with a collection file even if a future edit tried to — INV-4′
//! mechanism 1 reaching one layer up from the newtype into the seam.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use hb_core::{Identity, ManifestPayload, TransportTicket};

use crate::commands::collection::build_slug_manifest;
use crate::identity_state::SessionBrowseKey;
use crate::manifest_cache;
use crate::store::DataStore;
use crate::transport::ManifestSource;

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
    // `issued(&self, request_id)` — DELETED 2026-09-03, QURATOR-177 Option E (owner ruling:
    // authorization is the standing grant at ASK time; the ticket is address delivery). It read
    // the issued-ticket ledger and fed the serve path's per-request lookup; the ledger is deleted
    // and with it this method. Do not re-introduce any request_id → record lookup here.


    /// Build the manifest for an authorized request, from the same pure core `export_manifest`
    /// wraps — so the transport can never serve a tree the export would have described differently.
    ///
    /// **Built from the collection as it is NOW, not as it was at approval** (owner ruling ②,
    /// 2026-07-31). Tickets do not expire, so this can deliver a newer list than the owner reviewed.
    /// Ratified as intended: see `send_full_list`'s note for why freezing it would be worse.
    /// Sealing is what applies the 16 MiB ceiling; an over-cap collection is refused here, before a
    /// byte moves, by [`ManifestPayload::seal`].
    ///
    /// The parameter is the ticket itself (QURATOR-177 Option E): its OWN fields say WHICH kind
    /// of serve this is, with no ledger lookup. `ticket.author_npub: None` is the owner-issued
    /// case, unchanged — the manifest is rebuilt from the collection as it is NOW.
    /// `author_npub: Some` is a carrier-4 re-serve (QURATOR-79): the resolution target is
    /// **the newest cached copy for `(author_npub, slug)`** (QURATOR-177 slice 1, owner ruling
    /// 2026-09-03) — serve time resolves to whatever is currently cached for that author+slug,
    /// NOT the exact entry whose fingerprint `send_cached_manifest` minted the ticket against
    /// (resolution used to happen once, at mint; it no longer does). Serving a newer snapshot than
    /// the human approved is intended and matches the owner ruling above that rebuilds the owner's
    /// own collection as-it-is-NOW at redeem time, because tickets never expire — the refetch
    /// machinery updates the cache in place and the next re-serve serves what is there. A cache
    /// MISS on a re-serve errors rather than falling through to `build_slug_manifest`, because a
    /// "helpful" fallback would silently serve C's OWN same-slug collection as if it were A's
    /// (slugs are bare names; C may hold B's «roms» and A's «roms» both).
    fn payload(&self, ticket: &TransportTicket) -> Result<ManifestPayload> {
        // The cache-serving branch: `author_npub` ALONE is the Carrier-4 signal (QURATOR-177
        // slice 1). It used to additionally require `served_fingerprint` — under that rule a
        // record with `author_npub: Some` but `served_fingerprint: None` (a pre-field Carrier-4
        // mint) fell to the owner branch; `author_npub` is the real discriminator, so the
        // fingerprint is no longer read anywhere (the record itself is gone with the ledger,
        // QURATOR-177 Option E). Read-only use of the cache — `manifest_cache::put` has exactly
        // one production writer, in `commands/browse.rs`, pinned by a CI sweep.
        if let Some(author_npub) = &ticket.author_npub {
            let json = manifest_cache::get_latest(
                &self.store.manifest_cache_dir(),
                author_npub,
                &ticket.slug,
                now_secs(),
            )
            .ok_or_else(|| {
                anyhow!(
                    "the approved copy of '{slug}' by {author} is no longer in the manifest cache — \
                     ask the owner to re-send it",
                    slug = ticket.slug,
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
            &ticket.slug,
            &self.store,
            &self.identity,
            self.browse_key.bytes(),
        )
        .map_err(|e| anyhow!("{e}"))?;
        ManifestPayload::seal(&envelope).map_err(|e| anyhow!("{e}"))
    }

}

// `record_consumed(&self, receipt: &ConsumedTicket)` — DELETED 2026-09-03, QURATOR-177 Option E
// (owner ruling), with the receipt ledger it wrote and the plane's poisoning that consumed its
// errors. There is no spent bit to persist and no audit trail — both deliberately given up.

#[cfg(test)]
mod tests {
    use super::*;
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

    // `a_consumed_ticket_reads_back_as_already_consumed` — DELETED 2026-09-03, QURATOR-177
    // Option E (owner ruling): the issued-ticket ledger and its `consumed_at`/`delivered_bytes`
    // fields are deleted, so there is nothing to read back and no spent bit to report. Durable
    // replay protection was deliberately given up; what bounds repeat traffic is the asker-side
    // fingerprint-change trigger, never a serve-side spent bit.


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

    // `an_unissued_request_id_is_unknown` — DELETED 2026-09-03, QURATOR-177 Option E. It pinned
    // that the source could not answer for a request id it never issued — a property OF THE
    // LEDGER. With the ledger gone the serve path resolves by the ticket's own fields; an
    // unknowable request is indistinguishable because nothing is looked up, not because a map
    // lookup missed. The request-binding refusal survives in hb-core
    // (`a_ticket_is_bound_to_its_own_request`).


    // DELETED 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only block interaction i.e.
    // chats, it should not meaningfully affect other traffic."*): `blocking_a_contact_after_
    // approval_downgrades_its_standing` and `a_block_outranks_a_decline` pinned `contact_standing`
    // itself — the strictest-wins vocabulary that fed the serve-path and auto-approve standing gates
    // this change removes. The block-vs-decline ordering they exercised is still real for CHAT
    // acceptance, but chat reads `dm_blocked`/`dm_declined` directly (`commands/chat.rs`) and never
    // went through this fn, so there is nothing left for these tests to pin. Replaced by the test
    // directly below, which pins the new seam behaviour: blocking changes nothing on the serve path.

    // `a_blocked_redeemers_ticket_is_still_issued` — DELETED 2026-09-03, QURATOR-177 Option E.
    // It pinned that `StoreManifestSource::issued` ignored contact standing — but `issued()` and
    // the issued-ticket record it read are both deleted with the ledger, so there is no seam left
    // to pin: the serve path now resolves by the ticket's own fields and reads no app state that
    // standing could influence. The plane-level replacement pin (any re-introduced gate on the
    // serve path reds the fetch) is `transport.rs`'s
    // `a_blocked_redeemer_with_a_valid_ticket_is_still_served`, whose MUTATION names inserting a
    // refusal after `authorize_redemption` in `serve_manifest_stream`.


    // `no_issued_ticket_record_is_ever_evicted` — DELETED 2026-09-03, QURATOR-177 Option E: the
    // issued-ticket map itself is deleted (it existed to answer "is this ticket spent?" and to
    // audit), so its eviction policy is moot. The ruling explicitly gives up the audit trail.



    /// A re-serve-shaped ticket: same shape `send_cached_manifest` mints (QURATOR-177 Option E
    /// deleted the issued-ticket record these fixtures used to plant; the ticket's OWN
    /// `author_npub` is the branch discriminator now, so the fixture IS the ticket).
    fn a_re_serve_ticket(author_npub: Option<String>) -> TransportTicket {
        let mut ticket = TransportTicket::issue("req-x", "roms", "addr", 1_700_000_000, None);
        ticket.author_npub = author_npub;
        ticket
    }

    /// Required test 1 — an owner-issued ticket (`author_npub: None`) never falls through to the
    /// cache.
    ///
    /// ⚠ **EXPECTATION DELIBERATELY CHANGED by QURATOR-177 slice 1 (owner ruling 2026-09-03).**
    /// Until this slice the dangerous shape was `author_npub` PRESENT with `served_fingerprint`
    /// ABSENT (a record written before the field existed): the branch required BOTH fields, so
    /// that record took the owner-issued path, and this test asserted a "No draft found" error for
    /// exactly that shape. The ruling makes `author_npub` ALONE the Carrier-4 signal — a
    /// pre-field record with it set WAS a Carrier-4 mint (`send_cached_manifest` is the only
    /// writer of `author_npub`), so it now belongs on the cache branch and is pinned there by
    /// `a_pre_field_re_serve_record_is_resolved_from_the_cache` below. The owner-issued shape
    /// THIS test pins is `author_npub: None`.
    ///
    /// MUTATION (reds this test): in `StoreManifestSource::payload`
    /// (crates/hb-app/src/manifest_source.rs, the `impl ManifestSource for StoreManifestSource`
    /// block), weaken the author guard `if let Some(author_npub) = &ticket.author_npub` to
    /// engage on every ticket — e.g. change the binding to
    /// `ticket.author_npub.as_deref().or(Some(""))`. The ticket below has `author_npub: None`
    /// and no draft for the slug, so the mutant takes the cache branch, the lookup misses, and the
    /// error becomes "…no longer in the manifest cache" instead of "No draft found" — the assert
    /// on the message reds. (The inverse mutation — any second condition on the guard that this
    /// ticket cannot satisfy — reds `a_pre_field_re_serve_record_is_resolved_from_the_cache`
    /// instead, which is this slice's load-bearing pin.)
    #[test]
    fn an_owner_issued_request_id_never_falls_through_to_the_cache() {
        let (_dir, store) = store();
        let source = StoreManifestSource::new(
            store.clone(),
            Identity::generate(),
            SessionBrowseKey::new([7u8; 32]),
        );
        // Scene-setting: the cache DOES hold some author's «roms» and there is no draft for the
        // slug — the historical trap. The discriminator is the error message: only the owner
        // branch can produce "No draft found" here, and only a cache HIT (not this fixture's
        // stray entry) could produce an `Ok`.
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
        // `author_npub: None` — the OWNER-ISSUED ticket (QURATOR-177 slice 1 made the author
        // field alone the branch condition; `served_fingerprint` no longer decides anything
        // here). It names the same slug the cache happens to hold a copy of; the cache branch
        // would silently serve that entry under a ticket that never carried Carrier-4 provenance.
        let err = source.payload(&a_re_serve_ticket(None)).unwrap_err().to_string();
        assert!(
            err.contains("No draft found"),
            "an owner-issued record must take the build_slug_manifest branch, got: {err}"
        );
    }

    /// QURATOR-177 slice 1 — the LOAD-BEARING pin. A re-serve whose ticket names
    /// `(author_npub, slug)` resolves **the currently cached entry for that key**. (Until
    /// QURATOR-177 Option E the fingerprint to compare against came from the issued-ticket
    /// record; the record is gone, and the ticket never carried a fingerprint — the newest cached
    /// copy IS the resolution, with nothing to pin against.) The cache holds one entry per
    /// `(npub, slug)` and a refetch `put` replaces it in place, so serve time wants what is
    /// cached NOW. Under the pre-slice code (fingerprint-exact `get`) this fixture was a cache
    /// miss and the whole call errored.
    ///
    /// MUTATION (reds this test): in `StoreManifestSource::payload`
    /// (crates/hb-app/src/manifest_source.rs), change the resolve call
    /// `manifest_cache::get_latest(&self.store.manifest_cache_dir(), author_npub, &ticket.slug,
    /// now_secs())` back to a fingerprint-pinned lookup — e.g. refuse unless the cache holds an
    /// entry whose fingerprint is present in some map the ticket cannot supply (or simply return
    /// `Err` when the newest entry's fingerprint does not match a hardcoded "fp-at-mint"). Any
    /// pinned-fingerprint arm misses on this fixture — the cache holds `fp-new` and no fingerprint
    /// ever accompanied the ticket — so the call returns `Err` and `unwrap()` panics.
    #[test]
    fn a_re_serve_serves_the_currently_cached_snapshot_not_the_minted_one() {
        let (_dir, store) = store();
        let source = StoreManifestSource::new(
            store.clone(),
            Identity::generate(),
            SessionBrowseKey::new([7u8; 32]),
        );
        // The refetch-shaped state: the cache was updated (fp-new) — the newest cached copy for
        // `(author_npub, "roms")` is the fp-new envelope, and that is what must be served.
        let author = Identity::generate();
        let author_npub = npub_of(&author);
        let env = hb_core::build_manifest_envelope(
            &author,
            "roms",
            &[9u8; 32],
            "fp-new",
            1_700_000_000,
            &[r#"{"slug":"roms","entries":[]}"#.to_string()],
        )
        .unwrap();
        crate::manifest_cache::put(
            &store.manifest_cache_dir(),
            &author_npub,
            "roms",
            "fp-new",
            &env.to_json().unwrap(),
            1_700_000_000,
            crate::manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .unwrap();
        let payload = source.payload(&a_re_serve_ticket(Some(author_npub))).unwrap();
        // The served bytes are the fp-new envelope, sealed — not the (now absent) fp-at-mint one,
        // and not any same-slug owner build.
        let served = String::from_utf8(payload.as_bytes().to_vec()).unwrap();
        assert!(
            served.contains("fp-new"),
            "the re-serve must serve the currently cached snapshot, got: {served}"
        );
    }

    /// QURATOR-177 slice 1 — `author_npub` alone is the Carrier-4 signal; Option E deletes the
    /// issued-ticket record, so the ticket's own `author_npub` is the ONLY thing `payload` can
    /// branch on. A ticket with the author set takes the CACHE branch, full stop.
    ///
    /// MUTATION (reds this test): in `StoreManifestSource::payload`
    /// (crates/hb-app/src/manifest_source.rs), weaken the guard so this ticket escapes the cache
    /// branch — e.g. add `&& author_npub.len() > 40` to the `if let`, or restore any second
    /// condition the ticket cannot satisfy. The fixture has no draft for the slug, so the mutant
    /// falls to the owner branch, `build_slug_manifest` errors "No draft found", and `unwrap()`
    /// panics.
    #[test]
    fn a_pre_field_re_serve_record_is_resolved_from_the_cache() {
        let (_dir, store) = store();
        let source = StoreManifestSource::new(
            store.clone(),
            Identity::generate(),
            SessionBrowseKey::new([7u8; 32]),
        );
        let author = Identity::generate();
        let author_npub = npub_of(&author);
        let env = hb_core::build_manifest_envelope(
            &author,
            "roms",
            &[9u8; 32],
            "fp-prefield",
            1_700_000_000,
            &[r#"{"slug":"roms","entries":[]}"#.to_string()],
        )
        .unwrap();
        crate::manifest_cache::put(
            &store.manifest_cache_dir(),
            &author_npub,
            "roms",
            "fp-prefield",
            &env.to_json().unwrap(),
            1_700_000_000,
            crate::manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .unwrap();
        // No draft for the slug either, so the owner branch would error; only the cache branch
        // can satisfy this call.
        let payload = source.payload(&a_re_serve_ticket(Some(author_npub))).unwrap();
        let served = String::from_utf8(payload.as_bytes().to_vec()).unwrap();
        assert!(
            served.contains("fp-prefield"),
            "a pre-field re-serve record must resolve from the cache on the author alone, got: {served}"
        );
    }

    /// Required test 2 — the LOAD-BEARING one. A re-serve whose `(author_npub, slug)` has NO cache
    /// entry must ERROR, never fall through to `build_slug_manifest` or any other source. A
    /// "helpful" fallback would silently serve C's own same-slug collection as if it were A's.
    ///
    /// MUTATION (reds this test): in `StoreManifestSource::payload` (same containing impl block),
    /// turn the cache-miss into a FALLTHROUGH — nest the serve inside the hit so a miss skips it
    /// and execution reaches the `build_slug_manifest` call below the `if let`:
    /// change `let json = manifest_cache::get_latest(...).ok_or_else(|| ...)?` to
    /// `if let Some(json) = manifest_cache::get_latest(...) { ...return the sealed payload...; }` —
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
        // The trap: C DOES own a same-slug collection (the draft exists), and the ticket carries
        // an author whose `(author, "roms")` resolves to NO cache entry (the cache is empty for
        // that key entirely — there is no fingerprint pin to miss, so the fixture plants nothing
        // at all). Serving C's own «roms» here is exactly the
        // cross-tenant collision the fence exists to prevent.
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
        let err = source
            .payload(&a_re_serve_ticket(Some("npub1author".into())))
            .unwrap_err()
            .to_string();
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
