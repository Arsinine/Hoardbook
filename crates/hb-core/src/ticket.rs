//! The **transport ticket** (M18 W1) — the authorization to connect and fetch one manifest.
//!
//! A ticket is what the hoarder hands back when they approve a manifest request: an opaque node
//! address plus the binding that says *which request* it answers. It rides a **NIP-17 DM to one
//! recipient** — the DM provides the seal, so this module deliberately adds no crypto of its own.
//!
//! **It does NOT ride the presence event.** The retired plane sealed addresses into the public
//! presence event (`seal_addrs`/`SealedAddr`), which is the H4/MT2 IP-harvest hole; presence stays
//! freshness-only and `binding::presence_carries_no_address_or_node_key` stays green. A ticket goes
//! to exactly one asker, for exactly one request.
//!
//! **What a ticket grants, precisely.** The ability to *connect and fetch* — **not** the ability to
//! read. The manifest stays browse-key encrypted, so a ticket-holder without the share code gets
//! ciphertext. This is the distinction `SEMANTICS.md` reserves: a **share code** (`hbk1…`,
//! permanent) grants *read*; a **transport ticket** (until redeemed) grants *connect and fetch*.
//! (A third concept, the Mascara download ticket, was retired 2026-07-26 and is *dead* — deleted
//! from the vocabulary, not merely unused.)
//!
//! ## Lifecycle — owner rulings 2026-07-30 and 2026-08-31. Do not re-open the time-box option.
//!
//! > **Minted per fetch. Consumed on SUCCESS, not on attempt. Redeemed immediately. Not time-boxed
//! > — valid until redeemed. Backed by a standing grant per (peer, collection).**
//!
//! The 2026-08-31 amendment (QURATOR-137): the authorization a ticket exercises is a **standing
//! grant per (peer, collection)** — a peer already granted needs no new ask, so "one ticket per
//! request" in its original one-human-ask sense is dead. What survives unchanged is the mint
//! discipline: every fetch still mints its own ticket, still consumed on delivery. **There is
//! still no time-box, and no standing re-read at redeem** — the 2026-07-30 "live standing at
//! every redeem" clause was withdrawn by owner ruling 2026-09-03 (see below).
//!
//! Each property answers a failure the alternatives cause, and each is enforced here rather than
//! documented:
//!
//! 1. **No replay bookkeeping at all** (owner ruling 2026-09-03, QURATOR-177 Option E). Until that
//!    ruling a ticket was "consumed on success" via `RedemptionGrant`/`ConsumedTicket` and a
//!    durable spent bit on the issuer; the issued-ticket ledger that held it is DELETED, and with
//!    it `RedemptionGrant`, `ConsumedTicket`, `into_consumed`, `matches_issued` and
//!    `HbError::TicketAlreadyRedeemed`. Authorization happens at ASK time (the auto-approve path
//!    consults the standing grant against the asker's npub, established by the NIP-17 seal); the
//!    ticket is reduced to address delivery, so the serve path performs **no per-request
//!    authorization lookup at all**. Deliberately given up: cross-restart replay protection and the
//!    audit trail. **Do not re-introduce a spent bit, a replay set, or any per-request serve-side
//!    authorization check — the absence is the ruling, not an oversight.** What actually prevents
//!    repeat traffic is the TRIGGER condition on the asker (a refetch fires only when the
//!    collection's fingerprint changed) plus the asker-side ask-trace spend gate in hb-app — never
//!    a serve-side refusal.
//! 2. **Redeemed immediately, with no affordance to defer.** *"There is no way of strategically
//!    keeping a ticket to cash in later"* (owner) — which is what makes property 3 safe. This is an
//!    **implementation constraint, not a nicety: do not add a "redeem later" button.**
//! 3. **Not time-boxed.** Deliberately **no `expires_at` field** — see
//!    [`a_ticket_has_no_expiry_by_design`]. A window would silently discard an approval *both
//!    humans already gave*, whenever the asker happens to be offline when the owner clicks. The
//!    owner approves; the asker's client redeems whenever it next comes online. Time-boxing
//!    optimizes the wrong failure.
//!
//! **Redeem-time contact standing is WITHDRAWN** (owner ruling 2026-09-03, QURATOR-177: *"Blocks
//! should only block interaction i.e. chats, it should not meaningfully affect other traffic."*).
//! From 2026-07-30 until that ruling [`validate_redemption`] re-read the redeemer's live standing
//! and refused Blocked/Declined/Unknown, described then as "revocability" — a misnomer the ruling
//! retired: blocking was never read-access revocation (a mutual contact may re-serve its cached
//! copy, Carrier 4; a public browse key is a forwardable string), and it now gates chat/DM
//! interaction only. The check, its `ContactStanding` vocabulary, and its error variant are
//! deleted. Do not re-introduce a standing input to redemption.
//!
//! **Reuse IS offered — as a standing grant, owner ruling 2026-08-31 (QURATOR-137).** The premise
//! this file used to argue ("no scenario needs repeated fetches") is overruled: a peer already
//! granted for a collection skips the ask entirely, and each fetch still mints a fresh ticket. The
//! old re-ask path (`snapshot_fingerprint` + the stale flag → new request, new ticket) is no longer
//! the *required* route for a granted peer, but nothing here forbids it either — `request_id` stays
//! per-ask, and the asker's `ask_nonce` gate (fail-closed on `None`) still binds each ticket to one
//! specific ask, so a standing grant never becomes a licence to make our client dial an address of
//! someone's choosing unprompted. The one-per-request framing now describes mint discipline, not
//! a limit on how often a granted peer may fetch. What gates a re-serve is hb-app's ask-trace
//! spend gate and the standing-grant map, not anything in this module (the issued-ticket records
//! `load_issued_ticket`/`consumed_at` were deleted by the same 2026-09-03 ruling).

