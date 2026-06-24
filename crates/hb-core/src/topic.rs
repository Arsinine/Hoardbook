//! Topics (M11; spec §11 — opt-in subject groups) — a symmetric-topic-key room with a **durable
//! members-only roster** + an **ephemeral 24h channel**, plus a sealed single-use **invite
//! credential**. This is Hoardbook's **one deliberate "Not a tracker" exception**: joining *is*
//! consenting to be visible to fellow members, so the privacy posture is implemented as honestly as
//! it is specced.
//!
//! **The crypto contract (M11 Decisions A–F):**
//! ```text
//!   topic_key : random 32 bytes — the room's symmetric key (NIP-44 symmetric, domain-separated HKDF).
//!   topic_id  : a stable public id — hash(name) for a public Topic (so the same name = the same room),
//!               or a random 32-byte hex for a private Topic. NEVER the key.
//!
//!   ANNOUNCE  (public only, KIND_TOPIC_ANNOUNCE, replaceable, SIGNED by the author, PLAINTEXT):
//!       { v, topic_id, name, description, tags, private:false }  — discovery metadata. NO topic_key.
//!       (A private Topic publishes NO announce → unlisted/undiscoverable.)
//!
//!   MEMBERSHIP (KIND_TOPIC_MEMBER, replaceable per (topic_id, member), encrypted under topic_key):
//!       content = TOPIC_ENC(topic_key, 0x01 ‖ {member_npub, joined_at, proof})  // 0x01 = domain byte (F17)
//!       *** B2 — SIGNED under a per-member DERIVED key, NOT the real npub ***
//!         member_sign_key = HMAC-SHA256(topic_key, "member" ‖ member_xonly) → secp256k1 sk
//!       so the Nostr `pubkey` field is a topic-scoped pseudonym (stable per member, replaceable/
//!       retract still work), UN-linkable to the real npub WITHOUT the topic_key. The real npub lives
//!       ONLY inside the topic_key-encrypted content. `d`-tag = topic_id (roster is queryable).
//!       *** chorus-1 fix — PROOF OF PARTICIPATION (real-key authorization) ***
//!         The `member_sign_key` is DERIVABLE BY ANY TOPIC-KEY HOLDER from the (public) target npub, so
//!         the pseudonym-binding check alone repels only NON-members. To stop an insider from enrolling
//!         a person who never joined, the encrypted content also carries `proof` = a NIP-01 event
//!         **signed by the member's REAL key** binding `join:{topic_id}` at `joined_at`; `open_membership`
//!         verifies it against the claimed npub. A key-holder cannot forge it (no real secret key).
//!         **Honest residual:** a key-holder CAN replay a member's *own* old proof to re-add a member who
//!         previously, voluntarily joined *this* topic (resurrect-after-leave) — but cannot fabricate
//!         membership for a never-joiner. The roster is sound against "who never joined", not against a
//!         malicious insider resurrecting a past member. The proof is domain-separated (`hbm:join:…`,
//!         chorus-2) so it can't be a cross-protocol replay of a `KIND_TOPIC_PROOF` event signed elsewhere.
//!         **Transferable-attestation tradeoff (chorus-2 disclosure):** the proof is a real-key signature
//!         over a meaningful statement, so it is **non-repudiable, transferable evidence** that the member
//!         signed "joined this topic" — a fellow key-holder could show it to an outsider to prove your
//!         participation. Within the room this is no new exposure (members already see the roster), but it
//!         trades a little deniability for roster integrity; it matters most for a sensitive *private*
//!         Topic. Treat the proof JSON as topic-key-confidential, never a public certificate.
//!
//!   CHANNEL   (KIND_TOPIC_POST, regular/stored, encrypted under topic_key, NIP-40 expiration now+24h):
//!       content = TOPIC_ENC(topic_key, 0x02 ‖ {author_npub, body, ts})       // 0x02 = domain byte (F17)
//!       signed under the same per-member derived key. Wiped at 24h (relay-honoured, best-effort) AND
//!       filtered locally on the authenticated inner `ts` so a non-compliant relay can't resurrect it.
//!
//!   INVITE CREDENTIAL (private admission path 1 + public-join, sealed, the SAME NIP-59 seal the M10
//!   private listing uses — never re-derived):
//!       seal-to-invitee( {meta, topic_key, nonce, expires_at} )  — gift-wrap (1059) of a seal of a
//!       KIND_TOPIC_INVITE rumor. Single-use (seen-set scoped (topic_id, invitee)), short expiry.
//! ```
//!
//! **Decision A — public-topic key distribution is a PARTICIPATION bar, not a CRYPTO bar.** A public
//! Topic's key is delivered through a **public-join credential**: the SAME sealed invite, but sealed
//! to a **deterministic keypair derived from the topic name** ([`public_join_keys`]) that *any* joiner
//! can reconstruct. **Be honest about how thin this bar is (chorus-1):** the topic NAME is published
//! in the plaintext announce, so **anyone who learns or guesses the name and runs the join flow gets
//! the FULL topic key** — read AND write, past and future, *including* a read-only scraper of the
//! announce that never leaves a membership trace. The name IS the password, and topic names are
//! low-entropy ("anime", "general"). The encryption stops a relay that indexes only event *content*
//! and does not follow the public-join derivation — **not** anyone who parses a kind-31117 announce.
//! Do NOT claim a public roster/channel is confidential against a name-knower. A **private** Topic
//! publishes no announce + no public-join credential: its key is a real crypto bar, delivered only
//! through admission (an invite minted to your npub, or an approving member's reply).
//!
//! **No forward secrecy / post-compromise security (F16, restated).** The topic key is static, so a
//! leaked key retrospectively deanonymizes every pseudonym + decrypts every past post in-window. The
//! pseudonyms are deterministic + linkable per topic. Inherent to symmetric-key group messaging.
//!
//! **Negatives this enforces (tested first, hardest):** an announce never carries the topic_key (B1);
//! a membership event's Nostr `pubkey` is the derived pseudonym, the real npub only in ciphertext (B2);
//! a non-member (no key) cannot open a membership or post (`Err`); a membership ciphertext fed to
//! `open_post` (and vice-versa) is rejected on the domain byte (F17); no topic event carries the
//! author's browse-key/share-code (F13); `redeem_invite` rejects an invite not sealed to me / expired /
//! replayed (E); every malformed/tampered/foreign input is a reasoned `Err`, never a panic.

use std::collections::HashSet;
use std::fmt;

use base64::Engine as _;
use ::hkdf::hmac::{Hmac, Mac};
use ::hkdf::Hkdf;
use nostr::nips::nip44::{self, v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey}};
use nostr::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HbError;
use crate::identity::{parse_npub, verify_event, Identity};
use crate::version::{check_crypto, check_schema, CRYPTO_V, SCHEMA_V};

// ── Provisional kinds (Open Q#3 — hard kind registration deferred) ───────────────────────────────
/// Public-Topic announce — parameterized-replaceable (30xxx), `d` = topic_id, signed, **plaintext**
/// (never the key). **Provisional.**
pub const KIND_TOPIC_ANNOUNCE: u16 = 31_117;
/// Membership — parameterized-replaceable per (topic_id, member-pseudonym), `d` = topic_id, encrypted
/// under the topic key, signed by the **derived pseudonym** (B2). **Provisional.**
pub const KIND_TOPIC_MEMBER: u16 = 31_118;
/// Channel post — a **regular** (relay-stored) kind so a member who comes online reads the last 24h,
/// encrypted under the topic key, carrying a NIP-40 `expiration` tag, signed by the pseudonym.
/// **Provisional.**
pub const KIND_TOPIC_POST: u16 = 1_117;
/// Invite-credential **inner** kind — carried only *inside* the NIP-59 seal (never a top-level event),
/// like `KIND_PRIV_LISTING`. **Provisional.**
pub const KIND_TOPIC_INVITE: u16 = 31_119;
/// Proof-of-participation **inner** kind — a NIP-01 event signed by the member's REAL key, carried only
/// *inside* the topic_key-encrypted membership/post content (never published), so it authenticates the
/// real member without leaking the npub on the wire (chorus-1). **Provisional.**
pub const KIND_TOPIC_PROOF: u16 = 31_120;

const TAG_SCHEMA: &str = "hb-v";
const TAG_CRYPTO: &str = "hb-cv";

/// Domain byte distinguishing a membership ciphertext from a channel-post ciphertext (F17) — the two
/// share the topic conversation key, so the first plaintext byte pins which event type it is.
const MEMBERSHIP_DOMAIN: u8 = 0x01;
const POST_DOMAIN: u8 = 0x02;

/// Channel posts expire 24h after their authenticated `ts` (spec §11; Decision D — relay-honoured
/// **and** locally filtered).
pub const POST_TTL_SECS: u64 = 24 * 60 * 60;

