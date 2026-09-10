//! The **access request** (QURATOR-137 slice 2) — the asker→issuer "may I have your share code?" DM.
//!
//! The reverse-direction sibling of [`crate::ticket::TransportTicket`]: a ticket flows
//! issuer→asker (the grant of *connect and fetch*); an access request flows asker→issuer (the ask
//! for the browse key itself). It rides a **NIP-17 DM to one recipient** exactly as a ticket and a
//! `manifest_request` do — the DM provides the seal, so this module deliberately adds no crypto of
//! its own, exactly like `ticket.rs`.
//!
//! ## What it is for
//!
//! Today the "Ask for access" affordance (hb-app, contacts page) deep-links into the chat composer
//! with **prefilled prose** — a human being then edits and sends an ordinary kind-14 chat DM, which
//! is indistinguishable on the wire from any other message. Nothing marks it as a request, so the
//! issuer's client cannot recognise one, and answering stays a manual "Share my code" insert. This
//! type is the machine-recognisable form: a small JSON body whose `content.hb` discriminator says
//! what it is, mirroring how `{"hb":"manifest_request",…}` marks the manifest ask.
//!
//! ## Failure is the pre-existing behaviour
//!
//! [`AccessRequest::parse`] returns `None` for **anything** that is not a well-formed, recognised
//! access request: plain prose, malformed JSON, another structured body's tag, or a version this
//! build does not speak. `None` means "ordinary chat message" — which was the ONLY behaviour for
//! every DM before this type existed, so there is no downgrade risk in failing to parse. The caller
//! must treat `None` as fall-through, never as an error. Do not panic, do not partially trust a
//! malformed payload.
//!
//! ## Field notes (mirroring `ticket.rs`'s rulings)
//!
//! - **`asker_npub` is carried explicitly** even though it is recoverable from the DM's NIP-17 seal
//!   signer — the same rationale as `author_npub` on `TransportTicket`: usable without re-deriving
//!   it. The seal remains the authoritative attribution; this field is convenience, and a recipient
//!   MUST NOT treat a mismatch between the two as anything but a malformed request (the seal is what
//!   a future auto-answer keys its allow/block decision on — blocking gates chat/DM interaction
//!   only, owner ruling 2026-09-03 QURATOR-177, and a DM from a blocked peer is already dropped by
//!   the inbox before any body parsing runs).
//! - **`nonce` is asker-generated, unique per request** — the `ask_nonce` shape: an echoable value
//!   that binds an eventual answer to *this* ask.
//! - **`requested_at` is provenance/display only, NOT an expiry input** — the `issued_at` precedent.
//!   There is deliberately no expiry field at all (see [`a_request_has_no_expiry_by_design`]); the
//!   "no time-box" owner ruling of 2026-07-30 for tickets applies with the same force here: a window
//!   would silently discard a request both humans already agreed to, whenever the issuer happens to
//!   be offline at the moment it was sent.
//!
//! **Not in scope here: the answer.** This type is the request only. The grant half (auto-sending
//! the browse key / share code to an unblocked asker) has no existing issuance path to reuse —
//! `seal_key_grant` has no hb-app caller, and the standing-grant map was deleted (QURATOR-177/164,
//! 2026-09-03/04) — so it is deliberately NOT built in this slice.

use serde::{Deserialize, Serialize};

use crate::error::HbError;

/// Access-request schema version. Pinned in `wire_freeze`: a request rides a durable NIP-17 DM, so
/// one already sitting in a relay-stored wrap must stay readable.
pub const ACCESS_REQUEST_V: u8 = 1;

/// The `content.hb` discriminator marking a DM body as an access request — the asker→issuer
/// direction, distinct from `transport_ticket` (issuer→asker) and `manifest_request` (the manifest
/// ask, which is about one collection, not the browse key). Frozen for the same reason.
pub const ACCESS_REQUEST_TAG: &str = "access_request";

/// An access request: the asker's identity, a per-request nonce, and a timestamp.
///
/// **Note what is absent: there is no expiry field** — the owner's no-time-box ruling made
/// structural, so a later "consistency" change cannot time-box a request without changing this
/// type, which [`a_request_has_no_expiry_by_design`] will notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRequest {
    /// Always [`ACCESS_REQUEST_TAG`] — how the issuer's inbox recognises the DM.
    pub hb: String,
    pub v: u8,
    /// Who is asking. Also recoverable from the DM's NIP-17 seal signer, but carried explicitly so
    /// a recipient can key on it without re-deriving it (the `author_npub` rationale).
    pub asker_npub: String,
    /// The asker's nonce for this ask — unique per request, echoable into an eventual answer (the
    /// `ask_nonce` shape: what binds an answer to *this* ask).
    pub nonce: String,
    /// Unix seconds the request was sent. Provenance and display only — **not an expiry input.**
    pub requested_at: u64,
}