use serde::{Deserialize, Serialize};

use crate::error::HbError;

/// Ticket schema version. Pinned in `wire_freeze`: a ticket rides a durable NIP-17 DM, so one
/// already sitting in a relay-stored wrap must stay readable.
pub const TICKET_V: u8 = 1;

/// The `content.hb` discriminator marking a DM body as a transport ticket — the owner→asker
/// direction, distinct from `manifest_request`'s asker→owner. Frozen for the same reason.
pub const TICKET_TAG: &str = "transport_ticket";

// `ContactStanding` (Good/Blocked/Declined/Unknown) was deleted 2026-09-03, QURATOR-177: its only
// consumer was `validate_redemption`'s redeem-time standing arm, withdrawn by the owner ruling
// that blocking gates chat/DM interaction only. Chat's acceptance gate reads `dm_blocked`
// directly (`commands/chat.rs`, `route_dm`) and never used this vocabulary.

/// A transport ticket: the address to dial plus the binding that says which request it answers.
///
/// **Note what is absent: there is no expiry field.** That is the owner's ruling made structural —
/// a ticket cannot be time-boxed by a later well-meaning "consistency" change without changing this
/// type, which [`a_ticket_has_no_expiry_by_design`] will notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportTicket {
    /// Always [`TICKET_TAG`] — how the asker's inbox recognises the DM.
    pub hb: String,
    pub ticket_v: u8,
    /// The request this ticket answers. **One ticket per ask** (mint discipline, not a fetch limit
    /// — the standing grant behind it is per (peer, collection), owner ruling 2026-08-31): the
    /// binding that stops a ticket issued for one request being spent on another. A granted peer
    /// re-fetching gets a *newly minted* ticket, not a replay of this one.
    pub request_id: String,
    /// The collection the request was about, so a redeemer cannot silently fetch a different one.
    pub slug: String,
    /// The issuer's dialable node address, **opaque to hb-core** — this crate has no iroh
    /// dependency and deliberately does not parse it. The transport layer (hb-app) interprets it.
    pub node_addr: String,
    /// Unix seconds the ticket was issued. Provenance and display only — **not an expiry input.**
    pub issued_at: u64,
    /// **The asker's own nonce, echoed back** (M18 post-review ruling ①, owner 2026-07-31).
    ///
    /// `request_id` is minted by the OWNER, so it proves nothing to the asker: a peer can mint one
    /// freely. Without an asker-generated value the redeem-side gate could only ask "did I ever ask
    /// this peer for this collection?", which is a **standing, reusable authorization** — the peer
    /// could make our client dial an address of their choosing, unprompted, forever. Echoing the
    /// nonce binds the ticket to **one specific ask**.
    ///
    /// `Option` for wire compatibility only: a ticket minted before this field existed deserializes
    /// with `None`, and the asker's gate **refuses to auto-dial on `None`** (fail closed — the
    /// alternative re-opens the hole to anyone who claims to be an old client).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_nonce: Option<String>,
    /// `None` means "the issuer's own collection" — the ordinary, pre-existing case: this node is
    /// serving what it holds. `Some(npub)` means this ticket is a re-serve of that author's cached
    /// manifest (carrier 4): the issuer is not the author, and the redeemer must pin the manifest to
    /// `npub`, not to whoever dialed the address.
    ///
    /// **Additive and optional, `TICKET_V` unchanged** — same wire-compat shape as `ask_nonce`
    /// (owner ruling, QURATOR-79, 2026-08-30): `ticket.rs`'s version gate refuses any ticket whose
    /// `ticket_v` exceeds the build's own, so bumping it would make every old build reject *every*
    /// ticket from a new one, including ordinary manifest sends with no carrier-4 involvement.
    ///
    /// **Unlike `ask_nonce`, this field needs no fail-closed gate on `None`.** An optional field is
    /// only safe when its absence cannot be exploited as a downgrade. Strip this field and the
    /// issuer serves its *own* collection — which it is entitled to serve, and which the redeemer's
    /// author pin (when one is expected) refuses on mismatch. There is no weaker behaviour hiding
    /// behind `None` to downgrade into, so absence carries no capability an attacker could induce a
    /// peer into granting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_npub: Option<String>,
}