/// A post whose authenticated `ts` is more than this far in the **future** is dropped by `open_post`
/// (chorus-1): a future-dated `ts` would otherwise sail past the 24h-in-the-past filter and stay
/// visible indefinitely. 1h tolerates honest clock skew; beyond that is a pin-forever abuse.
const MAX_FUTURE_SKEW_SECS: u64 = 60 * 60;

const HKDF_SALT_TOPIC: &[u8] = b"hoardbook/topic-key";
const HKDF_SALT_PUBLIC_JOIN: &[u8] = b"hoardbook/topic-public-join";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

// ── Types ────────────────────────────────────────────────────────────────────────────────────────

/// A Topic's symmetric key — the one secret that gates the roster + channel. Serialized as hex (so the
/// app store can persist a joined Topic), `Debug`-redacted (never logged).
#[derive(Clone)]
pub struct TopicKey([u8; 32]);

impl TopicKey {
    /// A fresh random topic key.
    pub fn generate() -> Self {
        Self(rand::random())
    }
    /// Wrap raw bytes (from a redeemed invite or the persisted store).
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }
    /// The raw key bytes (for the symmetric HKDF). Kept crate-light — callers pass the `TopicKey`.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for TopicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TopicKey([REDACTED])")
    }
}

impl Serialize for TopicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for TopicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let hexs = String::deserialize(d)?;
        let bytes = hex::decode(&hexs).map_err(serde::de::Error::custom)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| serde::de::Error::custom("topic key must be 32 bytes"))?;
        Ok(Self(arr))
    }
}

/// Public Topic discovery metadata — exactly what an announce carries (and what an invite echoes). The
/// `topic_id` is public; the **key is never here**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicMeta {
    pub topic_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// True for a private (unlisted, admission-gated) Topic.
    #[serde(default)]
    pub private: bool,
}

/// One decrypted roster entry — the **real** member npub (recovered from ciphertext) + when they joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub member: PublicKey,
    pub joined_at: u64,
}

/// One decrypted channel post — the real author + body + authenticated send time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub author: PublicKey,
    pub body: String,
    pub ts: u64,
}

/// The seen-nonce set: redeemed invites, keyed `(topic_id, invitee)`. Persisted by the app store so a
/// restart can't re-accept an old invite (Decision E). **Honest limit:** device-local — a
/// factory-reset / restore-to-new-device user loses it and could re-redeem an unexpired old invite.
pub type NonceSet = HashSet<String>;

// ── serde payloads (inside ciphertext / inside the seal) ─────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct AnnouncePayload {
    v: u8,
    #[serde(flatten)]
    meta: TopicMeta,
}

#[derive(Serialize, Deserialize)]
struct MemberPayload {
    member_npub: String,
    joined_at: u64,
    /// chorus-1: a NIP-01 proof event (`KIND_TOPIC_PROOF`) signed by the member's REAL key, binding
    /// `join:{topic_id}` at `joined_at`. JSON of the signed event.
    proof: String,
}

#[derive(Serialize, Deserialize)]
struct PostPayload {
    author_npub: String,
    body: String,
    ts: u64,
    /// chorus-1: a NIP-01 proof event signed by the author's REAL key, binding the post's
    /// `post:{topic_id}:{sha256(body)}` at `ts` (so a key-holder cannot impersonate another member).
    proof: String,
}

#[derive(Serialize, Deserialize)]
struct InvitePayload {
    meta: TopicMeta,
    /// hex of the 32-byte topic key.
    topic_key: String,
    nonce: String,
    /// `None` = no expiry; `Some` = a short-lived invite. Checked **independently** of `reusable`.
    expires_at: Option<u64>,
    /// chorus-2: replay policy is **explicit**, not inferred from `expires_at`. `false` (the default,
    /// and every private invite) = single-use (replay-protected); `true` = the reusable public-join
    /// credential (exempt from the seen-set). Decoupling the two means a targeted no-expiry private
    /// invite is still single-use, and a hypothetical reusable-with-expiry stays reusable.
    #[serde(default)]
    reusable: bool,
    schema_v: u8,
    crypto_v: u8,
}

// ── topic-id + topic creation ────────────────────────────────────────────────────────────────────

/// The fixed-root categories a **public** Topic path's first segment must be — the existing
/// content-type enum (M12 W4, Decision K). Validated **client-side**: this is not a registry, not a
/// gatekeeper, not moderated. Below the root, sub-paths are freeform; pollution is made *inert*
/// (content-addressed convergence + activity-ranked discovery + normalization), not prevented.
pub const TOPIC_ROOTS: [&str; 6] = ["video", "audio", "image", "text", "software", "other"];

/// Max segments in a public Topic path (root + 5 sub-segments). A deeper path is rejected so a flood
/// of junk paths can't make the discovery tree unbounded (Decision K + M).
pub const MAX_TOPIC_DEPTH: usize = 6;

/// Normalize a Topic path into canonical segments (M12 W4, Decision K), **in order**: **NFKC**
/// (Unicode compatibility — so a full-width `ｖｉｄｅｏ` or a ligature normalizes to the ASCII form),
/// then **`to_lowercase()` AFTER NFKC**, then split on `/`, trim each segment, and drop empty
/// segments (collapsing `//`, leading/trailing `/`). No depth check here — that is validation. Pure.
fn normalize_path_segments(name: &str) -> Vec<String> {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = name.nfkc().collect();
    let lowered = nfkc.to_lowercase(); // lowercase AFTER NFKC (chorus: order matters)
    lowered
        .split('/')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The canonical normalized path string for a Topic name (segments rejoined with `/`). Used by both
/// [`topic_id_for_name`] and [`public_join_keys`], so a name and its public-join keypair always agree.
fn normalize_name(name: &str) -> String {
    normalize_path_segments(name).join("/")
}

/// The fixed root category of a public Topic name (its first normalized segment) if it is one of
/// [`TOPIC_ROOTS`], else `None` (a non-category root — e.g. `blah/...` or a bare `anime` — is invalid).
pub fn topic_root(name: &str) -> Option<&'static str> {
    let segs = normalize_path_segments(name);
    let first = segs.first()?;
    TOPIC_ROOTS.iter().copied().find(|r| r == first)
}

/// Validate a **public** Topic path (M12 W4, Decision K) — **backend-authoritative** (the UI root
/// picker is a convenience, not the only barrier): ≥1 segment, the root ∈ [`TOPIC_ROOTS`], and depth
/// ≤ [`MAX_TOPIC_DEPTH`]. Private Topics keep freeform names (this is a public-namespace rule only).
pub fn validate_public_name(name: &str) -> Result<(), HbError> {
    let segs = normalize_path_segments(name);
    if segs.is_empty() {
        return Err(HbError::InvalidEvent("a public Topic name cannot be empty".into()));
    }
    if topic_root(name).is_none() {
        return Err(HbError::InvalidEvent(format!(
            "a public Topic's first path segment must be a category ({}); got '{}'",
            TOPIC_ROOTS.join("/"),
            segs[0]
        )));
    }
    if segs.len() > MAX_TOPIC_DEPTH {
        return Err(HbError::InvalidEvent(format!(
            "a public Topic path may be at most {MAX_TOPIC_DEPTH} segments deep; got {}",
            segs.len()
        )));
    }
    Ok(())
}

/// The deterministic `topic_id` for a **public** Topic name — `hex(SHA256("hoardbook/topic-id" ‖
/// normalized_path))`, so two people naming the same public Topic (any case/spacing/extra-slash
/// variant, or a NFKC-equivalent Unicode form) land in the **same room** (name reuse, Decision C/L).
pub fn topic_id_for_name(name: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"hoardbook/topic-id");
    h.update(normalize_name(name).as_bytes());
    hex::encode(h.finalize())
}

/// Mint a new Topic: a fresh random `topic_key` + its `TopicMeta`. A **public** Topic is
/// **validated** (root ∈ category + depth cap — Decision K) and gets a name-derived `topic_id` over
/// its **normalized path** (shared room); its stored `name` is the canonical path. A **private**
/// Topic keeps its freeform name + a random `topic_id` (unguessable, unlisted — the root/depth rules
/// do not apply). The key is random either way — a recreated public Topic reuses the id but gets a
/// **new** key (Decision C), so old-key membership events correctly fail to decrypt against the new room.
pub fn new_topic(
    name: &str,
    description: &str,
    tags: Vec<String>,
    private: bool,
) -> Result<(TopicMeta, TopicKey), HbError> {
    let (topic_id, stored_name) = if private {
        (hex::encode(rand::random::<[u8; 32]>()), name.to_string())
    } else {
        validate_public_name(name)?;
        (topic_id_for_name(name), normalize_name(name))
    };
    let meta = TopicMeta {
        topic_id,
        name: stored_name,
        description: description.to_string(),
        tags,
        private,
    };
    Ok((meta, TopicKey::generate()))
}