impl AccessRequest {
    /// Mint an access request. `asker_npub` is this node's own npub in the production send path
    /// (`send_access_request_inner` in hb-app); `nonce` should be fresh randomness minted at the
    /// send site, never caller-supplied from the UI.
    pub fn new(asker_npub: &str, nonce: &str, requested_at: u64) -> Self {
        Self {
            hb: ACCESS_REQUEST_TAG.to_string(),
            v: ACCESS_REQUEST_V,
            asker_npub: asker_npub.to_string(),
            nonce: nonce.to_string(),
            requested_at,
        }
    }

    /// Serialize to the DM `content` string (canonical JSON). The inverse of [`Self::parse`].
    pub fn to_dm_content(&self) -> Result<String, HbError> {
        serde_json::to_string(self).map_err(|e| HbError::InvalidAccessRequest(e.to_string()))
    }

    /// Try to recognise an incoming DM body as an access request. `Some` **only** when the body is
    /// JSON, carries the [`ACCESS_REQUEST_TAG`] discriminator, speaks a version this build
    /// recognises, and has non-blank `asker_npub`/`nonce`. **`None` for everything else — plain
    /// prose, malformed JSON, another structured body, an unknown version — which the caller must
    /// treat as an ordinary chat message.** Failure-to-parse is the pre-existing behaviour for
    /// every DM, so `None` is always safe to fall through on.
    pub fn parse(content: &str) -> Option<Self> {
        let req: Self = serde_json::from_str(content).ok()?;
        req.verify_shape().ok()?;
        Some(req)
    }

