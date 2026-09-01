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
//! discipline: every fetch still mints its own ticket, still consumed on delivery, with live
//! contact standing re-read at **every** redeem — removing or blocking the contact refuses the
//! next fetch at this node's own gate. **There is still no time-box.**
//!
//! Each property answers a failure the alternatives cause, and each is enforced here rather than
//! documented:
//!
//! 1. **Consumed on success, not attempt.** Hole-punching succeeds ~90% of the time and the
//!    `drain_connection` teardown truncation is a known recurring bug, so a strictly one-attempt
//!    ticket would put a **human back in the loop after a dropped connection** — for a transfer
//!    both humans already agreed to. That is the launch embarrassment relocated, not fixed. **A
//!    failed connection must cost nothing.** Enforced by [`RedemptionGrant`]: the only way to reach
//!    a [`ConsumedTicket`] is [`RedemptionGrant::into_consumed`], which **requires the delivered
//!    payload as proof**. There is no code path that consumes a ticket without the goods in hand,
//!    and dropping a grant does nothing.
//! 2. **Redeemed immediately, with no affordance to defer.** *"There is no way of strategically
//!    keeping a ticket to cash in later"* (owner) — which is what makes property 3 safe. This is an
//!    **implementation constraint, not a nicety: do not add a "redeem later" button.**
//! 3. **Not time-boxed.** Deliberately **no `expires_at` field** — see
//!    [`a_ticket_has_no_expiry_by_design`]. A window would silently discard an approval *both
//!    humans already gave*, whenever the asker happens to be offline when the owner clicks. The
//!    owner approves; the asker's client redeems whenever it next comes online. Time-boxing
//!    optimizes the wrong failure.
//!
//! **Redeem-time contact standing is REQUIRED** (owner ruling 2026-07-30). Valid-until-redeemed
//! means the ticket alone cannot be the whole authorization: an asker later blocked or declined
//! must not be able to redeem an older ticket. So [`authorize_redemption`] checks **live standing
//! at redeem time**, not just ticket validity. It costs nothing — the owner's node is the one
//! accepting the connection — and it restores revocability without touching the ticket design.
//! Property 2 makes this defence-in-depth (an adversarial asker deliberately extracting a ticket
//! from its DM and withholding redemption) rather than routine, but it is the whole difference
//! between "cannot un-approve" and "can".
//!
//! **Reuse IS offered — as a standing grant, owner ruling 2026-08-31 (QURATOR-137).** The premise
//! this file used to argue ("no scenario needs repeated fetches") is overruled: a peer already
//! granted for a collection skips the ask entirely, and each fetch still mints a fresh ticket. The
//! old re-ask path (`snapshot_fingerprint` + the stale flag → new request, new ticket) is no longer
//! the *required* route for a granted peer, but nothing here forbids it either — `request_id` stays
//! per-ask, and the asker's `ask_nonce` gate (fail-closed on `None`) still binds each ticket to one
//! specific ask, so a standing grant never becomes a licence to make our client dial an address of
//! someone's choosing unprompted. The one-per-request framing now describes mint discipline, not
//! a limit on how often a granted peer may fetch: the ticket records that gate the re-serve are
//! hb-app's (`load_issued_ticket` / `consumed_at`), not anything in this module.

use serde::{Deserialize, Serialize};

use crate::error::HbError;
use crate::transport_payload::ManifestPayload;

/// Ticket schema version. Pinned in `wire_freeze`: a ticket rides a durable NIP-17 DM, so one
/// already sitting in a relay-stored wrap must stay readable.
pub const TICKET_V: u8 = 1;

/// The `content.hb` discriminator marking a DM body as a transport ticket — the owner→asker
/// direction, distinct from `manifest_request`'s asker→owner. Frozen for the same reason.
pub const TICKET_TAG: &str = "transport_ticket";

/// Where a redeemer stands with the issuer **right now**, read at redeem time rather than trusted
/// from the ticket. `Unknown` is deliberately its own case and deliberately refused: a redeemer the
/// issuer can no longer identify is not "probably fine".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactStanding {
    /// A saved contact in good standing.
    Good,
    /// Explicitly blocked since the ticket was issued.
    Blocked,
    /// Their request was declined since the ticket was issued.
    Declined,
    /// Not a known contact (removed, or never saved).
    Unknown,
}