// ── symmetric topic crypto (domain-separated from the browse-key + the CEK) ──────────────────────

fn topic_conversation_key(key: &TopicKey, crypto_v: u8) -> ConversationKey {
    let mut info = b"hoardbook/topic-key/v".to_vec();
    info.push(crypto_v);
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_TOPIC), &key.0);
    let mut ck = [0u8; 32];
    hk.expand(&info, &mut ck).expect("32 is a valid HKDF-SHA256 output length");
    ConversationKey::new(ck)
}

/// Encrypt `0x?? ‖ plaintext_json` under the topic key (NIP-44 v2 symmetric, base64). The domain byte
/// is prepended **inside** the ciphertext so membership/post can't be cross-interpreted (F17).
fn topic_encrypt(key: &TopicKey, domain: u8, plaintext_json: &str) -> Result<String, HbError> {
    let mut pt = Vec::with_capacity(1 + plaintext_json.len());
    pt.push(domain);
    pt.extend_from_slice(plaintext_json.as_bytes());
    let ck = topic_conversation_key(key, CRYPTO_V);
    let bytes = encrypt_to_bytes(&ck, &pt).map_err(|e| HbError::Nostr(e.to_string()))?;
    Ok(B64.encode(bytes))
}

/// Decrypt a topic ciphertext, enforcing the expected domain byte. `crypto_v` is the signed `hb-cv`;
/// an unknown version is refused before any decryption (forward-compat); a wrong domain byte is an
/// `Err` (F17), never silently mis-interpreted as the other event type.
fn topic_decrypt(key: &TopicKey, domain_expected: u8, crypto_v: u8, content_b64: &str) -> Result<Vec<u8>, HbError> {
    check_crypto(crypto_v)?;
    let ck = topic_conversation_key(key, crypto_v);
    let raw = B64.decode(content_b64.as_bytes()).map_err(|_| HbError::InvalidEncryptedMessage)?;
    let pt = decrypt_to_bytes(&ck, &raw).map_err(|_| HbError::DecryptionFailed)?;
    let (domain, rest) = pt.split_first().ok_or(HbError::DecryptionFailed)?;
    if *domain != domain_expected {
        return Err(HbError::InvalidEvent(format!(
            "topic domain byte mismatch: expected 0x{domain_expected:02x}, got 0x{:02x}",
            domain
        )));
    }
    Ok(rest.to_vec())
}

// ── B2 — the per-member derived signer (topic-scoped pseudonym) ──────────────────────────────────

/// Derive a member's **topic-scoped pseudonymous signing key** = `HMAC-SHA256(topic_key, "member" ‖
/// member_xonly)` → secp256k1 secret. Stable per (topic, member), un-linkable to the real npub
/// without the topic_key. The signer's public key is the membership/post event's `pubkey` field (B2);
/// nobody without the topic_key can tie it to the real member.
pub fn member_sign_keys(key: &TopicKey, member: &PublicKey) -> Result<Keys, HbError> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key.0).expect("HMAC accepts any key length");
    mac.update(b"member");
    mac.update(&member.to_bytes());
    let out = mac.finalize().into_bytes();
    // A uniform 32-byte HMAC output is a valid secp256k1 scalar with overwhelming probability; the
    // ~2^-128 out-of-range case surfaces as a clean Err (never a panic), not a silent weak key.
    let sk = SecretKey::from_slice(&out).map_err(|e| HbError::Nostr(e.to_string()))?;
    Ok(Keys::new(sk))
}

// ── proof-of-participation (chorus-1: real-key authorization, carried in ciphertext) ─────────────

/// The canonical statement a member's REAL key signs to authorize joining `topic_id`. The `hbm:`
/// prefix is a **Hoardbook-specific domain separator** (chorus-2): without it, a `KIND_TOPIC_PROOF`
/// event Alice ever signs for *any other* app with content `join:x` could be replayed as a Hoardbook
/// membership proof. The prefix binds the proof to this protocol.
fn membership_statement(topic_id: &str) -> String {
    format!("hbm:join:{topic_id}")
}

/// The canonical statement an author's REAL key signs to authorize a post (binds the body so a
/// key-holder cannot reattach the proof to a different message). Domain-separated like the membership
/// statement (chorus-2).
fn post_statement(topic_id: &str, body: &str) -> String {
    format!("hbm:post:{topic_id}:{}", hex::encode(Sha256::digest(body.as_bytes())))
}

/// Build a proof event signed by the member's **real** key over `statement` at `at`. It is never
/// published — it lives inside the topic_key-encrypted content, so the real npub stays off the wire.
fn build_proof(member: &Identity, statement: &str, at: u64) -> Result<Event, HbError> {
    member.sign(
        EventBuilder::new(Kind::from_u16(KIND_TOPIC_PROOF), statement.to_string())
            .custom_created_at(Timestamp::from(at)),
    )
}

/// Verify a proof event: a valid signature by the **claimed** member over exactly `statement` at `at`.
/// A key-holder cannot satisfy this without the member's real secret key.
fn verify_proof(proof_json: &str, expected_member: &PublicKey, statement: &str, at: u64) -> Result<(), HbError> {
    let proof = Event::from_json(proof_json).map_err(|e| HbError::InvalidEvent(e.to_string()))?;
    verify_event(&proof)?;
    if proof.kind != Kind::from_u16(KIND_TOPIC_PROOF) {
        return Err(HbError::InvalidEvent("proof is not a topic proof event".into()));
    }
    if proof.pubkey != *expected_member {
        return Err(HbError::InvalidEvent("proof is not signed by the claimed member's real key".into()));
    }
    if proof.content != statement {
        return Err(HbError::InvalidEvent("proof does not bind the expected statement".into()));
    }
    if proof.created_at.as_u64() != at {
        return Err(HbError::InvalidEvent("proof time does not bind the membership/post time".into()));
    }
    Ok(())
}

// ── ANNOUNCE (public discovery, key-free) ────────────────────────────────────────────────────────

/// Build a signed, **plaintext**, key-free public-Topic announce (kind 31117, `d` = topic_id). The
/// `meta.tags` surface as `t` tags so the Topic is tag-discoverable. **Never embeds the topic_key or
/// any browse-key** (B1/F13). A private Topic must not be announced.
pub fn build_announce(author: &Identity, meta: &TopicMeta, now: u64) -> Result<Event, HbError> {
    if meta.private {
        return Err(HbError::InvalidEvent("a private Topic must not be announced (unlisted)".into()));
    }
    let payload = serde_json::to_string(&AnnouncePayload { v: SCHEMA_V, meta: meta.clone() })?;
    let mut tags = vec![
        Tag::identifier(meta.topic_id.clone()),
        Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
    ];
    for t in &meta.tags {
        tags.push(Tag::hashtag(t));
    }
    author.sign(EventBuilder::new(Kind::from_u16(KIND_TOPIC_ANNOUNCE), payload).tags(tags).custom_created_at(Timestamp::from(now)))
}

/// Verify + parse a public-Topic announce → its `TopicMeta` (with `private` forced false — an
/// announced Topic is public by definition). No key is recoverable here (there is none to recover).
pub fn parse_announce(event: &Event) -> Result<TopicMeta, HbError> {
    verify_event(event)?;
    if event.kind != Kind::from_u16(KIND_TOPIC_ANNOUNCE) {
        return Err(HbError::InvalidEvent(format!(
            "expected announce kind {KIND_TOPIC_ANNOUNCE}, got {}",
            event.kind.as_u16()
        )));
    }
    let payload: AnnouncePayload = serde_json::from_str(&event.content)?;
    check_schema(payload.v)?;
    let mut meta = payload.meta;
    meta.private = false;
    Ok(meta)
}

// ── MEMBERSHIP (durable roster) ──────────────────────────────────────────────────────────────────

/// Seal a membership event for `member` (their own `Identity`, so it can sign the real-key proof) in
/// the Topic — encrypted under the topic key (domain 0x01), signed on the wire by the **derived
/// pseudonym** (B2), `d` = topic_id (replaceable per member). The real npub + the proof live only
/// inside the ciphertext. You only ever seal your **own** membership (you join).
pub fn seal_membership(key: &TopicKey, topic_id: &str, member: &Identity, now: u64) -> Result<Event, HbError> {
    let member_pk = member.public_key();
    let signer = member_sign_keys(key, &member_pk)?;
    let proof = build_proof(member, &membership_statement(topic_id), now)?;
    let payload = serde_json::to_string(&MemberPayload {
        member_npub: member_pk.to_bech32().map_err(|e| HbError::Nostr(e.to_string()))?,
        joined_at: now,
        proof: proof.as_json(),
    })?;
    let content = topic_encrypt(key, MEMBERSHIP_DOMAIN, &payload)?;
    EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), content)
        .tags([
            Tag::identifier(topic_id.to_string()),
            Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
            Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
        ])
        .custom_created_at(Timestamp::from(now))
        .sign_with_keys(&signer)
        .map_err(|e| HbError::Nostr(e.to_string()))
}

