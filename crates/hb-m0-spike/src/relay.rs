//! Leg 4 — a stock Nostr relay (strfry) accepting our custom kinds + a fresh key.
//!
//! The pivot replaces the self-run `hb-relay` with off-the-shelf Nostr relays, so the
//! open question (spec §Open Questions; "biggest unknowns to watch", Path-to-v1.0) is:
//! will a generic relay actually *store and return* our custom kinds (presence `1xxxx`,
//! listing `30xxx`) published by a brand-new key, rather than spam-gate or drop them?
//!
//! This leg publishes one of each kind from a fresh identity, then fetches them back by
//! filter and re-verifies the crypto end-to-end (binding verifies, listing decrypts).
//! A successful round-trip *is* the proof: the relay accepted and persisted both.
//!
//! Network-gated: provide a relay URL (e.g. `ws://127.0.0.1:7777`). The binary skips
//! this leg when `HB_M0_RELAY` is unset, so the offline legs always run.

use std::time::Duration;

use anyhow::{ensure, Context, Result};
use nostr_sdk::prelude::*;

/// Outcome of the relay round-trip, surfaced in the binary's report.
pub struct RelayProof {
    pub relay: String,
    pub fetched: usize,
    pub binding_ok: bool,
    pub listing_ok: bool,
}

/// Publish a presence + a listing event to `relay_url`, fetch them back, and re-verify.
pub async fn run(relay_url: &str) -> Result<RelayProof> {
    let keys = Keys::generate();
    let node = iroh::SecretKey::generate().public();
    let browse_key: [u8; 32] = rand::random();
    let slug = "p2p-it-films";
    let listing_json =
        r#"{"slug":"p2p-it-films","content_types":["video"],"files":[{"name":"sample.mkv","bytes":734003200}]}"#;

    // Build our two custom-kind events from a fresh identity.
    let presence = crate::binding::build_binding(&keys, &node)?;
    let ciphertext = crate::listing::encrypt_listing(&browse_key, listing_json)?;
    let listing = EventBuilder::new(Kind::from_u16(crate::KIND_LISTING), hex::encode(&ciphertext))
        .tag(Tag::identifier(slug)) // d=slug -> addressable/replaceable per snapshot
        .sign_with_keys(&keys)?;

    // Connect. `connect()` returns before the websocket handshake finishes, so use
    // `try_connect`, which waits and reports which relays actually came up.
    let client = Client::builder().signer(keys.clone()).build();
    client
        .add_relay(relay_url)
        .await
        .with_context(|| format!("add_relay({relay_url})"))?;
    let conn = client.try_connect(Duration::from_secs(10)).await;
    ensure!(
        !conn.success.is_empty(),
        "could not connect to relay {relay_url}: {:?}",
        conn.failed
    );

    // Publish. send_event surfaces a hard transport error here; relay-level rejection
    // (OK: false) is caught by the fetch-back below returning nothing.
    publish(&client, &presence, "presence").await?;
    publish(&client, &listing, "listing").await?;

    // Read both kinds back, authored by our fresh key.
    let filter = Filter::new().author(keys.public_key()).kinds([
        Kind::from_u16(crate::KIND_PRESENCE),
        Kind::from_u16(crate::KIND_LISTING),
    ]);
    let events = client.fetch_events(filter, Duration::from_secs(10)).await?;
    let _ = client.disconnect().await;

    // Re-verify what the relay handed back.
    let mut binding_ok = false;
    let mut listing_ok = false;
    for ev in events.iter() {
        if ev.kind == Kind::from_u16(crate::KIND_PRESENCE) {
            if let Ok(recovered) = crate::binding::verify_binding(ev, Timestamp::now().as_u64()) {
                binding_ok = recovered == node;
            }
        } else if ev.kind == Kind::from_u16(crate::KIND_LISTING) {
            if let Ok(ct) = hex::decode(ev.content.as_bytes()) {
                if let Ok(plain) = crate::listing::decrypt_listing(&browse_key, &ct) {
                    listing_ok = plain == listing_json;
                }
            }
        }
    }

    Ok(RelayProof {
        relay: relay_url.to_string(),
        fetched: events.len(),
        binding_ok,
        listing_ok,
    })
}

async fn publish(client: &Client, event: &Event, label: &str) -> Result<()> {
    let output = client
        .send_event(event)
        .await
        .with_context(|| format!("relay refused our {label} kind ({})", event.kind.as_u16()))?;
    ensure!(
        !output.success.is_empty(),
        "no relay accepted our {label} kind ({}); failures: {:?}",
        event.kind.as_u16(),
        output.failed
    );
    Ok(())
}
