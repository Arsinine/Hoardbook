//! Direct messages over NIP-17 (spec §Direct Messages).
//!
//! M4 cutover: the legacy signed-envelope DM + JCS-AAD + iroh-direct/relay
//! store-and-forward path is gone. A DM is now a NIP-17 gift wrap (`hb-net::wrap_dm`) published
//! to the configured relays; the inbox fetches kind-1059 wraps addressed to us and unwraps them
//! (`hb-net::unwrap_dm`), recovering the **real sender npub** from inside the seal. The legacy
//! DM history is intentionally **not** carried forward (decided break — pre-launch zero-user).
//!
//! `send_dm_inner` / `fetch_dms_inner` are the Tauri-free seam (mirroring `download_file_inner`);
//! the pure decode logic (`decode_dms`) is L1-tested without a relay (the wire is proven by
//! `hb-it` Suite DM).

use std::collections::HashSet;

use chrono::{TimeZone, Utc};
use nostr::prelude::*;
use serde::Serialize;
use tauri::State;

use hb_net::{unwrap_dm, wrap_dm, RelayClient};

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net,
    store::DataStore,
};

/// A decoded, sender-attributed chat message returned to the frontend. The sender is the **real**
/// npub recovered from the NIP-17 seal — never the ephemeral wrap key.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// Real sender npub (bech32).
    pub from: String,
    /// Recipient npub (bech32) — us for inbound, the peer for our sent echo.
    pub to: String,
    pub content: String,
    /// RFC3339 timestamp from the inner rumor (the real send time).
    pub sent_at: String,
}

/// Parse a DM recipient from a pasted npub or full `hbk` share code → its public key.
fn parse_recipient(s: &str) -> Result<PublicKey, String> {
    hb_core::ShareCode::parse(s)
        .map(|sc| sc.pubkey())
        .map_err(|e| format!("Invalid recipient: {e}"))
}

fn npub_of(pk: &PublicKey) -> String {
    pk.to_bech32().unwrap_or_else(|_| pk.to_hex())
}