/// Open a membership event with the topic key → the real member + join time. Enforces: the event
/// kind, a valid signature (the pseudonym did sign it), the 0x01 domain byte, **and the B2 binding** —
/// `event.pubkey` must equal the pseudonym re-derived from the decrypted npub, so a key-holder cannot
/// forge a roster entry under another member's npub. A non-member (no key) gets `Err` (no decrypt).
pub fn open_membership(key: &TopicKey, event: &Event) -> Result<Membership, HbError> {
    if event.kind != Kind::from_u16(KIND_TOPIC_MEMBER) {
        return Err(HbError::InvalidEvent("not a topic membership event".into()));
    }
    verify_event(event)?;
    let crypto_v = crypto_v_of(event)?;
    let plain = topic_decrypt(key, MEMBERSHIP_DOMAIN, crypto_v, &event.content)?;
    let payload: MemberPayload = serde_json::from_slice(&plain)?;
    let member = parse_npub(&payload.member_npub)?;
    // B2 binding: the signing pseudonym must be the one derived from this member under this key.
    let expected = member_sign_keys(key, &member)?.public_key();
    if event.pubkey != expected {
        return Err(HbError::InvalidEvent(
            "membership pubkey does not bind to the claimed member (B2 forgery guard)".into(),
        ));
    }
    // chorus-1: the real-key proof of participation — only the member's own key could have signed
    // `join:{topic_id}` at `joined_at`, so a key-holder cannot enrol a never-joiner.
    verify_proof(&payload.proof, &member, &membership_statement(&topic_id_of(event)?), payload.joined_at)?;
    Ok(Membership { member, joined_at: payload.joined_at })
}

/// The current roster = the real npubs of every membership event that opens cleanly, de-duplicated.
/// An event under a stale (old-key) membership fails `open_membership` and is excluded (Decision C
/// name-reuse). **Empty ⇒ the Topic is dissolved** (no server deletes it; dissolution is derived).
pub fn roster(key: &TopicKey, memberships: &[Event]) -> Vec<PublicKey> {
    let mut seen = HashSet::new();
    let mut out: Vec<PublicKey> = Vec::new();
    for ev in memberships {
        if let Ok(m) = open_membership(key, ev) {
            if seen.insert(m.member) {
                out.push(m.member);
            }
        }
    }
    out.sort_by_key(|p| p.to_hex());
    out
}

// ── CHANNEL (ephemeral 24h posts) ────────────────────────────────────────────────────────────────

/// Seal a channel post (with the author's own `Identity`, to sign the real-key proof) — encrypted
/// under the topic key (domain 0x02), signed on the wire by the derived pseudonym, carrying a NIP-40
/// `expiration` tag at `now + 24h` (Decision D, relay-honoured best-effort).
pub fn seal_post(key: &TopicKey, topic_id: &str, author: &Identity, body: &str, now: u64) -> Result<Event, HbError> {
    let author_pk = author.public_key();
    let signer = member_sign_keys(key, &author_pk)?;
    let proof = build_proof(author, &post_statement(topic_id, body), now)?;
    let payload = serde_json::to_string(&PostPayload {
        author_npub: author_pk.to_bech32().map_err(|e| HbError::Nostr(e.to_string()))?,
        body: body.to_string(),
        ts: now,
        proof: proof.as_json(),
    })?;
    let content = topic_encrypt(key, POST_DOMAIN, &payload)?;
    EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), content)
        .tags([
            Tag::identifier(topic_id.to_string()),
            Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
            Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
            Tag::expiration(Timestamp::from(now + POST_TTL_SECS)),
        ])
        .custom_created_at(Timestamp::from(now))
        .sign_with_keys(&signer)
        .map_err(|e| HbError::Nostr(e.to_string()))
}

/// Open a channel post → `Some(Post)` if fresh, `None` if older than 24h (the **local** filter, on the
/// authenticated inner `ts` — a non-compliant relay can't resurrect an expired post in the UI). Same
/// signature + domain (0x02) + B2-binding checks as `open_membership`.
pub fn open_post(key: &TopicKey, event: &Event, now: u64) -> Result<Option<Post>, HbError> {
    if event.kind != Kind::from_u16(KIND_TOPIC_POST) {
        return Err(HbError::InvalidEvent("not a topic channel post".into()));
    }
    verify_event(event)?;
    let crypto_v = crypto_v_of(event)?;
    let plain = topic_decrypt(key, POST_DOMAIN, crypto_v, &event.content)?;
    let payload: PostPayload = serde_json::from_slice(&plain)?;
    let author = parse_npub(&payload.author_npub)?;
    let expected = member_sign_keys(key, &author)?.public_key();
    if event.pubkey != expected {
        return Err(HbError::InvalidEvent(
            "post pubkey does not bind to the claimed author (B2 forgery guard)".into(),
        ));
    }
    // chorus-1: the real-key proof binds the topic + the body, so a key-holder cannot impersonate
    // another member or reattach a valid proof to a different message.
    verify_proof(&payload.proof, &author, &post_statement(&topic_id_of(event)?, &payload.body), payload.ts)?;
    // chorus-1: drop a wildly future-dated post (a malicious inner `ts` would otherwise pin it past
    // the 24h-in-the-past filter forever), and apply the 24h local filter (Decision D).
    if payload.ts > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
        return Ok(None);
    }
    if now.saturating_sub(payload.ts) > POST_TTL_SECS {
        return Ok(None);
    }
    Ok(Some(Post { author, body: payload.body, ts: payload.ts }))
}

// ── INVITE CREDENTIAL (sealed, single-use) + public-join ─────────────────────────────────────────

/// The seen-set key for an invite redemption — scoped `(topic_id, invitee)` (Decision E). The caller
/// inserts this after a successful `redeem_invite`; `redeem_invite` rejects an invite whose key is
/// already present (replay).
pub fn invite_seen_key(topic_id: &str, invitee: &PublicKey) -> String {
    format!("{topic_id}:{}", invitee.to_hex())
}

/// Mint a **single-use** sealed invite credential carrying the topic key, addressed to `invitee`
/// (NIP-59 seal + gift-wrap — the SAME primitive the M10 private listing uses). Always replay-protected
/// (`reusable = false`), regardless of `expires_at` (chorus-2). `nonce` makes each mint unique. The
/// reusable public-join credential is built separately by [`build_public_join`].
pub fn mint_invite(
    issuer: &Identity,
    invitee: &PublicKey,
    meta: &TopicMeta,
    key: &TopicKey,
    nonce: &str,
    expires_at: Option<u64>,
    now: u64,
) -> Result<Event, HbError> {
    mint_invite_with_policy(issuer, invitee, meta, key, nonce, expires_at, false, now)
}

/// The full invite builder — `reusable` is set explicitly (`false` = single-use private invite;
/// `true` = the public-join credential). Private to keep the public API two clear entry points.
#[allow(clippy::too_many_arguments)]
fn mint_invite_with_policy(
    issuer: &Identity,
    invitee: &PublicKey,
    meta: &TopicMeta,
    key: &TopicKey,
    nonce: &str,
    expires_at: Option<u64>,
    reusable: bool,
    now: u64,
) -> Result<Event, HbError> {
    let payload = serde_json::to_string(&InvitePayload {
        meta: meta.clone(),
        topic_key: hex::encode(key.0),
        nonce: nonce.to_string(),
        expires_at,
        reusable,
        schema_v: SCHEMA_V,
        crypto_v: CRYPTO_V,
    })?;
    let issuer_sk = issuer.keys().secret_key();
    let rumor: UnsignedEvent = EventBuilder::new(Kind::from_u16(KIND_TOPIC_INVITE), payload)
        .tags([
            Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
            Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
        ])
        .custom_created_at(Timestamp::from(now))
        .build(issuer.public_key());
    let seal_content = nip44::encrypt(issuer_sk, invitee, rumor.as_json(), nip44::Version::V2)
        .map_err(|e| HbError::Nostr(e.to_string()))?;
    let seal = EventBuilder::new(Kind::Seal, seal_content)
        .custom_created_at(Timestamp::from(now))
        .sign_with_keys(issuer.keys())
        .map_err(|e| HbError::Nostr(e.to_string()))?;
    EventBuilder::gift_wrap_from_seal(invitee, &seal, [])
        .map_err(|e| HbError::Nostr(e.to_string()))
}