impl TransportTicket {
    /// Mint a ticket for one request. `node_addr` is passed through untouched.
    pub fn issue(
        request_id: &str,
        slug: &str,
        node_addr: &str,
        issued_at: u64,
        ask_nonce: Option<&str>,
    ) -> Self {
        Self {
            hb: TICKET_TAG.to_string(),
            ticket_v: TICKET_V,
            request_id: request_id.to_string(),
            slug: slug.to_string(),
            node_addr: node_addr.to_string(),
            issued_at,
            // Empty is normalized to absent so "present but blank" cannot masquerade as a real
            // nonce at the gate.
            ask_nonce: ask_nonce.filter(|n| !n.is_empty()).map(str::to_string),
            // `issue` mints the ordinary "own collection" ticket; carrier-4 re-serve tickets are a
            // separate slice and not constructed here.
            author_npub: None,
        }
    }

    /// Structural self-check: the discriminator and version are recognised and the bindings are
    /// present. An unknown version is *recognised and refused*, never mis-read — the same
    /// forward-compat contract `MANIFEST_V` upholds.
    pub fn verify_shape(&self) -> Result<(), HbError> {
        if self.hb != TICKET_TAG {
            return Err(HbError::InvalidTicket("not a transport ticket".into()));
        }
        if self.ticket_v == 0 || self.ticket_v > TICKET_V {
            return Err(HbError::UnsupportedVersion(self.ticket_v));
        }
        if self.request_id.is_empty() || self.slug.is_empty() || self.node_addr.is_empty() {
            return Err(HbError::InvalidTicket("ticket is missing a required binding".into()));
        }
        // Present-but-blank is a malformed ticket, not an old one. Absent is the only legal way to
        // say "no nonce", so the asker's gate can treat `None` as one unambiguous case.
        if self.ask_nonce.as_deref().is_some_and(str::is_empty) {
            return Err(HbError::InvalidTicket("ticket carries an empty ask nonce".into()));
        }
        // Same shape, same reasoning: absent is the only legal way to say "the issuer's own
        // collection", so a blank must not be readable as that case. Production mints this field
        // only through `npub_of(parse_recipient(..))`, which never yields `""` — so a blank can
        // only arrive from the wire, and a serde derive is an unvalidated public constructor.
        if self.author_npub.as_deref().is_some_and(str::is_empty) {
            return Err(HbError::InvalidTicket("ticket carries an empty author npub".into()));
        }
        Ok(())
    }

}