    /// Structural self-check: the discriminator and version are recognised and the bindings are
    /// present. An unknown version is *recognised and refused*, never mis-read — the same
    /// forward-compat contract `TICKET_V` upholds. Present-but-blank identity fields are malformed,
    /// not absent (absent cannot happen for non-`Option` fields — a missing key fails
    /// deserialization — so a blank can only arrive from the wire, and a serde derive is an
    /// unvalidated public constructor).
    pub fn verify_shape(&self) -> Result<(), HbError> {
        if self.hb != ACCESS_REQUEST_TAG {
            return Err(HbError::InvalidAccessRequest("not an access request".into()));
        }
        if self.v == 0 || self.v > ACCESS_REQUEST_V {
            return Err(HbError::UnsupportedVersion(self.v));
        }
        if self.asker_npub.is_empty() || self.nonce.is_empty() {
            return Err(HbError::InvalidAccessRequest(
                "access request is missing a required binding".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AccessRequest {
        AccessRequest::new("npub1asker", "nonce-1", 1_700_000_000)
    }

    /// Round-trip: what `new` mints survives the DM body verbatim and parses back — the asker's
    /// npub and nonce are what an eventual answer must key on, so they must not drift through
    /// serialization.
    ///
    /// MUTATION (P-10, applied by the orchestrator — this lane compiles nothing): in
    /// `AccessRequest::parse`, delete the `req.verify_shape().ok()?;` line — this test still PASSES
    /// (round-trip needs no gate), which is why the fall-through test below is the one that reds.
    /// The meaningful mutation for THIS test: in `to_dm_content`, change
    /// `serde_json::to_string(self)` to `serde_json::to_string(&serde_json::json!({"x": 1}))` —
    /// it reds on the `.unwrap()`/`parse` assertion while still compiling.
    #[test]
    fn round_trips_through_a_dm_body() {
        let r = request();
        let json = r.to_dm_content().unwrap();
        assert!(json.contains(ACCESS_REQUEST_TAG), "the DM body carries its discriminator");
        assert!(json.contains("\"asker_npub\":\"npub1asker\""), "the field name is the contract");
        let back = AccessRequest::parse(&json).expect("a freshly minted request parses back");
        assert_eq!(back, r);
    }

    /// **The only safe fall-through.** Anything that is not a well-formed, recognised access
    /// request parses to `None` — plain prose, malformed JSON, another structured body's tag, a
    /// body with the right tag but a version this build does not speak, and a JSON object that is
    /// simply not this shape. `None` means "ordinary chat message", which was the behaviour for
    /// every DM before this type existed.
    ///
    /// MUTATION (P-10, applied by the orchestrator — this lane compiles nothing): in
    /// `verify_shape`, delete the `if self.hb != ACCESS_REQUEST_TAG { return Err(...); }` arm —
    /// the `manifest_request` body (whose `slug`-carrying JSON still deserializes once the tag
    /// check is gone? NO: its fields do not satisfy this struct, so it stays `None`; the cases
    /// that red are the `"hb":"chat"` and `"hb":"transport_ticket"` tags built over THIS struct's
    /// field names below) — this test reds on the two wrong-tag assertions while still compiling.
    #[test]
    fn plain_prose_and_wrong_tags_are_not_access_requests() {
        assert!(AccessRequest::parse("Hi, could I have your share code?").is_none(),
            "prose — the prefilled ask-access draft — is an ordinary chat DM");
        assert!(AccessRequest::parse("not json at all").is_none());
        assert!(AccessRequest::parse(r#"{"hb":"manifest_request","slug":"vault"}"#).is_none(),
            "the manifest ask is a DIFFERENT ask and must not be mis-read");
        assert!(AccessRequest::parse(r#"{"v":1,"asker_npub":"npub1a","nonce":"n"}"#).is_none(),
            "no discriminator is not this body");

        let wrong_tag = r#"{"hb":"chat","v":1,"asker_npub":"npub1a","nonce":"n","requested_at":1}"#;
        assert!(AccessRequest::parse(wrong_tag).is_none(), "a chat-tagged body is not a request");
        let ticket_tag =
            r#"{"hb":"transport_ticket","v":1,"asker_npub":"npub1a","nonce":"n","requested_at":1}"#;
        assert!(AccessRequest::parse(ticket_tag).is_none(),
            "the issuer→asker direction is not this asker→issuer body");
    }

    /// An unknown version is recognised and refused, never mis-read — the forward-compat contract
    /// `TICKET_V` upholds. A v2 body from a future build parses to `None` (ordinary chat), and
    /// `verify_shape` names the version as the reason.
    ///
    /// MUTATION (P-10, applied by the orchestrator — this lane compiles nothing): in
    /// `verify_shape`, delete the `if self.v == 0 || self.v > ACCESS_REQUEST_V { return Err(
    /// HbError::UnsupportedVersion(self.v)); }` arm — this test reds on the `.is_none()` assertion
    /// (a v2 body would parse) while still compiling.
    #[test]
    fn an_unknown_version_is_recognised_and_refused() {
        let mut future = request();
        future.v = ACCESS_REQUEST_V + 1;
        assert!(AccessRequest::parse(&future.to_dm_content().unwrap()).is_none(),
            "a version this build does not speak is NOT a request — fall through to chat");
        assert!(
            matches!(future.verify_shape(), Err(HbError::UnsupportedVersion(v)) if v == ACCESS_REQUEST_V + 1),
            "the refusal names the version, never mis-reads the body"
        );
    }

    /// Present-but-blank identity fields are malformed, not "absent" — the `ask_nonce`/`author_npub`
    /// blank shape from `ticket.rs`. These are non-`Option` fields, so a missing key already fails
    /// deserialization; a blank can only arrive from the wire, and this gate meets it.
    ///
    /// MUTATION (P-10, applied by the orchestrator — this lane compiles nothing): in
    /// `verify_shape`, change `if self.asker_npub.is_empty() || self.nonce.is_empty()` to
    /// `if false` (or delete the arm) — this test reds on both `.is_none()` assertions while still
    /// compiling.
    #[test]
    fn blank_identity_or_nonce_is_refused() {
        let blank_npub = r#"{"hb":"access_request","v":1,"asker_npub":"","nonce":"n","requested_at":1}"#;
        assert!(AccessRequest::parse(blank_npub).is_none(), "a blank asker npub is malformed");
        let blank_nonce =
            r#"{"hb":"access_request","v":1,"asker_npub":"npub1a","nonce":"","requested_at":1}"#;
        assert!(AccessRequest::parse(blank_nonce).is_none(), "a blank nonce is malformed");
    }

    /// **Not time-boxed — structurally.** The absence of an expiry field is the owner's ruling made
    /// structural (the `a_ticket_has_no_expiry_by_design` precedent): a request sent while the
    /// issuer was offline must still be answerable whenever they next come online. The serialized
    /// body is the durable artifact, so that is what is checked.
    ///
    /// MUTATION (P-10, applied by the orchestrator — this lane compiles nothing): add an
    /// `expires_at: u64` field (or any field named in the list) to the struct — this test reds on
    /// the `contains` assertion while still compiling.
    #[test]
    fn a_request_has_no_expiry_by_design() {
        let json = request().to_dm_content().unwrap();
        for forbidden in ["expires", "expiry", "ttl", "valid_until", "not_after"] {
            assert!(
                !json.contains(forbidden),
                "an access request must not carry `{forbidden}` — a request is answerable \
                 whenever the issuer next comes online, never expired"
            );
        }
    }
}