/// Redeem an invite addressed to `me` → `(TopicMeta, TopicKey)`. Rejects (clean `Err`, never a panic):
/// not a gift-wrap, a forged **outer** wrap signature, not sealed to me (ECDH mismatch), a forged
/// **seal** signature, the wrong inner kind, a future/zero version, **expired**, or **replayed**.
///
/// **Single-use is enforced here, atomically (chorus-1):** for a **single-use** invite (`expires_at =
/// Some`), the `(topic_id, invitee)` key is checked-and-inserted into `seen` in one step, closing the
/// caller-side TOCTOU. A **public-join** credential (`expires_at = None`) is intentionally **reusable**
/// — it is exempt from the seen-set (every joiner derives the same name-scoped invitee, so a shared
/// key would collide). The caller persists `seen` after a successful redeem. **Honest limit:** `seen`
/// is device-local — a restore-to-new-device user could re-redeem an unexpired old single-use invite.
pub fn redeem_invite(me: &Identity, invite: &Event, seen: &mut NonceSet, now: u64) -> Result<(TopicMeta, TopicKey), HbError> {
    if invite.kind != Kind::GiftWrap {
        return Err(HbError::InvalidEvent("not a NIP-59 gift wrap (kind 1059)".into()));
    }
    // Verify the outer 1059 signature (the ephemeral wrap key) before doing ECDH work — a relay
    // can't make us decrypt a signature-invalid junk wrap (chorus-1 R-2).
    verify_event(invite)?;
    let me_sk = me.keys().secret_key();
    let seal_json = nip44::decrypt(me_sk, &invite.pubkey, &invite.content).map_err(|_| HbError::DecryptionFailed)?;
    let seal = Event::from_json(&seal_json).map_err(|e| HbError::InvalidEvent(e.to_string()))?;
    if seal.kind != Kind::Seal {
        return Err(HbError::InvalidEvent("inner event is not a NIP-59 seal".into()));
    }
    seal.verify().map_err(|_| HbError::InvalidSignature)?;
    let issuer = seal.pubkey;
    let rumor_json = nip44::decrypt(me_sk, &issuer, &seal.content).map_err(|_| HbError::DecryptionFailed)?;
    let rumor = UnsignedEvent::from_json(&rumor_json).map_err(|e| HbError::InvalidEvent(e.to_string()))?;
    if rumor.kind != Kind::from_u16(KIND_TOPIC_INVITE) {
        return Err(HbError::InvalidEvent(format!(
            "expected invite kind {KIND_TOPIC_INVITE}, got {}",
            rumor.kind.as_u16()
        )));
    }
    let inner_schema = tag_u8_from(&rumor.tags, TAG_SCHEMA)
        .ok_or_else(|| HbError::InvalidEvent("invite missing/malformed schema version".into()))?;
    check_schema(inner_schema)?;
    let inner_crypto = tag_u8_from(&rumor.tags, TAG_CRYPTO)
        .ok_or_else(|| HbError::InvalidEvent("invite missing/malformed crypto version".into()))?;
    check_crypto(inner_crypto)?;

    let payload: InvitePayload = serde_json::from_str(&rumor.content)?;
    check_schema(payload.schema_v)?;
    check_crypto(payload.crypto_v)?;
    if payload.schema_v != inner_schema || payload.crypto_v != inner_crypto {
        return Err(HbError::InvalidEvent("version mismatch between the invite payload and its signed tags".into()));
    }
    // Expiry is checked INDEPENDENTLY of the replay policy (chorus-2): a reusable credential may still
    // carry an expiry, and a single-use invite need not.
    if let Some(exp) = payload.expires_at {
        if now > exp {
            return Err(HbError::InvalidEvent("invite expired".into()));
        }
    }
    // Replay protection is keyed on the EXPLICIT `reusable` flag, not on `expires_at`. A single-use
    // invite (every private invite) is atomically checked-and-inserted (closing the caller TOCTOU); the
    // reusable public-join credential is exempt (every joiner derives the same name-scoped invitee, so a
    // shared seen-key would collide and block honest joiners).
    if !payload.reusable {
        let seen_key = invite_seen_key(&payload.meta.topic_id, &me.public_key());
        if !seen.insert(seen_key) {
            return Err(HbError::InvalidEvent("invite already redeemed (replay)".into()));
        }
    }
    let key_bytes: [u8; 32] = hex::decode(&payload.topic_key)
        .map_err(|_| HbError::InvalidEncryptedMessage)?
        .try_into()
        .map_err(|_| HbError::InvalidEncryptedMessage)?;
    Ok((payload.meta, TopicKey(key_bytes)))
}

/// The deterministic **public-join keypair** derived from a public Topic's name. Any joiner
/// reconstructs it from the (public) name, so it can open the public-join credential — this is the
/// **participation bar** (Decision A): the encryption stops passive relay scrapers, not joiners.
pub fn public_join_keys(name: &str) -> Result<Keys, HbError> {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_PUBLIC_JOIN), normalize_name(name).as_bytes());
    let mut sk = [0u8; 32];
    hk.expand(b"public-join-secret", &mut sk).expect("32 is a valid HKDF-SHA256 output length");
    let secret = SecretKey::from_slice(&sk).map_err(|e| HbError::Nostr(e.to_string()))?;
    Ok(Keys::new(secret))
}

/// An `Identity` wrapping the public-join keypair — the "redeemer" of a public-join credential.
pub fn public_join_identity(name: &str) -> Result<Identity, HbError> {
    Ok(Identity::from_keys(public_join_keys(name)?))
}

/// Build the **public-join credential**: the topic key sealed to the name-derived public-join pubkey,
/// no expiry (reusable by any joiner). Published once by the creator; any joiner derives the keypair,
/// fetches it, and redeems → the key. Decision A's "key sealed to a well-known public-join tag".
pub fn build_public_join(creator: &Identity, meta: &TopicMeta, key: &TopicKey, now: u64) -> Result<Event, HbError> {
    if meta.private {
        return Err(HbError::InvalidEvent("a private Topic has no public-join credential".into()));
    }
    let pj = public_join_keys(&meta.name)?;
    // reusable = true: every joiner derives the same name-scoped invitee, so the credential must NOT be
    // consumed by the seen-set (chorus). No expiry (reusable for the Topic's life).
    mint_invite_with_policy(creator, &pj.public_key(), meta, key, "public-join", None, true, now)
}

// ── helpers ──────────────────────────────────────────────────────────────────────────────────────

/// Read the signed `hb-cv` crypto version from a topic membership/post event (the content is
/// ciphertext, so the signed tag is authoritative).
fn crypto_v_of(event: &Event) -> Result<u8, HbError> {
    event
        .tags
        .find(TagKind::custom(TAG_CRYPTO))
        .and_then(|t| t.content())
        .and_then(|s| s.parse::<u8>().ok())
        .ok_or_else(|| HbError::InvalidEvent("topic event missing/malformed crypto version".into()))
}

/// Read a custom named tag from a rumor's `Tags` as a `u8` (the rumor is an `UnsignedEvent`).
fn tag_u8_from(tags: &Tags, name: &str) -> Option<u8> {
    tags.find(TagKind::custom(name)).and_then(|t| t.content()).and_then(|s| s.parse::<u8>().ok())
}