impl ContactStanding {
    fn may_redeem(self) -> bool {
        matches!(self, ContactStanding::Good)
    }
}

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
        Ok(())
    }

    /// Does the ticket a redeemer presented match the one this node issued?
    ///
    /// **Every field except `node_addr`** — and that exclusion is the whole reason this method
    /// exists rather than a `==`.
    ///
    /// The owner side used to compare the structs with `==`, which was wrong in a way that broke the
    /// feature outright between two real machines (owner devtest 2026-08-27: "Send the full list"
    /// returned the forged-ticket refusal with both peers online). The asker is *required* to rewrite
    /// `node_addr` before redeeming: `redeem_manifest_ticket` runs it through `sanitize_node_addr`,
    /// the QURATOR-113 #20 SSRF guard, which drops loopback/RFC1918/link-local/CGNAT transport
    /// addresses and re-serializes what is left. Two machines on a LAN both advertise RFC1918
    /// addresses, so the guard always changed the string, so the comparison could never succeed. Even
    /// with nothing dropped, a parse-and-re-serialize round trip is not guaranteed byte-identical.
    /// The loopback QUIC tests never caught it because they call `fetch_manifest` directly and so
    /// never sanitize.
    ///
    /// **Excluding it costs nothing, because `node_addr` was never part of the capability.** It is
    /// the ISSUER'S OWN address — a value this node generated and put in the ticket itself. Making
    /// the asker echo it back byte-for-byte authenticates nobody: an attacker holding the ticket
    /// holds the address too, and a redeemer that reached us self-evidently found a working address
    /// already. The secret that rode a sealed one-recipient DM is the `request_id`/`ask_nonce`/`slug`
    /// triple, and every one of those is still compared here. `authorize_redemption` then applies the
    /// live standing and spent checks on top.
    #[must_use]
    pub fn matches_issued(&self, issued: &Self) -> bool {
        self.hb == issued.hb
            && self.ticket_v == issued.ticket_v
            && self.request_id == issued.request_id
            && self.slug == issued.slug
            && self.issued_at == issued.issued_at
            && self.ask_nonce == issued.ask_nonce
    }
}

/// Permission to attempt **one** redemption — and the only route to a [`ConsumedTicket`].
///
/// `#[must_use]` because ignoring one silently discards an authorization the user granted. Dropping
/// it *without* calling [`Self::into_consumed`] is the failed-connection path and is deliberately a
/// no-op: **the ticket stays unspent and a retry works.**
///
/// "Consumed on success, not on attempt" is a **compile-time** property, not a tested one — a
/// caller cannot assert success, only demonstrate it. This must not compile:
///
/// ```compile_fail
/// # use hb_core::ticket::{authorize_redemption, ContactStanding, TransportTicket};
/// let t = TransportTicket::issue("req-1", "slug", "addr", 0, None);
/// let grant = authorize_redemption(&t, "req-1", false, ContactStanding::Good).unwrap();
/// // A dropped connection has no ManifestPayload to hand over, so there is no way to reach
/// // ConsumedTicket from here. Calling it with nothing is a type error, by design.
/// let _receipt = grant.into_consumed();
/// ```
#[must_use = "a grant that is neither redeemed nor deliberately dropped means the approval was lost"]
#[derive(Debug)]
pub struct RedemptionGrant {
    request_id: String,
    slug: String,
}

impl RedemptionGrant {
    /// Spend the ticket — **only reachable with the delivered payload in hand.**
    ///
    /// This signature *is* the enforcement of "consumed on success, not on attempt": there is no
    /// way to call it after a dropped connection, because a dropped connection has no
    /// [`ManifestPayload`] to pass. (Taking the payload by reference and returning a receipt, rather
    /// than taking a bool, is the same discipline as INV-4′ mechanism 1 — the caller cannot assert
    /// success, only demonstrate it.)
    pub fn into_consumed(self, delivered: &ManifestPayload) -> ConsumedTicket {
        ConsumedTicket {
            request_id: self.request_id,
            slug: self.slug,
            delivered_bytes: delivered.len(),
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }
}

/// Proof that a ticket was spent on a completed transfer. The caller records this so a replay of
/// the same ticket is refused (see [`authorize_redemption`]'s `already_consumed`).
///
/// **The fields are private on purpose.** A receipt is evidence, and public fields made it
/// *constructible* — anything could mint a `ConsumedTicket { .. }` and burn a ticket that was never
/// delivered, which is the same class of hole as a serde derive on [`ManifestPayload`]. The only
/// way to obtain one is [`RedemptionGrant::into_consumed`], with the payload in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumedTicket {
    request_id: String,
    slug: String,
    delivered_bytes: usize,
}

