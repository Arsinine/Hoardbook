//! The presence **freshness** binding (spec §Data Model — presence row, §Privacy Model).
//! *"npub X is online as of T, valid until T+ttl."*
//!
//! Modelled as a signed presence event (`KIND_PRESENCE`) whose tags carry a schema version and an
//! explicit `expires_at`. Because NIP-01 signs a hash over the tags, the Schnorr signature covers
//! the validity window; `verify_binding` additionally pins the author to the **expected** `npub`,
//! so a lying relay cannot pass off a valid-but-different identity's presence as yours.
//!
//! **v0.9.6 — Hoardbook moves no files (INV-4).** The transport plane (download/sync) lives in the
//! Mascara companion, so presence carries **no dialable address and no node key** — it is purely a
//! freshness signal for online status. The former `npub`→iroh-node binding, the sealed address, and
//! the node-key resolution all moved to Mascara with file transfer; what remains here is the
//! signature- + author- + freshness-checked online beacon.

use nostr::prelude::*;

use crate::error::HbError;
use crate::identity::{verify_event, Identity};
use crate::tag_util::{tag_u64, tag_val, TagU64};
use crate::version::{check_schema, SCHEMA_V};

/// Presence event kind (replaceable, 1xxxx range — newest per author wins).
pub const KIND_PRESENCE: u16 = 11_111;
/// Maximum validity window a binding may claim. Presence refreshes every ~5 min, so a day is a
/// generous backstop; a verifier refuses any binding asserting a longer window, containing the
/// blast radius of a misconfigured or mistakenly-published binding.
pub const MAX_BINDING_TTL_SECS: u64 = 24 * 60 * 60;

pub(crate) const TAG_EXPIRES: &str = "hb-expires"; // explicit expiry, unix seconds
pub(crate) const TAG_SCHEMA: &str = "hb-v"; // payload schema version
/// Tolerance for a `created_at` slightly ahead of our clock (matches the ±300 s skew window).
const FUTURE_SKEW_SECS: u64 = 300;

/// A verified presence binding. All fields are read straight from the signed event.
#[derive(Debug, Clone)]
pub struct Binding {
    pub npub: PublicKey,
    pub created_at: u64,
    pub expires_at: u64,
}

/// Build a signed presence beacon for `identity`, valid for `ttl_secs` from `now`. Carries only a
/// schema version + expiry — **no node key, no address** (Hoardbook moves no files; transport lives
/// in Mascara). The signature covers the validity window; freshness = `created_at` recency.
pub fn build_binding(identity: &Identity, now: u64, ttl_secs: u64) -> Result<Event, HbError> {
    if ttl_secs > MAX_BINDING_TTL_SECS {
        return Err(HbError::InvalidEvent(format!(
            "binding ttl {ttl_secs}s exceeds max {MAX_BINDING_TTL_SECS}s"
        )));
    }
    let expires_at = now.saturating_add(ttl_secs);
    let tags = [
        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
        Tag::custom(TagKind::custom(TAG_EXPIRES), [expires_at.to_string()]),
    ];
    identity.sign(
        EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
            .tags(tags)
            .custom_created_at(Timestamp::from(now)),
    )
}