/// The `d`-tag (topic_id) a membership/post event claims. Both membership (`seal_membership`) **and**
/// channel posts (`seal_post`) carry a `d` = topic_id tag, so this resolves for both. The proof
/// statement is rebuilt from this, so a key-holder who re-tags an event to a different topic
/// invalidates the (topic-bound) proof — and re-tagging also breaks the outer pseudonym signature.
fn topic_id_of(event: &Event) -> Result<String, HbError> {
    event
        .tags
        .identifier()
        .map(str::to_string)
        .ok_or_else(|| HbError::InvalidEvent("topic event missing d=topic_id".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::BrowseKey;

    const NOW: u64 = 1_700_000_000;

    fn public_topic() -> (TopicMeta, TopicKey) {
        // W4: public names are category-rooted paths now ("video/…", not a bare "80s-anime").
        new_topic("video/80s-anime", "VHS rips & fansubs", vec!["anime".into(), "vhs".into()], false).unwrap()
    }

    fn private_topic() -> (TopicMeta, TopicKey) {
        new_topic("back-room", "private", vec![], true).unwrap()
    }

    // ───────────────────────── W4: path normalization + fixed-root rule ─────────────────────────

    #[test]
    fn path_normalizes_case_space_and_extra_slashes_to_one_id() {
        // Decision K: every trivial variant (case / spacing / leading-trailing-doubled slash)
        // collapses to ONE normalized path → ONE topic_id (so two creators land in the same room).
        let canonical = topic_id_for_name("video/animation/anime");
        for variant in [
            "Video / Animation / Anime",
            "  video/animation/anime  ",
            "video//animation///anime",
            "/video/animation/anime/",
            "VIDEO/Animation/ANIME",
        ] {
            assert_eq!(topic_id_for_name(variant), canonical, "variant {variant:?} must converge to one id");
        }
    }

    #[test]
    fn path_nfkc_unicode_normalizes_to_the_ascii_id() {
        // Decision K (Codex #5): a full-width Unicode root NFKC-normalizes to the ASCII category, and
        // lowercase is applied AFTER NFKC — so `ＶＩＤＥＯ/anime` resolves to the same id as `video/anime`.
        let ascii = topic_id_for_name("video/anime");
        assert_eq!(topic_id_for_name("ＶＩＤＥＯ/anime"), ascii, "full-width root → same id (NFKC then lowercase)");
        // And NFKC makes the full-width root a valid category (else the root check would reject it).
        assert_eq!(topic_root("ＶＩＤＥＯ/anime"), Some("video"));
    }

    #[test]
    fn public_name_with_non_category_root_is_rejected() {
        // The answer to "what stops /blah/test/video/video?": the root `blah` isn't a category.
        assert!(new_topic("blah/test/video", "", vec![], false).is_err(), "non-category root rejected");
        assert!(new_topic("anime", "", vec![], false).is_err(), "a bare non-category name is rejected");
        assert!(validate_public_name("blah").is_err());
    }

    #[test]
    fn public_name_exceeding_depth_cap_is_rejected() {
        // MAX_TOPIC_DEPTH segments is OK; one deeper is rejected (junk can't make the tree unbounded).
        let at_cap = std::iter::once("video").chain(std::iter::repeat("x").take(MAX_TOPIC_DEPTH - 1)).collect::<Vec<_>>().join("/");
        assert!(new_topic(&at_cap, "", vec![], false).is_ok(), "a path at the depth cap is accepted");
        let too_deep = std::iter::once("video").chain(std::iter::repeat("x").take(MAX_TOPIC_DEPTH)).collect::<Vec<_>>().join("/");
        assert!(new_topic(&too_deep, "", vec![], false).is_err(), "a path past the depth cap is rejected");
    }

    #[test]
    fn valid_category_path_is_accepted_and_name_is_normalized() {
        let (meta, _key) = new_topic(" Video / Animation / Anime ", "", vec![], false).unwrap();
        assert_eq!(meta.name, "video/animation/anime", "the stored name is the canonical normalized path");
        assert!(!meta.private);
    }

    #[test]
    fn private_topic_keeps_freeform_name_and_random_id() {
        // The root/depth rules are a PUBLIC-namespace rule only — a private Topic keeps its freeform
        // name (no category root required) and a random, unlisted id.
        let (a, _) = new_topic("back room: 90s tapes", "", vec![], true).unwrap();
        let (b, _) = new_topic("back room: 90s tapes", "", vec![], true).unwrap();
        assert_eq!(a.name, "back room: 90s tapes", "private name is kept verbatim (freeform)");
        assert_ne!(a.topic_id, b.topic_id, "private ids are random (not name-derived)");
    }

    // ───────────────────────── THE NEGATIVES (first, hardest) ─────────────────────────

    #[test]
    fn announce_never_carries_the_topic_key() {
        // B1: a public announce is plaintext discovery metadata — a non-member parses name/desc/tags +
        // topic_id and gets NO key. The raw key bytes appear nowhere in the serialized event.
        let author = Identity::generate();
        let (meta, key) = public_topic();
        let ev = build_announce(&author, &meta, NOW).unwrap();
        let json = ev.as_json();
        assert!(!json.contains(&hex::encode(key.0)), "the announce must NOT contain the topic key (hex)");
        // And the parser returns meta with no key channel at all.
        let parsed = parse_announce(&ev).unwrap();
        assert_eq!(parsed.topic_id, meta.topic_id);
        assert_eq!(parsed.name, "video/80s-anime", "the announce carries the canonical normalized path");
        assert!(!parsed.private);
    }

    #[test]
    fn private_topic_has_no_announce() {
        let author = Identity::generate();
        let (meta, _key) = private_topic();
        assert!(build_announce(&author, &meta, NOW).is_err(), "a private Topic must be unlisted (no announce)");
    }

    #[test]
    fn membership_pubkey_is_the_derived_pseudonym_not_the_real_npub() {
        // B2: the Nostr `pubkey` field is the topic-scoped pseudonym; the real npub is ONLY inside the
        // topic_key-encrypted content. A holder of neither key reads no npub from the raw event.
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let ev = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();

        let expected_pseudonym = member_sign_keys(&key, &member.public_key()).unwrap().public_key();
        assert_eq!(ev.pubkey, expected_pseudonym, "event.pubkey is the derived pseudonym");
        assert_ne!(ev.pubkey, member.public_key(), "event.pubkey is NOT the real npub");

        // The real npub (hex AND bech32) must not appear anywhere in the raw event (it is in ciphertext).
        let json = ev.as_json();
        assert!(!json.contains(&member.public_key().to_hex()), "real npub hex must not leak in the raw event");
        assert!(!json.contains(&member.npub()), "real npub bech32 must not leak in the raw event");
    }

    #[test]
    fn non_member_cannot_open_membership_or_post() {
        // No key → ciphertext only → Err. (The wrong-key holder is the stand-in for a non-member.)
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let m = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        let p = seal_post(&key, &meta.topic_id, &member, "hi", NOW).unwrap();
        let (_other_meta, wrong_key) = new_topic("other", "", vec![], false).unwrap();
        assert!(matches!(open_membership(&wrong_key, &m), Err(HbError::DecryptionFailed)));
        assert!(matches!(open_post(&wrong_key, &p, NOW), Err(HbError::DecryptionFailed)));
    }

    #[test]
    fn f17_membership_and_post_cannot_be_cross_interpreted() {
        // A membership ciphertext fed to open_post (and vice-versa) → Err on the domain byte, even
        // with the RIGHT key, so the two event types are never confused.
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let m = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        let p = seal_post(&key, &meta.topic_id, &member, "hi", NOW).unwrap();
        // Re-sign the membership content as a POST event (same content, wrong kind+domain) → domain Err.
        let signer = member_sign_keys(&key, &member.public_key()).unwrap();
        let m_as_post = EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), m.content.clone())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        let p_as_member = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), p.content.clone())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(matches!(open_post(&key, &m_as_post, NOW), Err(HbError::InvalidEvent(_))), "membership bytes are not a post");
        assert!(matches!(open_membership(&key, &p_as_member), Err(HbError::InvalidEvent(_))), "post bytes are not a membership");
    }

    #[test]
    fn f13_no_topic_event_carries_a_browse_key() {
        // Producer-side INV-2: scan every topic artifact's serialized bytes for a browse-key — it is
        // in no field, plaintext or ciphertext (the functions never receive one).
        let author = Identity::generate();
        let member = Identity::generate();
        let (meta, key) = public_topic();
        let browse_key: BrowseKey = rand::random();
        let bk_hex = hex::encode(browse_key);

        let announce = build_announce(&author, &meta, NOW).unwrap();
        let membership = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        let post = seal_post(&key, &meta.topic_id, &member, "body", NOW).unwrap();
        let invite = mint_invite(&author, &member.public_key(), &meta, &key, "n", Some(NOW + 100), NOW).unwrap();
        let public_join = build_public_join(&author, &meta, &key, NOW).unwrap();

        for ev in [&announce, &membership, &post, &invite, &public_join] {
            assert!(!ev.as_json().contains(&bk_hex), "a topic event leaked a browse-key");
        }
    }

    #[test]
    fn redeem_rejects_not_for_me_expired_and_replayed() {
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let stranger = Identity::generate();
        let (meta, key) = private_topic();

        // (a) not sealed to me.
        let inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n1", Some(NOW + 100), NOW).unwrap();
        assert!(redeem_invite(&stranger, &inv, &mut NonceSet::new(), NOW).is_err(), "a stranger cannot redeem");

        // (b) expired.
        let expired = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n2", Some(NOW - 1), NOW).unwrap();
        assert!(redeem_invite(&invitee, &expired, &mut NonceSet::new(), NOW).is_err(), "an expired invite is refused");

        // (c) replayed: the FIRST redeem atomically records the (topic_id, invitee) seen-key, so the
        // SECOND is rejected — single-use is enforced inside redeem_invite (no caller TOCTOU).
        let mut seen = NonceSet::new();
        let _ = redeem_invite(&invitee, &inv, &mut seen, NOW).unwrap();
        assert!(redeem_invite(&invitee, &inv, &mut seen, NOW).is_err(), "a redeemed invite cannot be replayed");
    }

    // ───────────────────────── round-trips ─────────────────────────

    #[test]
    fn membership_seal_open_round_trip() {
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let ev = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        let opened = open_membership(&key, &ev).unwrap();
        assert_eq!(opened.member, member.public_key(), "the real npub is recovered from ciphertext");
        assert_eq!(opened.joined_at, NOW);
    }

    #[test]
    fn post_seal_open_round_trip_and_expiry_tag_present() {
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let ev = seal_post(&key, &meta.topic_id, &author, "criterion sale!", NOW).unwrap();
        // NIP-40 expiration tag present at now + 24h.
        let exp = ev.tags.find(TagKind::Expiration).and_then(|t| t.content()).and_then(|s| s.parse::<u64>().ok());
        assert_eq!(exp, Some(NOW + POST_TTL_SECS), "post carries a NIP-40 expiration at +24h");
        let opened = open_post(&key, &ev, NOW).unwrap().unwrap();
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.body, "criterion sale!");
        assert_eq!(opened.ts, NOW);
    }

    #[test]
    fn invite_mint_redeem_yields_same_meta_and_key() {
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n", Some(NOW + 100), NOW).unwrap();
        let (rmeta, rkey) = redeem_invite(&invitee, &inv, &mut NonceSet::new(), NOW).unwrap();
        assert_eq!(rmeta, meta, "the meta round-trips");
        assert_eq!(rkey.0, key.0, "the topic key round-trips");
    }

    #[test]
    fn public_join_credential_opens_for_any_name_deriver() {
        // Decision A: a public-join credential sealed to the name-derived keypair opens for ANY joiner
        // who reconstructs that keypair from the (public) name — the participation bar.
        let creator = Identity::generate();
        let (meta, key) = public_topic();
        let cred = build_public_join(&creator, &meta, &key, NOW).unwrap();

        // A joiner who knows only the name reconstructs the public-join identity and redeems.
        let joiner_view = public_join_identity(&meta.name).unwrap();
        let (rmeta, rkey) = redeem_invite(&joiner_view, &cred, &mut NonceSet::new(), NOW).unwrap();
        assert_eq!(rmeta.topic_id, meta.topic_id);
        assert_eq!(rkey.0, key.0, "any joiner obtains the topic key from the name-derived credential");
    }

    #[test]
    fn roster_is_current_set_leaving_shrinks_zero_is_dissolved() {
        let (meta, key) = public_topic();
        let a = Identity::generate();
        let b = Identity::generate();
        let ma = seal_membership(&key, &meta.topic_id, &a, NOW).unwrap();
        let mb = seal_membership(&key, &meta.topic_id, &b, NOW).unwrap();
        let full = roster(&key, &[ma.clone(), mb.clone()]);
        assert_eq!(full.len(), 2, "both members on the roster");
        assert!(full.contains(&a.public_key()) && full.contains(&b.public_key()));
        // "Leaving" = the event is gone (retracted at the relay); roster of what remains shrinks.
        let after_leave = roster(&key, &[mb]);
        assert_eq!(after_leave, vec![b.public_key()], "a left member is no longer on the roster");
        // Zero memberships = dissolved.
        assert!(roster(&key, &[]).is_empty(), "no memberships ⇒ dissolved (empty roster)");
    }

    #[test]
    fn membership_dedups_by_real_npub() {
        // Two membership events for the same member (a re-join / replace) collapse to one roster entry.
        let (meta, key) = public_topic();
        let a = Identity::generate();
        let m1 = seal_membership(&key, &meta.topic_id, &a, NOW).unwrap();
        let m2 = seal_membership(&key, &meta.topic_id, &a, NOW + 5).unwrap();
        assert_eq!(roster(&key, &[m1, m2]), vec![a.public_key()], "one member ⇒ one roster entry");
    }

    #[test]
    fn name_reuse_old_key_membership_excluded_from_new_room() {
        // Decision C: a recreated public Topic reuses the topic_id but gets a NEW key; an old-key
        // membership event fails to open under the new key and is excluded from the new roster.
        let (meta, old_key) = public_topic();
        let member = Identity::generate();
        let old_ev = seal_membership(&old_key, &meta.topic_id, &member, NOW).unwrap();
        // Recreate: same name ⇒ same topic_id, fresh key.
        let (meta2, new_key) = new_topic(&meta.name, "recreated", vec![], false).unwrap();
        assert_eq!(meta2.topic_id, meta.topic_id, "name reuse ⇒ same topic_id");
        assert!(open_membership(&new_key, &old_ev).is_err(), "old-key membership fails under the new key");
        assert!(roster(&new_key, &[old_ev]).is_empty(), "stale events do not pollute the recreated roster");
    }

    // ───────────────────────── 24h filter ─────────────────────────

    #[test]
    fn post_older_than_24h_is_filtered_locally() {
        let (meta, key) = public_topic();
        let author = Identity::generate();
        // A post authored 24h+1s ago; even though we can decrypt it, the local filter drops it.
        let ev = seal_post(&key, &meta.topic_id, &author, "stale", NOW).unwrap();
        let later = NOW + POST_TTL_SECS + 1;
        assert!(open_post(&key, &ev, later).unwrap().is_none(), "a >24h post is filtered to None");
        // A fresh post (exactly at the boundary) still opens.
        assert!(open_post(&key, &ev, NOW + POST_TTL_SECS).unwrap().is_some(), "a post at the 24h boundary still opens");
    }

    // ───────────────────────── versioning / fuzz / adversarial ─────────────────────────

    #[test]
    fn membership_future_crypto_version_is_recognised_not_misdecrypted() {
        // A signed hb-cv tag claiming a future version is refused cleanly (UnsupportedVersion), not
        // decrypted under a wrong key.
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let signer = member_sign_keys(&key, &member.public_key()).unwrap();
        let content = topic_encrypt(&key, MEMBERSHIP_DOMAIN, "{}").unwrap();
        let ev = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), content)
            .tags([
                Tag::identifier(meta.topic_id.clone()),
                Tag::custom(TagKind::custom(TAG_CRYPTO), [(CRYPTO_V + 1).to_string()]),
            ])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(matches!(open_membership(&key, &ev), Err(HbError::UnsupportedVersion(v)) if v == CRYPTO_V + 1));
    }

    #[test]
    fn malformed_topic_events_are_reasoned_err_never_panic() {
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let good = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        let signer = member_sign_keys(&key, &member.public_key()).unwrap();

        let mutations: Vec<String> = vec![
            String::new(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
            "!!!! not base64 @@@@".to_string(),
            good.content.chars().take(good.content.len() / 2).collect(),
        ];
        for content in mutations {
            // Re-sign so the event is structurally valid but the CONTENT is junk.
            let ev = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), content)
                .tags([
                    Tag::identifier(meta.topic_id.clone()),
                    Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
                ])
                .custom_created_at(Timestamp::from(NOW))
                .sign_with_keys(&signer)
                .unwrap();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open_membership(&key, &ev)));
            assert!(r.is_ok(), "open_membership panicked on junk content — must be a reasoned Err");
            assert!(r.unwrap().is_err(), "junk membership content must be rejected");
        }
    }

    #[test]
    fn tampered_membership_ciphertext_fails_the_aead_tag() {
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let signer = member_sign_keys(&key, &member.public_key()).unwrap();
        let good = topic_encrypt(&key, MEMBERSHIP_DOMAIN, r#"{"member_npub":"x","joined_at":1}"#).unwrap();
        // Flip the last base64 char → AEAD/MAC failure.
        let mut chars: Vec<char> = good.chars().collect();
        if let Some(last) = chars.last_mut() {
            *last = if *last == 'A' { 'B' } else { 'A' };
        }
        let tampered: String = chars.into_iter().collect();
        let ev = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), tampered)
            .tags([
                Tag::identifier(meta.topic_id.clone()),
                Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
            ])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(open_membership(&key, &ev).is_err(), "a tampered ciphertext must fail the AEAD tag");
    }

    #[test]
    fn b2_forged_npub_binding_is_rejected() {
        // A key-holder seals a membership but lies about whose npub it is: encrypt a DIFFERENT npub in
        // the content while signing with a pseudonym derived from yet another — open_membership re-derives
        // and rejects the mismatch (roster-integrity guard).
        let (meta, key) = public_topic();
        let real = Identity::generate();
        let victim = Identity::generate();
        // Sign with the pseudonym for `real`, but claim to be `victim` in the ciphertext.
        let signer = member_sign_keys(&key, &real.public_key()).unwrap();
        let payload = serde_json::to_string(&MemberPayload {
            member_npub: victim.npub(),
            joined_at: NOW,
            proof: String::new(), // rejected on the B2 pseudonym binding before the proof is even checked
        })
        .unwrap();
        let content = topic_encrypt(&key, MEMBERSHIP_DOMAIN, &payload).unwrap();
        let forged = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), content)
            .tags([
                Tag::identifier(meta.topic_id.clone()),
                Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
            ])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(
            matches!(open_membership(&key, &forged), Err(HbError::InvalidEvent(_))),
            "a membership whose pubkey doesn't bind to its claimed npub is rejected"
        );
    }

    #[test]
    fn redeem_rejects_non_gift_wrap_and_foreign_inner_kind() {
        let me = Identity::generate();
        let note = me.sign(EventBuilder::new(Kind::TextNote, "hi")).unwrap();
        assert!(matches!(redeem_invite(&me, &note, &mut NonceSet::new(), NOW), Err(HbError::InvalidEvent(_))));
    }

    // ───────────────────── chorus-1: insider forgery + reusable public-join + future ts ─────────────────────

    #[test]
    fn insider_cannot_forge_membership_for_a_never_joiner() {
        // THE chorus-1 CRITICAL: a topic-key holder can derive ANY member's pseudonym signing key, so
        // the B2 pubkey-binding alone does not stop an insider from enrolling someone who never joined.
        // The real-key proof-of-participation does: the insider has no valid proof signed by the victim,
        // and cannot forge one. open_membership rejects.
        let (meta, key) = public_topic();
        let victim = Identity::generate(); // never joined; the insider knows only the public npub
        let insider_signer = member_sign_keys(&key, &victim.public_key()).unwrap(); // derivable by any keyholder

        // (a) No proof at all → reject.
        let no_proof = serde_json::to_string(&MemberPayload {
            member_npub: victim.npub(),
            joined_at: NOW,
            proof: String::new(),
        })
        .unwrap();
        let forged = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &no_proof).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&insider_signer)
            .unwrap();
        assert!(open_membership(&key, &forged).is_err(), "an insider with no real-key proof cannot enrol the victim");

        // (b) A proof the INSIDER signs (with their own key, claiming to be the victim) → reject: the
        // proof is not signed by the victim's real key.
        let insider = Identity::generate();
        let bogus_proof = build_proof(&insider, &membership_statement(&meta.topic_id), NOW).unwrap();
        let with_bogus = serde_json::to_string(&MemberPayload {
            member_npub: victim.npub(),
            joined_at: NOW,
            proof: bogus_proof.as_json(),
        })
        .unwrap();
        let forged2 = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &with_bogus).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&insider_signer)
            .unwrap();
        assert!(open_membership(&key, &forged2).is_err(), "a proof not signed by the victim's real key is rejected");

        // And the victim's OWN membership opens fine (positive control).
        let real = seal_membership(&key, &meta.topic_id, &victim, NOW).unwrap();
        assert_eq!(open_membership(&key, &real).unwrap().member, victim.public_key());
    }

    #[test]
    fn insider_cannot_impersonate_another_in_the_channel() {
        // A key-holder cannot post as another member: the post's real-key proof binds the author + body.
        let (meta, key) = public_topic();
        let victim = Identity::generate();
        let signer = member_sign_keys(&key, &victim.public_key()).unwrap();
        let payload = serde_json::to_string(&PostPayload {
            author_npub: victim.npub(),
            body: "I said this".into(),
            ts: NOW,
            proof: String::new(),
        })
        .unwrap();
        let forged = EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), topic_encrypt(&key, POST_DOMAIN, &payload).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(open_post(&key, &forged, NOW).is_err(), "a key-holder cannot impersonate another member in the channel");
    }

    #[test]
    fn proof_bound_to_body_cannot_be_reattached_to_a_different_message() {
        // A keyholder takes a member's VALID post proof and tries to staple it to a different body →
        // reject (the proof binds sha256(body)).
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let real = seal_post(&key, &meta.topic_id, &author, "the real body", NOW).unwrap();
        // Extract the (decrypted) proof and re-wrap it around a different body.
        let plain = topic_decrypt(&key, POST_DOMAIN, CRYPTO_V, &real.content).unwrap();
        let payload: PostPayload = serde_json::from_slice(&plain).unwrap();
        let signer = member_sign_keys(&key, &author.public_key()).unwrap();
        let swapped = serde_json::to_string(&PostPayload {
            author_npub: payload.author_npub,
            body: "a DIFFERENT body".into(),
            ts: payload.ts,
            proof: payload.proof, // the valid proof for the OTHER body
        })
        .unwrap();
        let tampered = EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), topic_encrypt(&key, POST_DOMAIN, &swapped).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(open_post(&key, &tampered, NOW).is_err(), "a proof for one body cannot be reattached to another");
    }

    #[test]
    fn public_join_credential_is_reusable_not_consumed_by_the_seen_set() {
        // chorus-1 A-1: the public-join credential (no expiry) is intentionally reusable — every joiner
        // derives the SAME name-scoped invitee, so it must NOT be consumed by the (topic_id, invitee)
        // seen-set. Redeeming twice with the SAME seen-set both succeed.
        let creator = Identity::generate();
        let (meta, key) = public_topic();
        let cred = build_public_join(&creator, &meta, &key, NOW).unwrap();
        let joiner = public_join_identity(&meta.name).unwrap();
        let mut seen = NonceSet::new();
        assert!(redeem_invite(&joiner, &cred, &mut seen, NOW).is_ok(), "first public-join redeem ok");
        assert!(redeem_invite(&joiner, &cred, &mut seen, NOW).is_ok(), "public-join is reusable (not consumed)");
        assert!(seen.is_empty(), "a reusable public-join credential records no seen-nonce");
    }

    #[test]
    fn proof_without_the_hbm_domain_prefix_is_rejected() {
        // chorus-2 #4: a KIND_TOPIC_PROOF event the member signed for ANOTHER app (content `join:{id}`,
        // no `hbm:` prefix) cannot be replayed as a Hoardbook membership proof — open rebuilds the
        // prefixed statement and the equality check fails.
        let (meta, key) = public_topic();
        let victim = Identity::generate();
        let signer = member_sign_keys(&key, &victim.public_key()).unwrap();
        // A foreign, un-prefixed proof the victim "signed elsewhere".
        let foreign_proof = victim
            .sign(EventBuilder::new(Kind::from_u16(KIND_TOPIC_PROOF), format!("join:{}", meta.topic_id)).custom_created_at(Timestamp::from(NOW)))
            .unwrap();
        let payload = serde_json::to_string(&MemberPayload {
            member_npub: victim.npub(),
            joined_at: NOW,
            proof: foreign_proof.as_json(),
        })
        .unwrap();
        let ev = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &payload).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(open_membership(&key, &ev).is_err(), "an un-prefixed (cross-protocol) proof must be rejected");
    }

    #[test]
    fn single_use_invite_with_no_expiry_is_still_replay_protected() {
        // chorus-2 #5: replay policy is keyed on the explicit `reusable` flag, NOT on `expires_at`. A
        // targeted private invite with no expiry is still single-use (reusable=false).
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n", None, NOW).unwrap();
        let mut seen = NonceSet::new();
        assert!(redeem_invite(&invitee, &inv, &mut seen, NOW).is_ok(), "first redeem ok");
        assert!(
            redeem_invite(&invitee, &inv, &mut seen, NOW).is_err(),
            "a no-expiry private invite is still single-use (reusable=false)"
        );
    }

    #[test]
    fn future_dated_post_is_dropped_not_pinned_forever() {
        // chorus-1: a post whose authenticated ts is far in the future would otherwise sail past the
        // 24h-in-the-past filter forever; open_post drops it beyond the clock-skew tolerance.
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let future = NOW + 10 * POST_TTL_SECS;
        let ev = seal_post(&key, &meta.topic_id, &author, "pinned?", future).unwrap();
        assert!(open_post(&key, &ev, NOW).unwrap().is_none(), "a wildly future-dated post is dropped");
    }

    #[test]
    fn redeem_rejects_a_forged_outer_wrap_signature() {
        // chorus-1 R-2: the outer 1059 signature is verified, so a relay can't make us ECDH-decrypt a
        // signature-invalid junk wrap.
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let mut inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n", Some(NOW + 100), NOW).unwrap();
        // Tamper the outer content after signing → the outer id/sig no longer matches.
        inv.content.push('A');
        assert!(matches!(redeem_invite(&invitee, &inv, &mut NonceSet::new(), NOW), Err(HbError::InvalidSignature)));
    }

    #[test]
    fn public_topic_id_is_stable_for_the_same_name() {
        assert_eq!(topic_id_for_name("Criterion"), topic_id_for_name(" criterion "), "id normalizes name");
        assert_ne!(topic_id_for_name("a"), topic_id_for_name("b"));
    }

    #[test]
    fn topic_key_serde_roundtrips_as_hex_and_debug_redacts() {
        let key = TopicKey::generate();
        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains(&hex::encode(key.0)), "serialized as hex");
        let back: TopicKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back.0, key.0);
        assert!(format!("{key:?}").contains("REDACTED"), "Debug must not leak the key");
    }
}