// `matches_issued` — DELETED 2026-09-03, QURATOR-177 Option E (owner ruling: authorization is
// at ASK time via the standing grant; the ticket is address delivery). Its only caller was the
// serve gate that compared the presented ticket against the ledger's stored copy — that gate and
// the ledger behind it are gone with the same ruling. The `node_addr`-exclusion lesson lives on
// in the type's doc comments above.

// `RedemptionGrant`, `RedemptionGrant::into_consumed`, `ConsumedTicket` and its accessors —
// DELETED 2026-09-03, QURATOR-177 Option E (owner ruling 2026-09-03). Their entire purpose was
// one-ticket-one-delivery replay bookkeeping: the issuer's node spent the ticket on the asker's
// ACK and refused a later replay. The ruling deletes that machinery — the issued-ticket ledger
// that durably held the spent bit is gone, and with it the need for a compile-time "consumed on
// success" type story. Do not re-introduce a spent bit, a receipt, or any per-request
// authorization state on the serve path: the absence is the ruling, not an oversight. What
// prevents repeat traffic is the asker-side TRIGGER condition (a refetch fires only on a
// fingerprint change) and the ask-trace spend gate in hb-app.

// The redeem-time gate, run by the **issuer's** node as it accepts a connection.
///
/// Two checks, in this order, each of which a real failure mode motivates:
///
/// 1. the ticket is structurally a ticket of a version we speak;
/// 2. it answers **this** request (one ticket per ask — no spending A's ticket on B).
///
/// Deliberately absent: any expiry comparison. There is no clock input, so this function
/// *cannot* time-box a ticket even by accident.
///
/// Also deliberately absent: any contact-standing check (owner ruling 2026-09-03, QURATOR-177 —
/// blocking gates chat/DM interaction only, and was never read-access revocation). Do not re-add
/// a standing parameter here.
///
/// And deliberately absent: the **already-consumed replay check** (owner ruling 2026-09-03,
/// QURATOR-177 Option E). Until that ruling this function took an `already_consumed: bool` and
/// refused a replay with `HbError::TicketAlreadyRedeemed`; the durable spent bit that fed it lived
/// in hb-app's issued-ticket ledger, which the same ruling deletes. Its removal is a ruling, not
/// an oversight: cross-restart replay protection and the audit trail were deliberately given up,
/// and repeat traffic is prevented by the asker-side trigger condition, never by a serve-side
/// refusal. Do not re-add a consumed/spent/replay input here.
///
/// ⚠ **This function performs NO AUTHORIZATION, and its old name (`validate_redemption`) said
/// otherwise for months.** It checks two things, both pure validation: the ticket is well-formed,
/// and it is the ticket for THIS request. There is no permission lookup, because there is nothing
/// to look up — QURATOR-164 (2026-09-04) deleted approvals entirely on the grounds that public
/// collections need none, and this doc previously claimed "authorization belongs at ASK time (the
/// standing grant)", which became false the moment the grant map was deleted. CLAUDE.md records
/// that this misnomer already misled a whole design pass; the rename is the fix.
pub fn validate_redemption(
    ticket: &TransportTicket,
    for_request_id: &str,
) -> Result<(), HbError> {
    ticket.verify_shape()?;
    if ticket.request_id != for_request_id {
        return Err(HbError::InvalidTicket("ticket was issued for a different request".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket() -> TransportTicket {
        TransportTicket::issue("req-1", "my-slug", "node-addr-opaque", 1_700_000_000, Some("nonce-1"))
    }


    #[test]
    fn a_valid_ticket_for_this_request_is_authorized() {
        validate_redemption(&ticket(), "req-1").unwrap();
    }

    /// **The nonce is what makes an approval answer ONE ask** (owner ruling ① 2026-07-31). It
    /// survives the DM body verbatim, because the asker compares it against what it stored locally.
    #[test]
    fn the_ask_nonce_round_trips_through_a_dm_body() {
        let t = ticket();
        assert_eq!(t.ask_nonce.as_deref(), Some("nonce-1"));
        let back: TransportTicket = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back.ask_nonce.as_deref(), Some("nonce-1"), "the asker must see its own nonce");
        back.verify_shape().unwrap();
    }

    /// A pre-ruling ticket has no nonce and must still PARSE — `TICKET_V` did not bump, so an
    /// already-sent ticket sitting in a relay-stored wrap stays readable. Safety comes from the
    /// asker refusing to auto-dial on `None`, not from the wire rejecting it.
    #[test]
    fn a_ticket_without_a_nonce_still_parses_and_is_shape_valid() {
        let json = r#"{"hb":"transport_ticket","ticket_v":1,"request_id":"r","slug":"s",
                       "node_addr":"a","issued_at":1}"#;
        let t: TransportTicket = serde_json::from_str(json).expect("a v1 ticket still parses");
        assert_eq!(t.ask_nonce, None);
        t.verify_shape().expect("absent is legal; the redeem-side gate is what refuses it");
    }

    /// **Present-but-blank is malformed, not "old".** Collapsing the two would let a peer send
    /// `"ask_nonce":""` and have it treated as the compatibility case, which is the hole the ruling
    /// closes. `issue` normalizes an empty nonce to absent so this can only arrive from the wire.
    #[test]
    fn an_empty_ask_nonce_is_refused_rather_than_read_as_absent() {
        let json = r#"{"hb":"transport_ticket","ticket_v":1,"request_id":"r","slug":"s",
                       "node_addr":"a","issued_at":1,"ask_nonce":""}"#;
        let t: TransportTicket = serde_json::from_str(json).unwrap();
        assert!(t.verify_shape().is_err(), "an empty nonce is a malformed ticket");

        assert_eq!(
            TransportTicket::issue("r", "s", "a", 1, Some("")).ask_nonce,
            None,
            "issue normalizes empty to absent, so we never mint the ambiguous shape"
        );
    }

    /// **Field present** — a carrier-4 re-serve ticket carries `author_npub` and it survives the DM
    /// body verbatim (QURATOR-79, additive/optional, `TICKET_V` unchanged).
    #[test]
    fn an_author_npub_round_trips_through_a_dm_body() {
        let mut t = ticket();
        t.author_npub = Some("npub1author".to_string());
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"author_npub\":\"npub1author\""), "the field name is the contract");
        let back: TransportTicket = serde_json::from_str(&json).unwrap();
        assert_eq!(back.author_npub.as_deref(), Some("npub1author"));
        back.verify_shape().unwrap();
    }

    /// **Field absent** — an ordinary "own collection" ticket, minted before this field existed or
    /// simply by an issuer serving its own collection, still parses with `author_npub: None`. This is
    /// the case the owner's ruling turned on: absence is not a downgrade, because `None` means the
    /// issuer serves its own collection, which it is entitled to do.
    #[test]
    fn a_ticket_without_an_author_npub_still_parses_and_is_shape_valid() {
        let json = r#"{"hb":"transport_ticket","ticket_v":1,"request_id":"r","slug":"s",
                       "node_addr":"a","issued_at":1}"#;
        let t: TransportTicket = serde_json::from_str(json).expect("a ticket without the field still parses");
        assert_eq!(t.author_npub, None);
        t.verify_shape().expect("absence is the ordinary own-collection case, not an error");
    }

    /// **Present-but-blank is malformed, not "own collection"** — the `ask_nonce` shape exactly
    /// (QURATOR-172 #4). Collapsing the two would let a peer send `"author_npub":""` and have the
    /// re-serve signal read as the ordinary own-collection case. The blank is built by
    /// deserializing wire JSON, NOT via `issue`/`npub_of`: production mints this field only through
    /// `npub_of(parse_recipient(..))` (fulfil.rs), which never yields `""`, so deserializing
    /// attacker-supplied bytes is the only route a blank can take — and `verify_shape` is the gate
    /// that meets it. Together with the two tests above this pins all three shapes: absent (pass),
    /// present-and-valid (pass), present-and-blank (reject).
    ///
    /// MUTATION (P-10, applied by the orchestrator — this lane compiles nothing): in
    /// `verify_shape`, delete the three code lines of the `if self.author_npub.as_deref()
    /// .is_some_and(str::is_empty)` block (the `if` line, the `return Err(HbError::InvalidTicket(
    /// "ticket carries an empty author npub".into()));` arm, and its closing brace) — this test
    /// reds on the `.is_err()` assertion while still compiling.
    #[test]
    fn an_empty_author_npub_is_refused_rather_than_read_as_absent() {
        let json = r#"{"hb":"transport_ticket","ticket_v":1,"request_id":"r","slug":"s",
                       "node_addr":"a","issued_at":1,"author_npub":""}"#;
        let t: TransportTicket = serde_json::from_str(json).unwrap();
        assert!(t.verify_shape().is_err(), "an empty author npub is a malformed ticket");
        assert!(
            matches!(t.verify_shape(), Err(HbError::InvalidTicket(_))),
            "the refusal names the ticket malformed, not a version problem"
        );
    }

    #[test]
    fn ticket_round_trips_through_a_dm_body() {
        let t = ticket();
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(TICKET_TAG), "the DM body carries its discriminator");
        let back: TransportTicket = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        back.verify_shape().unwrap();
    }

    // DELETED 2026-09-03, QURATOR-177 Option E (owner ruling: authorization is at ASK time via the
    // standing grant; the ticket is address delivery): `a_failed_attempt_does_not_consume_the_
    // ticket_and_a_retry_succeeds` pinned the withdrawn one-ticket-one-delivery consume semantics —
    // drop a `RedemptionGrant` without `into_consumed`, retry, then assert the successful delivery
    // consumes and a replay is refused with `TicketAlreadyRedeemed`. The grant, the receipt, the
    // spent bit and the error variant are all gone with the ruling, so "a failed connection costs
    // nothing" now holds trivially (nothing is ever recorded to cost anything) and the replay half
    // pinned withdrawn behaviour. Deliberately given up: durable replay protection.

    /// **Property 3 — not time-boxed.** An approval issued while the asker was offline is still
    /// redeemable when they return, however long that took.
    #[test]
    fn an_approval_given_while_the_asker_was_offline_is_still_redeemable() {
        // Issued a year ago. There is no clock argument to `validate_redemption` at all, so this
        // cannot depend on elapsed time — which is the point.
        let ancient = TransportTicket::issue("req-1", "my-slug", "node-addr-opaque", 1, None);
        validate_redemption(&ancient, "req-1")
            .expect("a ticket is valid until redeemed, never expired");
    }

    /// **Property 3, structurally.** The absence of an expiry field is a ruling, not an oversight —
    /// a later "consistency" change that time-boxes tickets must trip this, not slip through. The
    /// serialized body is the durable artifact, so that is what is checked.
    #[test]
    fn a_ticket_has_no_expiry_by_design() {
        let json = serde_json::to_string(&ticket()).unwrap();
        for forbidden in ["expires", "expiry", "ttl", "valid_until", "not_after"] {
            assert!(
                !json.contains(forbidden),
                "a transport ticket must not carry `{forbidden}` — it is valid until redeemed \
                 (owner ruling 2026-07-30: a time box discards approvals both humans already gave)"
            );
        }
    }

    // DELETED 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only block interaction i.e.
    // chats, it should not meaningfully affect other traffic."*):
    // `a_redeemer_blocked_or_declined_after_approval_is_refused` pinned the withdrawn redeem-time
    // standing refusal — Blocked / Declined / Unknown refused with `TicketRedeemerNotInGoodStanding`,
    // restore-to-Good re-admitted. The standing parameter, the arm, the `ContactStanding` type, and
    // the error variant are all gone, so a blocked redeemer holding a valid unspent ticket now
    // redeems. Blocking still gates chat/DM (pinned in hb-app's `commands/chat.rs`, e.g.
    // `proactive_block_refuses_later_dms_and_unblock_restores_acceptance`).

    // DELETED 2026-09-03, QURATOR-177 Option E (owner ruling): `a_standing_grant_re_mints_rather_
    // than_replays` pinned the withdrawn spent-ticket bookkeeping — consume ticket req-1 via
    // `into_consumed`, assert a replay of it is refused with `TicketAlreadyRedeemed`, then assert a
    // fresh mint authorizes. The consume/refuse half exercised machinery the ruling deletes (the
    // grant, the receipt, the durable spent bit); the fresh-mint half is now trivially true for
    // EVERY ticket, granted or not, because nothing is ever recorded. The asker-side ask-trace
    // spend gate (`AskClaim::Spent`) is what one-ask-one-fetch means now, pinned in hb-app's
    // `manifest_source.rs`.

    /// **One ticket per ask** — a ticket cannot be spent on a different request or collection.
    /// UNCHANGED by the 2026-08-31 standing-grant ruling: that ruling changes WHO is entitled to a
    /// ticket (a granted peer, without re-asking), not WHAT a ticket is bound to. `request_id` +
    /// `ask_nonce` remain the anti-forgery binding (a peer must not make our client dial an address
    /// of their choosing "for" a request we never made), so this stays exactly as pinned.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): in `validate_redemption`
    /// (crates/hb-core/src/ticket.rs), delete the
    /// `if ticket.request_id != for_request_id { return Err(HbError::InvalidTicket(...)); }` arm —
    /// this test reds on the `matches!(Err(HbError::InvalidTicket(_)))` assertion while still
    /// compiling.
    #[test]
    fn a_ticket_is_bound_to_its_own_request() {
        let t = ticket();
        assert!(
            matches!(
                validate_redemption(&t, "req-2"),
                Err(HbError::InvalidTicket(_))
            ),
            "a ticket issued for req-1 must not redeem req-2"
        );
    }

    #[test]
    fn malformed_and_unknown_version_tickets_are_refused() {
        let mut wrong_tag = ticket();
        wrong_tag.hb = "manifest_request".into();
        assert!(
            matches!(wrong_tag.verify_shape(), Err(HbError::InvalidTicket(_))),
            "another DM body type is not a ticket"
        );

        let mut future = ticket();
        future.ticket_v = TICKET_V + 1;
        assert!(
            matches!(future.verify_shape(), Err(HbError::UnsupportedVersion(v)) if v == TICKET_V + 1),
            "an unknown version is recognised and refused, never mis-read"
        );

        for blank in ["request_id", "slug", "node_addr"] {
            let mut t = ticket();
            match blank {
                "request_id" => t.request_id.clear(),
                "slug" => t.slug.clear(),
                _ => t.node_addr.clear(),
            }
            assert!(
                matches!(t.verify_shape(), Err(HbError::InvalidTicket(_))),
                "a ticket with an empty {blank} is refused"
            );
        }
    }

    // DELETED 2026-09-03, QURATOR-177 Option E (owner ruling: authorization is at ASK time via the
    // standing grant; the ticket is address delivery):
    // `a_redeemer_that_sanitized_the_node_addr_still_matches_the_issued_ticket` and
    // `matches_issued_still_refuses_every_capability_field_mismatch` pinned `matches_issued` — the
    // serve-path comparison of the presented ticket against the ledger's stored copy, deliberately
    // excluding `node_addr` (the devtest 2026-08-27 `==` defect). That comparison's only caller was
    // the serve gate `serve_manifest_stream` ran after `source.issued(...)`; the gate and the
    // ledger behind it are deleted by the same ruling, so there is no stored copy left to compare
    // against. The `node_addr`-rewriting requirement itself survives: `sanitize_node_addr` still
    // runs asker-side (QURATOR-113 #20), and `a_redeemer_that_sanitized_the_node_addr_is_still_
    // served` in hb-app's transport.rs pins that a sanitized ticket is served over real QUIC.
}
