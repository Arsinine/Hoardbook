//! Leg 2 — the `npub` -> iroh-node binding (the v0.9 replacement for the shipped
//! `hb_id == conn.remote_id()` equality; spec §File Sharing H2/H17, §P2P Layer).
//!
//! The binding is *"npub X authorises iroh node Y, as of time T"*. We model it as a
//! signed Nostr **presence event** (`KIND_PRESENCE`) that carries the iroh node key
//! (an Ed25519 `EndpointId`) as a tag. Because NIP-01 signs the event id — a hash over
//! `(pubkey, created_at, kind, tags, content)` — the Schnorr signature covers the node
//! key *and* the timestamp. So `event.verify()` proves the `npub` really vouched for
//! exactly that node key at exactly that time; tampering with either breaks the id.
//!
//! This is the honest cross-system proof: secp256k1 identity (nostr) vouching for an
//! Ed25519 transport key (iroh), the two key systems the pivot must bridge.

use anyhow::{ensure, Result};
use nostr_sdk::prelude::*;

/// Tag carrying the bound iroh endpoint id (hex of the 32-byte Ed25519 node key).
const TAG_IROH_NODE: &str = "hb-iroh-node";
/// Tag carrying the binding schema version — a versioned, signed sub-object so the
/// transport credential can evolve without touching identity (spec §Schema versioning).
const TAG_BINDING_VERSION: &str = "hb-binding-v";
const BINDING_VERSION: &str = "1";

/// How stale a presence/binding event may be before a downloader rejects it. The spec
/// refreshes presence ~every 5 min; we allow a generous multiple for clock skew.
pub const MAX_BINDING_AGE_SECS: u64 = 30 * 60;
/// Tolerance for a timestamp slightly in the future (clock skew), matching the shipped
/// design's ±300 s window.
const FUTURE_SKEW_SECS: u64 = 300;

/// Build a signed presence event binding `iroh_node` under `keys`' `npub`.
pub fn build_binding(keys: &Keys, iroh_node: &iroh::PublicKey) -> Result<Event> {
    let node_hex = hex::encode(iroh_node.as_bytes());
    let event = EventBuilder::new(Kind::from_u16(crate::KIND_PRESENCE), "")
        .tags([
            Tag::custom(TagKind::custom(TAG_IROH_NODE), [node_hex]),
            Tag::custom(TagKind::custom(TAG_BINDING_VERSION), [BINDING_VERSION.to_string()]),
        ])
        .sign_with_keys(keys)?;
    Ok(event)
}

/// Verify a presence event's binding *as of `now` (unix secs)* and return the iroh node
/// key the author vouched for. A downloader runs this before dialing, so a lying relay
/// cannot redirect a transfer to an impostor node (H2): the address only resolves if the
/// target `npub` signed for it.
pub fn verify_binding(event: &Event, now: u64) -> Result<iroh::PublicKey> {
    // (1) Schnorr signature + id integrity: the npub authored exactly these tags.
    event.verify()?;

    // (2) Freshness: stale presence is treated as offline / replay-suspect.
    let created = event.created_at.as_u64();
    ensure!(created <= now + FUTURE_SKEW_SECS, "binding timestamp is in the future");
    ensure!(
        now.saturating_sub(created) <= MAX_BINDING_AGE_SECS,
        "binding is stale (>{MAX_BINDING_AGE_SECS}s old)"
    );

    // (3) Extract and parse the bound iroh node key.
    let node_hex = event
        .tags
        .find(TagKind::custom(TAG_IROH_NODE))
        .and_then(|t| t.content())
        .ok_or_else(|| anyhow::anyhow!("presence event has no {TAG_IROH_NODE} tag"))?;
    let bytes: [u8; 32] = hex::decode(node_hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("iroh node id is not 32 bytes"))?;
    Ok(iroh::PublicKey::from_bytes(&bytes)?)
}

/// Human-readable proof line for the binary's report.
pub fn demo() -> Result<String> {
    let keys = Keys::generate();
    let node = iroh::SecretKey::generate().public();
    let event = build_binding(&keys, &node)?;
    let recovered = verify_binding(&event, Timestamp::now().as_u64())?;
    ensure!(recovered == node, "round-trip node-key mismatch");
    Ok(format!(
        "{}  authorises iroh node  {}",
        keys.public_key().to_bech32()?,
        node
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (Keys, iroh::PublicKey, Event, u64) {
        let keys = Keys::generate();
        let node = iroh::SecretKey::generate().public();
        let event = build_binding(&keys, &node).unwrap();
        let now = event.created_at.as_u64();
        (keys, node, event, now)
    }

    #[test]
    fn valid_binding_recovers_the_node_key() {
        let (_keys, node, event, now) = fresh();
        let recovered = verify_binding(&event, now).unwrap();
        assert_eq!(recovered, node, "verify must return the exact bound node key");
    }

    #[test]
    fn tampered_node_key_is_rejected() {
        let (keys, _node, event, now) = fresh();
        // Re-issue the event with a *different* node key but reuse the original signature.
        let impostor = iroh::SecretKey::generate().public();
        let forged = Event::new(
            event.id,
            keys.public_key(),
            event.created_at,
            Kind::from_u16(crate::KIND_PRESENCE),
            [Tag::custom(TagKind::custom(TAG_IROH_NODE), [hex::encode(impostor.as_bytes())])],
            "",
            event.sig,
        );
        // The id no longer matches the (now-different) tags, so verification fails:
        // a relay cannot swap in an impostor node key without re-signing as the npub.
        assert!(verify_binding(&forged, now).is_err(), "swapped node key must be rejected");
    }

    #[test]
    fn stale_binding_is_rejected() {
        let (_keys, _node, event, now) = fresh();
        let way_later = now + MAX_BINDING_AGE_SECS + 60;
        assert!(verify_binding(&event, way_later).is_err(), "expired binding must be rejected");
    }

    #[test]
    fn future_dated_binding_is_rejected() {
        let (_keys, _node, event, now) = fresh();
        let before = now.saturating_sub(FUTURE_SKEW_SECS + 60);
        assert!(verify_binding(&event, before).is_err(), "future-dated binding must be rejected");
    }
}