/// Verify a presence event as of `now`, pinned to the `expected` author. The verification semantics
/// (Schnorr + kind-pin + author-pin + validity window + freshness) are unchanged from before the
/// v0.9.6 cut; only the address/node-key reads are gone (presence carries neither).
pub fn verify_binding(event: &Event, expected: &PublicKey, now: u64) -> Result<Binding, HbError> {
    // (1) Schnorr signature + canonical id: the author signed exactly these tags.
    verify_event(event)?;
    // (2) Kind pin — a presence event, not some other kind with confusable tags (NIP-01 clients key
    //     behaviour off kind; without this a profile event could pose as a presence beacon).
    if event.kind != Kind::from_u16(KIND_PRESENCE) {
        return Err(HbError::InvalidEvent(format!(
            "expected presence kind {KIND_PRESENCE}, got {}",
            event.kind.as_u16()
        )));
    }
    // (3) Author pin — the true wrong-signer gate. A *valid* presence from a different identity is
    //     rejected (a relay can't pass off someone else's beacon as the expected npub's).
    if &event.pubkey != expected {
        return Err(HbError::WrongSigner);
    }
    // (4) Schema version recognised (forward-compat).
    let schema = tag_val(event, TAG_SCHEMA)
        .and_then(|s| s.parse::<u8>().ok())
        .ok_or_else(|| HbError::InvalidEvent("missing or malformed schema version".into()))?;
    check_schema(schema)?;
    // (5) Validity window — explicit expiry, bounded so a misconfigured caller can't mint a beacon
    //     that lives for years.
    let created = event.created_at.as_u64();
    if created > now.saturating_add(FUTURE_SKEW_SECS) {
        return Err(HbError::BindingNotYetValid);
    }
    let expires_at = match tag_u64(event, TAG_EXPIRES) {
        TagU64::Value(v) => v,
        TagU64::Missing => return Err(HbError::InvalidEvent("missing expires_at".into())),
        TagU64::Malformed(s) => {
            return Err(HbError::InvalidEvent(format!("malformed expires_at: {s}")))
        }
    };
    // Reject a degenerate / inverted window (`expires_at <= created`) explicitly: otherwise the
    // saturating-sub window check below reads 0 and would wave through an event whose expiry precedes
    // its creation (a narrow band could then satisfy the `now <= expires_at` liveness check).
    if expires_at <= created {
        return Err(HbError::InvalidEvent("expires_at must be after created_at".into()));
    }
    if expires_at.saturating_sub(created) > MAX_BINDING_TTL_SECS {
        return Err(HbError::InvalidEvent("binding validity window exceeds the maximum".into()));
    }
    if now > expires_at {
        return Err(HbError::BindingExpired);
    }

    Ok(Binding { npub: event.pubkey, created_at: created, expires_at })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: u64 = 30 * 60;
    const NOW: u64 = 1_700_000_000;

    /// A fresh presence event: (identity, event, now).
    fn fresh() -> (Identity, Event, u64) {
        let id = Identity::generate();
        let ev = build_binding(&id, NOW, TTL).unwrap();
        (id, ev, NOW)
    }

    #[test]
    fn valid_presence_verifies_to_the_author_and_window() {
        let (id, ev, now) = fresh();
        let b = verify_binding(&ev, &id.public_key(), now).unwrap();
        assert_eq!(b.npub, id.public_key());
        assert_eq!(b.created_at, now);
        assert_eq!(b.expires_at, now + TTL);
    }

    /// INV-4 (v0.9.6): the presence beacon carries **no node key and no address** — Hoardbook moves
    /// no files, so a presence event must not advertise any dialable transport endpoint. This is the
    /// behavioural guard that the seal/node-key surface stays gone.
    #[test]
    fn presence_carries_no_address_or_node_key() {
        let (_id, ev, _now) = fresh();
        let json = ev.as_json();
        for forbidden in ["hb-node", "hb-saddr", "hb-addrs", "hb-cv"] {
            assert!(!json.contains(forbidden), "presence leaked a transport tag: {forbidden}");
        }
    }

    #[test]
    fn binding_by_wrong_npub_rejected() {
        // ID3/AB4: a *validly-signed* presence authored by B is rejected when we expect A.
        let (_id, ev, now) = fresh();
        let other = Identity::generate();
        assert!(matches!(verify_binding(&ev, &other.public_key(), now), Err(HbError::WrongSigner)));
    }

    #[test]
    fn expired_binding_rejected() {
        let (id, ev, now) = fresh();
        assert!(matches!(
            verify_binding(&ev, &id.public_key(), now + TTL + 1),
            Err(HbError::BindingExpired)
        ));
    }

    #[test]
    fn future_dated_binding_rejected() {
        let (id, ev, now) = fresh();
        let before = now.saturating_sub(FUTURE_SKEW_SECS + 60);
        assert!(matches!(
            verify_binding(&ev, &id.public_key(), before),
            Err(HbError::BindingNotYetValid)
        ));
    }

    #[test]
    fn wrong_kind_rejected() {
        // A non-presence event carrying presence-shaped tags must not pass (event-confusion guard).
        let id = Identity::generate();
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(30_117), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(TagKind::custom(TAG_EXPIRES), [(NOW + TTL).to_string()]),
                    ])
                    .custom_created_at(Timestamp::from(NOW)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), NOW), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn excessive_ttl_rejected_on_build() {
        let id = Identity::generate();
        assert!(build_binding(&id, NOW, MAX_BINDING_TTL_SECS + 1).is_err());
    }

    #[test]
    fn oversized_window_rejected_on_verify() {
        // A presence hand-built with a window beyond the max is refused even though it is validly
        // signed and unexpired.
        let id = Identity::generate();
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(
                            TagKind::custom(TAG_EXPIRES),
                            [(NOW + MAX_BINDING_TTL_SECS + 10).to_string()],
                        ),
                    ])
                    .custom_created_at(Timestamp::from(NOW)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), NOW), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn malformed_expires_rejected() {
        let id = Identity::generate();
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(TagKind::custom(TAG_EXPIRES), ["soon".to_string()]),
                    ])
                    .custom_created_at(Timestamp::from(NOW)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), NOW), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn inverted_window_rejected() {
        // `expires_at` BEFORE `created_at`, evaluated in the band `now <= expires_at < created` — the
        // exact case that, without the explicit guard, would slip through (window reads 0, `now <=
        // expires_at` passes the liveness check). It must be refused as a malformed event.
        let id = Identity::generate();
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(TagKind::custom(TAG_EXPIRES), [(NOW - 10).to_string()]),
                    ])
                    .custom_created_at(Timestamp::from(NOW)),
            )
            .unwrap();
        assert!(matches!(
            verify_binding(&ev, &id.public_key(), NOW - 30),
            Err(HbError::InvalidEvent(_))
        ));
    }
}
