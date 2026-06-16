//! The `npub` → iroh-node binding, carried in the presence event (spec §P2P Layer,
//! §File Sharing — H2/H17). *"npub X authorises iroh node Y, valid until T."*
//!
//! Modelled as a signed presence event (`KIND_PRESENCE`) whose tags carry the iroh node
//! key (a raw 32-byte Ed25519 `EndpointId` — hb-core stays transport-agnostic; hb-app maps
//! it to `iroh::EndpointId`), the advertised transport addresses, a schema version, and an
//! explicit `expires_at`. Because NIP-01 signs a hash over all of these, the Schnorr
//! signature covers the node key *and* the validity window. `verify_binding` additionally
//! pins the author to the **expected** `npub`, so a lying relay cannot substitute a
//! valid-but-different identity's binding.

use nostr::prelude::*;

use crate::error::HbError;
use crate::identity::{verify_event, Identity};
use crate::tag_util::{tag_u64, tag_val, tag_vals, TagU64};
use crate::version::{check_schema, SCHEMA_V};

/// Presence + binding event kind (replaceable, 1xxxx range — newest per author wins).
pub const KIND_PRESENCE: u16 = 11_111;
/// Maximum validity window a binding may claim. Presence refreshes every ~5 min, so a day
/// is a generous backstop; a verifier refuses any binding asserting a longer window,
/// containing the blast radius of a misconfigured or mistakenly-published binding.
pub const MAX_BINDING_TTL_SECS: u64 = 24 * 60 * 60;

const TAG_NODE: &str = "hb-node"; // iroh Ed25519 endpoint key, hex
const TAG_ADDRS: &str = "hb-addrs"; // transport/node-address seam (advertised list)
const TAG_EXPIRES: &str = "hb-expires"; // explicit expiry, unix seconds
const TAG_SCHEMA: &str = "hb-v"; // payload schema version
/// Tolerance for a `created_at` slightly ahead of our clock (matches the ±300 s skew window).
const FUTURE_SKEW_SECS: u64 = 300;

/// A verified binding.
#[derive(Debug, Clone)]
pub struct Binding {
    pub npub: PublicKey,
    pub node_key: [u8; 32],
    pub addrs: Vec<String>,
    pub created_at: u64,
    pub expires_at: u64,
}

/// Build a signed presence event binding `node_key` (advertising transport `addrs`) under
/// `identity`, valid for `ttl_secs` from `now`.
pub fn build_binding(
    identity: &Identity,
    node_key: &[u8; 32],
    addrs: &[String],
    now: u64,
    ttl_secs: u64,
) -> Result<Event, HbError> {
    if ttl_secs > MAX_BINDING_TTL_SECS {
        return Err(HbError::InvalidEvent(format!(
            "binding ttl {ttl_secs}s exceeds max {MAX_BINDING_TTL_SECS}s"
        )));
    }
    let expires_at = now.saturating_add(ttl_secs);
    let mut tags = vec![
        Tag::custom(TagKind::custom(TAG_NODE), [hex::encode(node_key)]),
        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
        Tag::custom(TagKind::custom(TAG_EXPIRES), [expires_at.to_string()]),
    ];
    if !addrs.is_empty() {
        tags.push(Tag::custom(TagKind::custom(TAG_ADDRS), addrs.iter().cloned()));
    }
    identity.sign(
        EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
            .tags(tags)
            .custom_created_at(Timestamp::from(now)),
    )
}