impl ConsumedTicket {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// Size of what was actually delivered — the receipt's evidence, and useful for a log line.
    pub fn delivered_bytes(&self) -> usize {
        self.delivered_bytes
    }
}

/// The redeem-time gate, run by the **issuer's** node as it accepts a connection.
///
/// Four checks, in this order, each of which a real failure mode motivates:
///
/// 1. the ticket is structurally a ticket of a version we speak;
/// 2. it answers **this** request (one ticket per ask — no spending A's ticket on B);
/// 3. it has not already been consumed (replay of a completed transfer);
/// 4. the redeemer is **still** a contact in good standing (revocability — the property
///    valid-until-redeemed would otherwise cost).
///
/// Deliberately absent: any expiry comparison. There is no clock input, so this function
/// *cannot* time-box a ticket even by accident.
pub fn authorize_redemption(
    ticket: &TransportTicket,
    for_request_id: &str,
    already_consumed: bool,
    standing: ContactStanding,
) -> Result<RedemptionGrant, HbError> {
    ticket.verify_shape()?;
    if ticket.request_id != for_request_id {
        return Err(HbError::InvalidTicket("ticket was issued for a different request".into()));
    }
    if already_consumed {
        return Err(HbError::TicketAlreadyRedeemed);
    }
    if !standing.may_redeem() {
        return Err(HbError::TicketRedeemerNotInGoodStanding);
    }
    Ok(RedemptionGrant { request_id: ticket.request_id.clone(), slug: ticket.slug.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::manifest::build_manifest_envelope;

    fn ticket() -> TransportTicket {
        TransportTicket::issue("req-1", "my-slug", "node-addr-opaque", 1_700_000_000, Some("nonce-1"))
    }

    fn delivered() -> ManifestPayload {
        let id = Identity::generate();
        let env = build_manifest_envelope(
            &id,
            "my-slug",
            &[3u8; 32],
            "fp-abc",
            1_700_000_000,
            &[r#"{"part":0,"entries":[]}"#.to_string()],
        )
        .unwrap();
        ManifestPayload::seal(&env).unwrap()
    }

    #[test]
    fn a_valid_ticket_for_this_request_is_authorized() {
        let grant =
            authorize_redemption(&ticket(), "req-1", false, ContactStanding::Good).unwrap();
        assert_eq!(grant.request_id(), "req-1");
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

    #[test]
    fn ticket_round_trips_through_a_dm_body() {
        let t = ticket();
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(TICKET_TAG), "the DM body carries its discriminator");
        let back: TransportTicket = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        back.verify_shape().unwrap();
    }

    /// **Property 1 — a failed connection costs nothing.** The grant is dropped without
    /// `into_consumed`, the ticket is still not consumed, and the retry authorizes again.
    #[test]
    fn a_failed_attempt_does_not_consume_the_ticket_and_a_retry_succeeds() {
        let t = ticket();
        let mut consumed = false;

        // Attempt 1: authorized, then the connection dies. Nothing calls `into_consumed`.
        let grant = authorize_redemption(&t, "req-1", consumed, ContactStanding::Good).unwrap();
        drop(grant);
        assert!(!consumed, "a dropped grant cannot have consumed anything — there is no path");

        // Attempt 2: the same ticket still works.
        let grant = authorize_redemption(&t, "req-1", consumed, ContactStanding::Good)
            .expect("a retry after a failed connection must be authorized");
        let receipt = grant.into_consumed(&delivered());
        consumed = true;
        assert_eq!(receipt.request_id(), "req-1");
        assert!(receipt.delivered_bytes() > 0, "the receipt records what actually arrived");

        // Attempt 3: now that it succeeded, a replay is refused.
        assert!(
            matches!(
                authorize_redemption(&t, "req-1", consumed, ContactStanding::Good),
                Err(HbError::TicketAlreadyRedeemed)
            ),
            "a completed transfer consumes the ticket and a replay is refused"
        );
    }

    /// **Property 3 — not time-boxed.** An approval issued while the asker was offline is still
    /// redeemable when they return, however long that took.
    #[test]
    fn an_approval_given_while_the_asker_was_offline_is_still_redeemable() {
        // Issued a year ago. There is no clock argument to `authorize_redemption` at all, so this
        // cannot depend on elapsed time — which is the point.
        let ancient = TransportTicket::issue("req-1", "my-slug", "node-addr-opaque", 1, None);
        let grant = authorize_redemption(&ancient, "req-1", false, ContactStanding::Good)
            .expect("a ticket is valid until redeemed, never expired");
        assert_eq!(grant.request_id(), "req-1");
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

    /// **The revocation test** — the property that valid-until-redeemed would otherwise cost.
    #[test]
    fn a_redeemer_blocked_or_declined_after_approval_is_refused() {
        let t = ticket();
        for standing in [ContactStanding::Blocked, ContactStanding::Declined, ContactStanding::Unknown] {
            assert!(
                matches!(
                    authorize_redemption(&t, "req-1", false, standing),
                    Err(HbError::TicketRedeemerNotInGoodStanding)
                ),
                "{standing:?} must not be able to redeem an already-issued ticket"
            );
        }
        // And the ticket itself is untouched — revoking standing is not the same as burning it, so
        // restoring the contact restores the approval rather than requiring a fresh request.
        let restored = authorize_redemption(&t, "req-1", false, ContactStanding::Good).unwrap();
        assert_eq!(restored.request_id(), "req-1");
    }

    /// **The standing grant as this module sees it** (owner ruling 2026-08-31, QURATOR-137). A
    /// granted peer's next fetch is a NEWLY MINTED ticket: the previous ticket's consumption is
    /// spent-ticket bookkeeping, not a revoke of the peer. And the grant is re-read live at EVERY
    /// redeem, so a contact blocked between fetches is refused on the next one — at this node's own
    /// gate only, NOT system-wide: since Carrier 4 a mutual contact may still re-serve its cached
    /// copy without consulting the author, and re-keying does not rescue that. Never call this
    /// end-to-end revocation.
    ///
    /// P-10 mutations the orchestrator must apply and see red (do not run here):
    /// (a) delete the `if already_consumed { return Err(HbError::TicketAlreadyRedeemed); }` arm in
    ///     `authorize_redemption` — the replay assertion below must fail;
    /// (b) delete the `if !standing.may_redeem()` arm in the same function — the Blocked assertion
    ///     must fail. (This test shares (b) with
    ///     `a_redeemer_blocked_or_declined_after_approval_is_refused`; (a) is the discriminator
    ///     unique to this test's first half.)
    /// No edit *inside* this module can redden the "second mint authorizes" half, because the module
    /// holds no cross-call state — that statelessness is the finding: hb-core needed no code change
    /// for the ruling, only this pin plus the rationale rewrite above.
    #[test]
    fn a_standing_grant_re_mints_rather_than_replays_and_is_rechecked_every_redeem() {
        // Fetch 1: the granted peer redeems ticket req-1 successfully.
        let first = ticket();
        let grant = authorize_redemption(&first, "req-1", false, ContactStanding::Good).unwrap();
        let _receipt = grant.into_consumed(&delivered());

        // That ticket is spent: a replay of it is refused — spent-ticket bookkeeping.
        assert!(
            matches!(
                authorize_redemption(&first, "req-1", true, ContactStanding::Good),
                Err(HbError::TicketAlreadyRedeemed)
            ),
            "consuming one ticket never licenses a replay of the same ticket"
        );

        // Fetch 2 — same peer, same collection, no new ask needed: a FRESH mint authorizes. The
        // prior consumption is not a revoke of the grant.
        let second = TransportTicket::issue(
            "req-2",
            "my-slug",
            "node-addr-opaque",
            1_700_000_100,
            Some("nonce-2"),
        );
        let grant = authorize_redemption(&second, "req-2", false, ContactStanding::Good)
            .expect("a granted peer's next fetch is a new ticket, not a replay of the spent one");
        drop(grant); // not delivered — costs nothing, and does not touch the grant

        // Blocked between fetches: the NEXT fetch is refused, here, at this node's own gate.
        let third = TransportTicket::issue(
            "req-3",
            "my-slug",
            "node-addr-opaque",
            1_700_000_200,
            Some("nonce-3"),
        );
        assert!(
            matches!(
                authorize_redemption(&third, "req-3", false, ContactStanding::Blocked),
                Err(HbError::TicketRedeemerNotInGoodStanding)
            ),
            "blocking the contact must refuse the next fetch at this node's gate"
        );

        // Unblocked again: the following fetch mints and authorizes — refusal is per-check, not
        // sticky, and no time-box intervenes.
        let fourth = TransportTicket::issue(
            "req-4",
            "my-slug",
            "node-addr-opaque",
            1_700_000_300,
            Some("nonce-4"),
        );
        assert!(
            authorize_redemption(&fourth, "req-4", false, ContactStanding::Good).is_ok(),
            "restoring the contact restores the grant — the refusal was the standing read, not a burn"
        );
    }

    /// **One ticket per ask** — a ticket cannot be spent on a different request or collection.
    /// UNCHANGED by the 2026-08-31 standing-grant ruling: that ruling changes WHO is entitled to a
    /// ticket (a granted peer, without re-asking), not WHAT a ticket is bound to. `request_id` +
    /// `ask_nonce` remain the anti-forgery binding (a peer must not make our client dial an address
    /// of their choosing "for" a request we never made), so this stays exactly as pinned.
    #[test]
    fn a_ticket_is_bound_to_its_own_request() {
        let t = ticket();
        assert!(
            matches!(
                authorize_redemption(&t, "req-2", false, ContactStanding::Good),
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
    /// **The devtest 2026-08-27 regression.** The owner clicked "Send the full list" with both dev
    /// machines online and the asker got the refusal reserved for a forged ticket. Cause: the owner
    /// side compared the two tickets with `==`, but the asker MUST rewrite `node_addr` first
    /// (`sanitize_node_addr`, the QURATOR-113 #20 SSRF guard, drops RFC1918/loopback/link-local/CGNAT
    /// addresses and re-serializes). Two machines on a LAN always trip that, so the check could never
    /// pass. This is the red-green: it fails against a `==` comparison and passes against
    /// `matches_issued`.
    #[test]
    fn a_redeemer_that_sanitized_the_node_addr_still_matches_the_issued_ticket() {
        let issued = TransportTicket::issue(
            "req-1",
            "vhs",
            r#"{"node_id":"abc","addrs":["192.168.1.20:41234","203.0.113.9:41234"]}"#,
            1_700_000_000,
            Some("nonce-1"),
        );

        // What the asker actually presents: same ticket, private address stripped by the SSRF guard.
        let mut presented = issued.clone();
        presented.node_addr = r#"{"node_id":"abc","addrs":["203.0.113.9:41234"]}"#.to_string();

        assert_ne!(presented, issued, "the fixture must actually differ, or this proves nothing");
        assert!(
            presented.matches_issued(&issued),
            "a redeemer that sanitized the address it was given must still be recognised"
        );
    }

    /// The other half: everything that IS the capability still has to match, or the exclusion of
    /// `node_addr` would have widened the gate instead of correcting it. Each field is mutated on its
    /// own so a single over-broad `true` cannot pass this.
    #[test]
    fn matches_issued_still_refuses_every_capability_field_mismatch() {
        let issued = TransportTicket::issue("req-1", "vhs", "addr-a", 1_700_000_000, Some("nonce-1"));

        let mut wrong_request = issued.clone();
        wrong_request.request_id = "req-2".into();
        assert!(!wrong_request.matches_issued(&issued), "a different request id is refused");

        let mut wrong_slug = issued.clone();
        wrong_slug.slug = "betamax".into();
        assert!(!wrong_slug.matches_issued(&issued), "a ticket redirected at another collection is refused");

        let mut wrong_nonce = issued.clone();
        wrong_nonce.ask_nonce = Some("nonce-2".into());
        assert!(!wrong_nonce.matches_issued(&issued), "a different ask nonce is refused");

        let mut no_nonce = issued.clone();
        no_nonce.ask_nonce = None;
        assert!(!no_nonce.matches_issued(&issued), "dropping the nonce entirely is refused");

        let mut wrong_issued_at = issued.clone();
        wrong_issued_at.issued_at += 1;
        assert!(!wrong_issued_at.matches_issued(&issued), "a re-dated ticket is refused");

        let mut wrong_v = issued.clone();
        wrong_v.ticket_v = TICKET_V + 1;
        assert!(!wrong_v.matches_issued(&issued), "a different ticket version is refused");

        let mut wrong_tag = issued.clone();
        wrong_tag.hb = "hb-something-else".into();
        assert!(!wrong_tag.matches_issued(&issued), "a different discriminator is refused");

        assert!(issued.matches_issued(&issued), "and the unmodified ticket still matches");
    }
}
