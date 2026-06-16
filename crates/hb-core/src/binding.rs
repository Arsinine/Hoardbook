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
use crate::version::{check_schema, SCHEMA_V};

/// Presence + binding event kind (replaceable, 1xxxx range — newest per author wins).
pub const KIND_PRESENCE: u16 = 11_111;

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
    // (2) Author pin — the true wrong-signer gate. A *valid* binding from a different
    //     identity is rejected (H2: a relay can't redirect to a vouched-by-someone-else node).
    if &event.pubkey != expected {
        return Err(HbError::WrongSigner);
    }
    // (3) Schema version recognised (forward-compat).
    let schema = tag_val(event, TAG_SCHEMA)
        .and_then(|s| s.parse::<u8>().ok())
        .ok_or_else(|| HbError::InvalidEvent("missing schema version".into()))?;
    check_schema(schema)?;
    // (4) Validity window — explicit expiry, not an implicit freshness guess.
    let created = event.created_at.as_u64();
    if created > now.saturating_add(FUTURE_SKEW_SECS) {
        return Err(HbError::BindingNotYetValid);
    }
    let expires_at = tag_val(event, TAG_EXPIRES)
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| HbError::InvalidEvent("missing expires_at".into()))?;
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

fn tag_val(event: &Event, name: &str) -> Option<String> {
    event
        .tags
        .find(TagKind::custom(name))
        .and_then(|t| t.content())
        .map(str::to_string)
}

fn tag_vals(event: &Event, name: &str) -> Vec<String> {
    event
        .tags
        .find(TagKind::custom(name))
        .map(|t| t.as_slice().iter().skip(1).cloned().collect())
        .unwrap_or_default()
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