fn rfc3339_of(unix_secs: u64) -> String {
    Utc.timestamp_opt(unix_secs as i64, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

// ---------------------------------------------------------------------------
// The Tauri-free seam (composes hb-net::wrap_dm / unwrap_dm over a RelayClient)
// ---------------------------------------------------------------------------

/// Build the NIP-17 gift wrap for `content` from `identity` to `recipient` (no I/O). Thin alias
/// over `hb-net::wrap_dm`, named for the seam + its L1 conformance tests.
pub(crate) async fn build_dm(
    identity: &hb_core::Identity,
    recipient: &PublicKey,
    content: &str,
) -> Result<Event, hb_net::NetError> {
    wrap_dm(identity, recipient, content).await
}

/// Send a DM: build the gift wrap and publish it to the connected relays. Returns the wrap.
pub(crate) async fn send_dm_inner(
    client: &RelayClient,
    identity: &hb_core::Identity,
    recipient: &PublicKey,
    content: &str,
) -> Result<Event, hb_net::NetError> {
    let wrap = build_dm(identity, recipient, content).await?;
    client.publish(&wrap).await?;
    Ok(wrap)
}

/// Decode a batch of gift-wrap events into sender-attributed messages (pure; no relay). A wrap not
/// addressed to us, tampered, or malformed is **skipped with a log, never a panic**. When
/// `contact_npubs` is `Some`, messages from npubs outside the set are dropped (the `allow_dms` off
/// case). Result is sorted oldest-first by send time.
pub(crate) async fn decode_dms(
    own_npub: &str,
    identity: &hb_core::Identity,
    gift_wraps: Vec<Event>,
    contact_npubs: Option<&HashSet<String>>,
) -> Vec<ReceivedMessage> {
    let mut out: Vec<ReceivedMessage> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (from, sent_at) dedup
    for wrap in gift_wraps {
        match unwrap_dm(identity, &wrap).await {
            Ok(dm) => {
                let from = npub_of(&dm.sender);
                if contact_npubs.is_some_and(|ids| !ids.contains(&from)) {
                    continue;
                }
                let sent_at = rfc3339_of(dm.created_at);
                if !seen.insert((from.clone(), sent_at.clone())) {
                    continue;
                }
                out.push(ReceivedMessage {
                    from,
                    to: own_npub.to_string(),
                    content: dm.content,
                    sent_at,
                });
            }
            Err(e) => tracing::debug!("skipping undecryptable/foreign gift wrap: {e}"),
        }
    }
    out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    out
}

/// Fetch + decode the NIP-17 inbox: gift wraps (kind 1059) addressed to us, unwrapped.
pub(crate) async fn fetch_dms_inner(
    client: &RelayClient,
    identity: &hb_core::Identity,
    own_npub: &str,
    contact_npubs: Option<&HashSet<String>>,
    timeout: std::time::Duration,
) -> Result<Vec<ReceivedMessage>, hb_net::NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(identity.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    Ok(decode_dms(own_npub, identity, wraps, contact_npubs).await)
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Encrypt + send a chat message to `to` (an npub or full share code) over NIP-17.
#[tauri::command]
pub async fn send_message(
    to: String,
    content: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<ReceivedMessage> {
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Err("Message cannot be empty".into());
    }
    if trimmed.len() > 4096 {
        return Err(format!("Message too long ({} chars, max 4096)", trimmed.len()));
    }

    let recipient = parse_recipient(&to)?;

    let (from, id_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.npub(), id.identity.clone())
    };

    let client = net::connect(&id_clone, &store).await.map_err(cmd_err)?;
    let result = send_dm_inner(&client, &id_clone, &recipient, &trimmed).await;
    client.disconnect().await;
    result.map_err(cmd_err)?;

    Ok(ReceivedMessage {
        from,
        to: npub_of(&recipient),
        content: trimmed,
        sent_at: Utc::now().to_rfc3339(),
    })
}

/// Fetch + decrypt the NIP-17 inbox. Respects `allow_dms`: when off, only contacts' messages.
#[tauri::command]
pub async fn get_messages(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<ReceivedMessage>> {
    let (own_npub, id_clone) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded.")?;
        (id.npub(), id.identity.clone())
    };

    let allow_dms = store.load_settings().map_err(cmd_err)?.map(|s| s.allow_dms).unwrap_or(true);
    let contact_npubs: Option<HashSet<String>> = if allow_dms {
        None
    } else {
        Some(store.list_contacts().map_err(cmd_err)?.into_iter().map(|c| c.npub).collect())
    };

    let client = net::connect(&id_clone, &store).await.map_err(cmd_err)?;
    let result =
        fetch_dms_inner(&client, &id_clone, &own_npub, contact_npubs.as_ref(), net::RELAY_TIMEOUT)
            .await;
    client.disconnect().await;
    result.map_err(cmd_err)
}

// ---------------------------------------------------------------------------
// Tests — the DM seam (L1, no relay; the wire is proven by hb-it Suite DM)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::Identity;

    #[tokio::test]
    async fn send_dm_inner_produces_a_nip17_giftwrap() {
        // build_dm (the no-I/O half of send_dm_inner) yields a kind-1059 gift wrap signed by an
        // ephemeral key — never the sender's npub (DM2).
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = build_dm(&alice, &bob.public_key(), "back room is open").await.unwrap();
        assert_eq!(wrap.kind, Kind::GiftWrap, "DM wrap must be kind 1059");
        assert_ne!(wrap.pubkey, alice.public_key(), "wrap must not be signed by the sender");
    }

    #[tokio::test]
    async fn send_dm_inner_inner_rumor_is_kind_14() {
        // NIP-17 conformance: the sealed inner rumor is an unsigned kind-14 (PrivateDirectMessage)
        // event. A round-trip test alone could pass on a non-conformant inner event a real NIP-17
        // peer would reject. The recovered sender is the real npub, not the ephemeral wrap key.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = build_dm(&alice, &bob.public_key(), "hi").await.unwrap();
        let unwrapped = nostr::nips::nip59::extract_rumor(bob.keys(), &wrap).await.unwrap();
        assert_eq!(
            unwrapped.rumor.kind,
            Kind::PrivateDirectMessage,
            "inner rumor must be kind 14 (private direct message)"
        );
        assert_eq!(unwrapped.sender, alice.public_key(), "rumor sender is the real npub");
    }

    #[tokio::test]
    async fn fetch_dms_inner_unwraps_to_sender_and_plaintext() {
        // decode_dms recovers the REAL sender npub + plaintext from the seal.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = build_dm(&alice, &bob.public_key(), "secret tape list").await.unwrap();
        let msgs = decode_dms(&bob.npub(), &bob, vec![wrap], None).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from, alice.npub(), "from is the real sender npub");
        assert_eq!(msgs[0].to, bob.npub());
        assert_eq!(msgs[0].content, "secret tape list");
    }

    #[tokio::test]
    async fn fetch_dms_inner_rejects_malformed_giftwrap_not_panicked() {
        // A corrupt/foreign gift wrap from a hostile relay → skipped with a reason, never a panic.
        let alice = Identity::generate();
        let bob = Identity::generate();
        // A plain text note is not a gift wrap addressed to bob.
        let garbage = alice.sign(EventBuilder::new(Kind::TextNote, "not a wrap")).unwrap();
        let real = build_dm(&alice, &bob.public_key(), "real").await.unwrap();
        let msgs = decode_dms(&bob.npub(), &bob, vec![garbage, real], None).await;
        assert_eq!(msgs.len(), 1, "only the real DM decodes; the garbage is skipped");
        assert_eq!(msgs[0].content, "real");
    }

    #[tokio::test]
    async fn decode_dms_honours_contact_allow_list() {
        // allow_dms off: a stranger's DM is filtered out; a contact's is kept.
        let me = Identity::generate();
        let contact = Identity::generate();
        let stranger = Identity::generate();
        let from_contact = build_dm(&contact, &me.public_key(), "hey").await.unwrap();
        let from_stranger = build_dm(&stranger, &me.public_key(), "spam").await.unwrap();
        let allow: HashSet<String> = [contact.npub()].into_iter().collect();
        let msgs =
            decode_dms(&me.npub(), &me, vec![from_contact, from_stranger], Some(&allow)).await;
        assert_eq!(msgs.len(), 1, "only the contact's DM survives the allow-list");
        assert_eq!(msgs[0].from, contact.npub());
    }

    #[test]
    fn dm_path_no_longer_builds_a_signed_envelope() {
        // The legacy DM payload is gone: ReceivedMessage carries only npub-attributed fields, with
        // no `encrypted` flag and no JCS-AAD concept. Asserted by the serialized shape.
        let msg = ReceivedMessage {
            from: "npub1from".into(),
            to: "npub1to".into(),
            content: "x".into(),
            sent_at: "2026-06-17T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("encrypted"), "no legacy `encrypted` flag");
        assert!(json.contains("\"from\":\"npub1from\""));
    }
}
