//! NIP-17 gift-wrapped direct messages (spec §Direct Messages, §DM metadata).
//!
//! A DM is sealed to the recipient and wrapped under a **per-message ephemeral key**, so the
//! relay cannot attribute the wrap to the real sender, and the wrap's timestamp is randomised
//! within a window. The seal/wrap (NIP-59) is done by `nostr`'s vetted implementation — never
//! hand-rolled — over hb-core's NIP-44 layer. The real sender is recovered only by the
//! recipient, from inside the verified seal.

use hb_core::Identity;
use nostr::nips::nip59;
use nostr::prelude::*;

use crate::error::NetError;

/// A DM after unwrapping: the *real* sender (recovered from the seal, not the ephemeral wrap),
/// the plaintext, the inner rumor's `created_at` (the true send time — the outer gift-wrap
/// timestamp is randomised by NIP-59, so it must never be used for ordering), and the inner
/// rumor's **kind**. The outer wrap is always 1059, but what is sealed inside is not necessarily
/// chat: Hoardbook gift-wraps kind-31_113 private listings (`hb_core::KIND_PRIV_LISTING`) and
/// kind-31_114 key grants into the very same p-tagged-1059 inbox shape. A consumer that renders
/// DMs MUST branch on `kind` (QURATOR-187) — only [`Kind::PrivateDirectMessage`] (14) is a chat
/// message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMessage {
    pub sender: PublicKey,
    pub content: String,
    /// Unix seconds from the inner rumor — the real send time (kind 14 for chat).
    pub created_at: u64,
    /// The inner rumor's kind (QURATOR-187): 14 = chat; anything else is a non-chat payload that
    /// arrived in the same 1059 inbox (e.g. a private listing or key grant).
    pub kind: Kind,
}

/// Gift-wrap `message` from `identity` to `recipient` (NIP-17). The returned event (kind 1059)
/// is signed by a fresh ephemeral key, addressed only to `recipient`.
pub async fn wrap_dm(
    identity: &Identity,
    recipient: &PublicKey,
    message: &str,
) -> Result<Event, NetError> {
    EventBuilder::private_msg(identity.keys(), *recipient, message, [])
        .await
        .map_err(|e| NetError::Client(e.to_string()))
}

/// Unwrap a gift-wrapped DM addressed to `identity`. Verifies the inner seal and returns the
/// real sender + plaintext. A wrap not addressed to us (wrong recipient), tampered, or malformed
/// fails cleanly with [`NetError::DmUnwrap`] — never a panic.
pub async fn unwrap_dm(identity: &Identity, gift_wrap: &Event) -> Result<DirectMessage, NetError> {
    let unwrapped = nip59::extract_rumor(identity.keys(), gift_wrap)
        .await
        .map_err(|e| NetError::DmUnwrap(e.to_string()))?;
    Ok(DirectMessage {
        sender: unwrapped.sender,
        content: unwrapped.rumor.content,
        created_at: unwrapped.rumor.created_at.as_secs(),
        kind: unwrapped.rumor.kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nip17_wrap_unwrap_roundtrip() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = wrap_dm(&alice, &bob.public_key(), "back room is open").await.unwrap();
        let dm = unwrap_dm(&bob, &wrap).await.unwrap();
        assert_eq!(dm.content, "back room is open");
        assert_eq!(dm.sender, alice.public_key(), "the real sender is recovered from the seal");
    }

    #[tokio::test]
    async fn nip17_outer_signed_by_ephemeral_not_sender() {
        // DM2: the relay sees the wrap signed by an ephemeral key — never the sender's npub.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = wrap_dm(&alice, &bob.public_key(), "hi").await.unwrap();
        assert_ne!(wrap.pubkey, alice.public_key(), "wrap must not be signed by the sender");
        assert_ne!(wrap.pubkey, bob.public_key(), "nor by the recipient");
        assert_eq!(wrap.kind, Kind::GiftWrap);
    }

    #[tokio::test]
    async fn nip17_created_at_is_inner_send_time_not_outer_wrap() {
        // The outer gift wrap's timestamp is randomised into the past by NIP-59; the inner rumor
        // carries the real send time. DirectMessage.created_at must reflect the inner time, so it
        // is never older than the (randomised-past) outer wrap timestamp.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let wrap = wrap_dm(&alice, &bob.public_key(), "when?").await.unwrap();
        let dm = unwrap_dm(&bob, &wrap).await.unwrap();
        assert!(
            dm.created_at >= wrap.created_at.as_secs(),
            "inner send time {} must not predate the randomised-past wrap time {}",
            dm.created_at,
            wrap.created_at.as_secs()
        );
    }

    #[tokio::test]
    async fn nip17_wrong_recipient_cannot_unwrap() {
        // DM3: a DM addressed to Bob does not decrypt for Carol.
        let alice = Identity::generate();
        let bob = Identity::generate();
        let carol = Identity::generate();
        let wrap = wrap_dm(&alice, &bob.public_key(), "secret").await.unwrap();
        assert!(matches!(unwrap_dm(&carol, &wrap).await, Err(NetError::DmUnwrap(_))));
    }

    #[tokio::test]
    async fn unwrap_dm_reports_the_inner_rumor_kind() {
        // QURATOR-187: a 1059 gift wrap is an envelope for ANY NIP-59 rumor, not only chat. The
        // relay filter can only see the outer kind, so `unwrap_dm` must surface the INNER kind for
        // consumers to branch on — a kind-31_113 private listing (built here by the production
        // `seal_private_listing`) unwraps "successfully" exactly like a chat DM.
        //
        // MUTATION (P-10, orchestrator applies): in `unwrap_dm`, set `kind: Kind::PrivateDirectMessage`
        // (hardcode it, discarding `unwrapped.rumor.kind`) — this test must then FAIL on the
        // KIND_PRIV_LISTING assert below.
        let author = Identity::generate();
        let me = Identity::generate();
        let listing = hb_core::seal_private_listing(
            &author,
            &[me.public_key()],
            r#"{"slug":"vault","entries":[]}"#,
            1_700_000_000,
        )
        .unwrap();
        let dm = unwrap_dm(&me, &listing[0]).await.unwrap();
        assert_eq!(
            dm.kind.as_u16(),
            hb_core::KIND_PRIV_LISTING,
            "the inner kind is carried through, not discarded — this is the chat/private dispatch key"
        );

        // And a real chat wrap still reports kind 14.
        let chat = wrap_dm(&author, &me.public_key(), "hi").await.unwrap();
        let chat_dm = unwrap_dm(&me, &chat).await.unwrap();
        assert_eq!(chat_dm.kind, Kind::PrivateDirectMessage);
    }
}