/// Verify a presence event's binding as of `now`, pinned to the `expected` author.
pub fn verify_binding(event: &Event, expected: &PublicKey, now: u64) -> Result<Binding, HbError> {
    // (1) Schnorr signature + canonical id: the author signed exactly these tags.
    verify_event(event)?;
    // (2) Kind pin — a presence/binding event, not some other kind with confusable tags
    //     (NIP-01 clients key behaviour off kind; without this a profile event could pose
    //     as a binding).
    if event.kind != Kind::from_u16(KIND_PRESENCE) {
        return Err(HbError::InvalidEvent(format!(
            "expected presence kind {KIND_PRESENCE}, got {}",
            event.kind.as_u16()
        )));
    }
    // (3) Author pin — the true wrong-signer gate. A *valid* binding from a different
    //     identity is rejected (H2: a relay can't redirect to a vouched-by-someone-else node).
    if &event.pubkey != expected {
        return Err(HbError::WrongSigner);
    }
    // (4) Schema version recognised (forward-compat).
    let schema = tag_val(event, TAG_SCHEMA)
        .and_then(|s| s.parse::<u8>().ok())
        .ok_or_else(|| HbError::InvalidEvent("missing or malformed schema version".into()))?;
    check_schema(schema)?;
    // (5) Validity window — explicit expiry, bounded so a misconfigured caller can't mint a
    //     binding that lives for years.
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
    if expires_at.saturating_sub(created) > MAX_BINDING_TTL_SECS {
        return Err(HbError::InvalidEvent("binding validity window exceeds the maximum".into()));
    }
    if now > expires_at {
        return Err(HbError::BindingExpired);
    }
    // (5) The bound node key.
    let node_hex =
        tag_val(event, TAG_NODE).ok_or_else(|| HbError::InvalidEvent("missing node tag".into()))?;
    let node_key: [u8; 32] = ::hex::decode(&node_hex)
        .map_err(|_| HbError::InvalidEvent("node key is not valid hex".into()))?
        .try_into()
        .map_err(|_| HbError::InvalidEvent("node key is not 32 bytes".into()))?;

    Ok(Binding {
        npub: event.pubkey,
        node_key,
        addrs: tag_vals(event, TAG_ADDRS),
        created_at: created,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: u64 = 30 * 60;

    fn node() -> [u8; 32] {
        rand::random()
    }

    fn fresh() -> (Identity, [u8; 32], Event, u64) {
        let id = Identity::generate();
        let nk = node();
        let now = 1_700_000_000u64;
        let ev = build_binding(&id, &nk, &["addr-a".into(), "addr-b".into()], now, TTL).unwrap();
        (id, nk, ev, now)
    }

    #[test]
    fn valid_binding_recovers_node_key() {
        let (id, nk, ev, now) = fresh();
        let b = verify_binding(&ev, &id.public_key(), now).unwrap();
        assert_eq!(b.node_key, nk);
        assert_eq!(b.addrs, vec!["addr-a".to_string(), "addr-b".to_string()]);
        assert_eq!(b.npub, id.public_key());
    }

    #[test]
    fn binding_by_wrong_npub_rejected() {
        // ID3/AB4: a *validly-signed* binding authored by B is rejected when we expect A.
        let (_id, _nk, ev, now) = fresh();
        let other = Identity::generate();
        assert!(matches!(verify_binding(&ev, &other.public_key(), now), Err(HbError::WrongSigner)));
    }

    #[test]
    fn swapped_node_key_rejected() {
        // A relay swaps the node tag but reuses the signature → id mismatch → rejected.
        let (id, _nk, ev, now) = fresh();
        let impostor = node();
        let forged = Event::new(
            ev.id,
            id.public_key(),
            ev.created_at,
            Kind::from_u16(KIND_PRESENCE),
            [
                Tag::custom(TagKind::custom(TAG_NODE), [hex::encode(impostor)]),
                Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                Tag::custom(TagKind::custom(TAG_EXPIRES), [(now + TTL).to_string()]),
            ],
            "",
            ev.sig,
        );
        assert!(verify_binding(&forged, &id.public_key(), now).is_err());
    }

    #[test]
    fn expired_binding_rejected() {
        let (id, _nk, ev, now) = fresh();
        assert!(matches!(
            verify_binding(&ev, &id.public_key(), now + TTL + 1),
            Err(HbError::BindingExpired)
        ));
    }

    #[test]
    fn future_dated_binding_rejected() {
        let (id, _nk, ev, now) = fresh();
        let before = now.saturating_sub(FUTURE_SKEW_SECS + 60);
        assert!(matches!(
            verify_binding(&ev, &id.public_key(), before),
            Err(HbError::BindingNotYetValid)
        ));
    }

    #[test]
    fn wrong_kind_rejected() {
        // A non-presence event carrying binding-shaped tags must not pass as a binding
        // (event-confusion guard).
        let id = Identity::generate();
        let now = 1_700_000_000u64;
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(30_117), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_NODE), [hex::encode(node())]),
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(TagKind::custom(TAG_EXPIRES), [(now + TTL).to_string()]),
                    ])
                    .custom_created_at(Timestamp::from(now)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), now), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn excessive_ttl_rejected_on_build() {
        let id = Identity::generate();
        assert!(build_binding(&id, &node(), &[], 1_700_000_000, MAX_BINDING_TTL_SECS + 1).is_err());
    }

    #[test]
    fn oversized_window_rejected_on_verify() {
        // A binding hand-built with a window beyond the max is refused even though it is
        // validly signed and unexpired.
        let id = Identity::generate();
        let now = 1_700_000_000u64;
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_NODE), [hex::encode(node())]),
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(
                            TagKind::custom(TAG_EXPIRES),
                            [(now + MAX_BINDING_TTL_SECS + 10).to_string()],
                        ),
                    ])
                    .custom_created_at(Timestamp::from(now)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), now), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn malformed_expires_rejected() {
        let id = Identity::generate();
        let now = 1_700_000_000u64;
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_NODE), [hex::encode(node())]),
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(TagKind::custom(TAG_EXPIRES), ["soon".to_string()]),
                    ])
                    .custom_created_at(Timestamp::from(now)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), now), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn missing_node_tag_rejected() {
        // A presence event with no node tag is not a usable binding.
        let id = Identity::generate();
        let now = 1_700_000_000u64;
        let ev = id
            .sign(
                EventBuilder::new(Kind::from_u16(KIND_PRESENCE), "")
                    .tags([
                        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                        Tag::custom(TagKind::custom(TAG_EXPIRES), [(now + TTL).to_string()]),
                    ])
                    .custom_created_at(Timestamp::from(now)),
            )
            .unwrap();
        assert!(matches!(verify_binding(&ev, &id.public_key(), now), Err(HbError::InvalidEvent(_))));
    }
}
