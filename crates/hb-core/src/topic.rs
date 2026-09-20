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
//!         membership sk = HKDF-SHA256(salt="hoardbook/topic-member", ikm=member_SECRET)
//!                           .expand("member/v2" ‖ topic_key ‖ topic_id)        (QURATOR-292)
//!       so the Nostr `pubkey` field is a topic-scoped pseudonym computable ONLY by the member
//!       (posts/announces/channel keep the older HMAC(topic_key, npub) pseudonym — they are 24h-
//!       ephemeral and their integrity rests on the real-key proof, not on pseudonym secrecy).
//!       QURATOR-292 closed the two eviction vectors the OLD public-tweak derivation allowed: any
//!       key-holder could derive a member's pseudonym from the (public) npub and (1) sign a NIP-09
//!       retraction of their membership with it, or (2) — worse — publish junk at the addressable
//!       (pseudonym, 31118, topic_id) coordinate with a later `created_at`, which every NIP-01
//!       relay honours as a supersession, no deletion compliance needed. A secret-keyed derivation
//!       makes both unconstructible, and being a pure function of (nsec, topic_key, topic_id) it
//!       re-derives after a reinstall from a restored nsec + re-obtained topic key.
//!       *** B2 binding (v2) — the proof commits to the pseudonym ***
//!         A verifier holds only the topic key, so it can NO LONGER re-derive the pseudonym to check
//!         `event.pubkey`. Instead the encrypted content carries `proof` = a NIP-01 event **signed by
//!         the member's REAL key** binding `hbm:join2:{topic_id}:{pseudonym_pubkey}` at `joined_at` —
//!         the real key authorizes the EXACT pseudonym that signed the outer event. The distinct
//!         `join2` prefix (vs the v1 `hbm:join:…`, which names no pseudonym) is the DOWNGRADE GUARD:
//!         a v1 proof must not be wrappable in an event signed by an attacker's pseudonym. The
//!         reader is v2-ONLY (owner ruling 2026-09-20: republish-on-next-launch, no dual-read), so
//!         pre-migration events fail `open_membership` until the member republishes.
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
//!   BROADCAST (M13 Part A — a member's announce to the whole roster; NOT the discovery ANNOUNCE
//!   above — same English word, two different wire artifacts, disambiguated by type: `Announcement`/
//!   `seal_announce`/`open_announce` here vs `TopicMeta`/`build_announce`/`parse_announce` above):
//!       Rides the SAME kind as CHANNEL (KIND_TOPIC_POST) and the SAME 24h NIP-40/local-filter
//!       lifecycle — only the ciphertext's domain byte tells the two apart:
//!       content = TOPIC_ENC(topic_key, 0x03 ‖ {author_npub, body, ts})       // 0x03 = domain byte (F17)
//!       *** the proof is DOMAIN-SEPARATE from a post's (`hbm:announce:` vs `hbm:post:`), MANDATORY
//!       not optional ***: without its own prefix, a topic-key holder could decrypt a member's ordinary
//!       post and re-wrap the identical plaintext under the announce domain — "promoting" a post the
//!       member never broadcast into one that verifies. The channel POST stays UNTHROTTLED; a BROADCAST
//!       is rate-limited (a timer, enforced in hb-app against `ANNOUNCE_MIN_INTERVAL_SECS`/
//!       `announce_cooldown_remaining` — this crate supplies only the arithmetic, never a clock).
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
//! **Negatives this enforces (tested first, hardest):** the discovery announce never carries the
//! topic_key (B1); a membership event's Nostr `pubkey` is the derived pseudonym, the real npub only in
//! ciphertext (B2); a non-member (no key) cannot open a membership, post, or broadcast (`Err`); a
//! membership/post/broadcast ciphertext fed to the wrong opener is rejected on the domain byte (F17,
//! now three-way: 0x01/0x02/0x03); a post's real-key proof cannot be promoted into a passing broadcast
//! proof, nor vice-versa (M13 Part A — the distinct `hbm:announce:` prefix); no topic event carries the
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

pub(crate) const TAG_SCHEMA: &str = "hb-v";
pub(crate) const TAG_CRYPTO: &str = "hb-cv";

/// Domain byte distinguishing a membership ciphertext from a channel-post ciphertext (F17) — the two
/// share the topic conversation key, so the first plaintext byte pins which event type it is.
/// `pub(crate)`: the wire-freeze test (INVARIANT_AUDIT.md I-3) pins these as launch-frozen wire
/// discriminants — they live inside signed ciphertext already durable on relays.
pub(crate) const MEMBERSHIP_DOMAIN: u8 = 0x01;
pub(crate) const POST_DOMAIN: u8 = 0x02;
/// M13 Part A — a member BROADCAST rides the SAME kind as a post (1117) under the SAME topic key;
/// this third domain byte is the only thing that tells the two apart (F17, extended). **Not** to be
/// confused with the plaintext discovery ANNOUNCE (`KIND_TOPIC_ANNOUNCE`) above — that event carries
/// no domain byte at all (it isn't topic-key-encrypted).
pub(crate) const ANNOUNCE_DOMAIN: u8 = 0x03;

/// Channel posts expire 24h after their authenticated `ts` (spec §11; Decision D — relay-honoured
/// **and** locally filtered).
pub const POST_TTL_SECS: u64 = 24 * 60 * 60;

/// A post whose authenticated `ts` is more than this far in the **future** is dropped by `open_post`
/// (chorus-1): a future-dated `ts` would otherwise sail past the 24h-in-the-past filter and stay
/// visible indefinitely. 1h tolerates honest clock skew; beyond that is a pin-forever abuse.
const MAX_FUTURE_SKEW_SECS: u64 = 60 * 60;

const HKDF_SALT_TOPIC: &[u8] = b"hoardbook/topic-key";
const HKDF_SALT_PUBLIC_JOIN: &[u8] = b"hoardbook/topic-public-join";
/// QURATOR-292 — domain-separates the membership pseudonym from every other derivation in the crate
/// (the topic conversation key above, the public-join keys, the DM-cache key). **A launch-frozen wire
/// discriminant**: changing it silently changes every member's membership pseudonym, which — because
/// kind 31118 is addressable — would strand every membership event already on a relay at a coordinate
/// the member no longer signs at. Pinned in `wire_freeze`.
pub(crate) const HKDF_SALT_TOPIC_MEMBER: &[u8] = b"hoardbook/topic-member";
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

/// One decrypted member broadcast (M13 Part A) — same field shape as `Post` (a broadcast IS a channel
/// item, just domain-tagged differently), kept as its own type so a reader can't accidentally treat an
/// announcement as an ordinary post, or vice-versa, at the type level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
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

/// M13 Part A — the payload inside a member BROADCAST ciphertext. Identical shape to `PostPayload`
/// (same fields, same types); the two are still distinct Rust types so a broadcast can never be
/// silently deserialized as a post (or vice-versa) even if a caller fumbled the domain byte.
#[derive(Serialize, Deserialize)]
struct AnnounceMsgPayload {
    author_npub: String,
    body: String,
    ts: u64,
    /// Same shape as `PostPayload.proof`, but bound to the DISTINCT `hbm:announce:` statement
    /// (`announce_statement`), never the post statement — so a post's proof cannot be replayed to
    /// forge a broadcast (see the negative test `a_posts_proof_cannot_be_promoted_to_an_announce`).
    proof: String,
}

/// QURATOR-298: `pub` (fields stay private) so [`open_invite`] can hand the parsed payload across
/// the crate boundary — hb-net's poller holds it OPAQUELY and feeds it straight back into
/// [`redeem_opened_invite`]; nothing outside this module reads a field.
#[derive(Serialize, Deserialize)]
pub struct InvitePayload {
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

/// The public seam onto [`normalize_name`] (private in this module): validate `name` as a **public**
/// Topic path, then return its canonical normalized form — the same string [`topic_id_for_name`]
/// hashes, so a caller can agree with it (devtest #11 — a join-first lookup normalizes the same way
/// a create would, so the same room is found before a new one is minted).
pub fn normalized_public_name(name: &str) -> Result<String, HbError> {
    validate_public_name(name)?;
    Ok(normalize_name(name))
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

/// Decrypt a topic ciphertext without assuming which domain it is — returns the domain byte + the
/// remaining plaintext. `topic_decrypt` (the strict single-domain caller) and `open_channel_item` (the
/// multi-domain caller) both delegate here, so there is exactly ONE place that turns ciphertext bytes
/// into `(domain, plaintext)` — the discipline that keeps F17 sound as domains multiply (M13 Part A
/// added the third, 0x03).
fn topic_decrypt_any(key: &TopicKey, crypto_v: u8, content_b64: &str) -> Result<(u8, Vec<u8>), HbError> {
    check_crypto(crypto_v)?;
    let ck = topic_conversation_key(key, crypto_v);
    let raw = B64.decode(content_b64.as_bytes()).map_err(|_| HbError::InvalidEncryptedMessage)?;
    let pt = decrypt_to_bytes(&ck, &raw).map_err(|_| HbError::DecryptionFailed)?;
    let (domain, rest) = pt.split_first().ok_or(HbError::DecryptionFailed)?;
    Ok((*domain, rest.to_vec()))
}

/// Decrypt a topic ciphertext, enforcing the expected domain byte. `crypto_v` is the signed `hb-cv`;
/// an unknown version is refused before any decryption (forward-compat); a wrong domain byte is an
/// `Err` (F17), never silently mis-interpreted as the other event type. Delegates to
/// `topic_decrypt_any` — unchanged signature/behavior, so every existing F17 negative stays green.
fn topic_decrypt(key: &TopicKey, domain_expected: u8, crypto_v: u8, content_b64: &str) -> Result<Vec<u8>, HbError> {
    let (domain, rest) = topic_decrypt_any(key, crypto_v, content_b64)?;
    if domain != domain_expected {
        return Err(HbError::InvalidEvent(format!(
            "topic domain byte mismatch: expected 0x{domain_expected:02x}, got 0x{domain:02x}"
        )));
    }
    Ok(rest)
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

/// QURATOR-292 — the MEMBERSHIP pseudonym: derived from the member's **secret**, so only the member
/// can produce it (any topic-key holder could derive the old `member_sign_keys` from the public npub
/// and evict them — NIP-09 retraction, or junk at the addressable `(P_victim, 31118, topic_id)`
/// coordinate that every NIP-01 relay honours as a supersession). `topic_key` stays in the HKDF info
/// so a key rotation yields a fresh pseudonym, as before; `topic_id` scopes it so a pseudonym learned
/// in one topic is useless in another. A pure function of (nsec, topic_key, topic_id), so a reinstall
/// recovers it from a restored nsec + re-obtained topic key. MEMBERSHIP only — posts/announces/channel
/// keep [`member_sign_keys`]; nothing compares the two pseudonyms, so they diverge alone.
pub fn membership_sign_keys(key: &TopicKey, topic_id: &str, member: &Identity) -> Result<Keys, HbError> {
    let ikm = member.keys().secret_key().secret_bytes();
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_TOPIC_MEMBER), &ikm);
    let mut info = b"member/v2".to_vec();
    info.extend_from_slice(&key.0);
    info.extend_from_slice(topic_id.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out).expect("32 is a valid HKDF-SHA256 output length");
    // A uniform 32-byte HKDF output is a valid secp256k1 scalar with overwhelming probability; the
    // ~2^-128 out-of-range case surfaces as a clean Err (never a panic), as in member_sign_keys.
    let sk = SecretKey::from_slice(&out).map_err(|e| HbError::Nostr(e.to_string()))?;
    Ok(Keys::new(sk))
}

// ── proof-of-participation (chorus-1: real-key authorization, carried in ciphertext) ─────────────

/// The `hbm:` proof domain prefixes — **launch-frozen wire discriminants** (they live inside signed
/// proof events that persist in members' rosters), extracted as consts so the wire-freeze test can
/// pin them (INVARIANT_AUDIT.md I-3).
/// The v1 join prefix. Superseded by `PROOF_JOIN2_PREFIX` (QURATOR-292) — no production path builds
/// a v1 statement any more, but the string is kept and frozen: v1 proofs still live inside sealed
/// roster events on relays, and the freeze records what the v2-only reader must keep NOT accepting.
#[allow(dead_code)] // referenced only by cfg(test) code (wire_freeze + the v1-rejection tests)
pub(crate) const PROOF_JOIN_PREFIX: &str = "hbm:join:";
/// QURATOR-292 — the v2 membership proof prefix. DISTINCT from `PROOF_JOIN_PREFIX`, and not merely a
/// different literal: the v2 statement also names the pseudonym pubkey that signed the OUTER
/// membership event. If the reader accepted the old `hbm:join:{topic_id}` shape (which names no
/// pseudonym), an attacker could wrap a victim's old v1 proof in an event signed by the ATTACKER's
/// own pseudonym and enrol the victim at a coordinate the attacker controls. The distinct prefix is
/// a downgrade guard, not cosmetics. **Launch-frozen** in `wire_freeze`.
pub(crate) const PROOF_JOIN2_PREFIX: &str = "hbm:join2:";
pub(crate) const PROOF_POST_PREFIX: &str = "hbm:post:";
/// M13 Part A — the broadcast-announce proof prefix. MANDATORY and distinct from `PROOF_POST_PREFIX`:
/// without its own prefix, a topic-key holder could decrypt a member's ordinary post (proof binds
/// `hbm:post:{topic}:{sha256(body)}`), re-encrypt the SAME plaintext under the announce domain byte,
/// and re-sign with the same (derivable) pseudonym — "promoting" a post the member sent into an
/// announcement they never broadcast, with a proof that still verifies. The distinct prefix makes
/// `verify_proof` reject that promotion (see `a_posts_proof_cannot_be_promoted_to_an_announce`).
pub(crate) const PROOF_ANNOUNCE_PREFIX: &str = "hbm:announce:";

/// The canonical v2 statement a member's REAL key signs to authorize joining `topic_id` **under the
/// pseudonym `pseudonym`** — the pseudonym pubkey that signs the outer membership event. This IS the
/// B2 binding since QURATOR-292: the verifier cannot re-derive a secret-keyed pseudonym, so instead
/// the real key must have committed to the exact `event.pubkey` it authorizes. Domain-separated by
/// the `hbm:` prefix (chorus-2) and by `join2` vs the v1 shape (the downgrade guard above).
fn membership_statement_v2(topic_id: &str, pseudonym: &PublicKey) -> String {
    format!("{PROOF_JOIN2_PREFIX}{topic_id}:{}", pseudonym.to_hex())
}

/// The canonical statement an author's REAL key signs to authorize a post (binds the body so a
/// key-holder cannot reattach the proof to a different message). Domain-separated like the membership
/// statement (chorus-2).
fn post_statement(topic_id: &str, body: &str) -> String {
    format!("{PROOF_POST_PREFIX}{topic_id}:{}", hex::encode(Sha256::digest(body.as_bytes())))
}

/// The canonical statement an author's REAL key signs to authorize a broadcast announce (M13 Part A).
/// Domain-separated from `post_statement` by its OWN `hbm:` prefix — not merely a different literal,
/// but the property that makes a post's proof unusable as a broadcast's proof (see
/// `PROOF_ANNOUNCE_PREFIX`).
fn announce_statement(topic_id: &str, body: &str) -> String {
    format!("{PROOF_ANNOUNCE_PREFIX}{topic_id}:{}", hex::encode(Sha256::digest(body.as_bytes())))
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
    if proof.created_at.as_secs() != at {
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
///
/// The payload's `name` is **derived-checked, never trusted** (QURATOR-133): `topic_id_for_name`
/// (the same function `new_topic` uses) must re-derive the claimed `topic_id`, and the event must
/// not be `#t`-tagged with a DIFFERENT category root than `topic_root(&name)` — so a validly-signed
/// announce can never relabel a room it does not own or file it under a root it does not belong to.
/// The root itself is derived, never read back from the tags (a correctly-named announce with only
/// user tags stays valid — the root is implied by the name). Topics are user-agnostic: nobody owns
/// a public Topic, so nobody gets to rename it — the name IS the identity.
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
    // The reader enforces the same public-namespace shape `new_topic` enforces (QURATOR-199): a
    // peer's announce never went through the create path, so without this check an over-deep path
    // (the only thing `validate_public_name` adds beyond the root checks below) would admit an
    // unbounded discovery tree from forged announces.
    validate_public_name(&meta.name)?;
    if topic_id_for_name(&meta.name) != meta.topic_id {
        return Err(HbError::InvalidEvent(format!(
            "announce name '{}' does not derive to its claimed topic_id (QURATOR-133 relabel)",
            meta.name
        )));
    }
    let root = topic_root(&meta.name).ok_or_else(|| {
        HbError::InvalidEvent(format!("announce name '{}' has no category root", meta.name))
    })?;
    if let Some(wrong) = event.tags.hashtags().find(|t| TOPIC_ROOTS.contains(t) && *t != root) {
        return Err(HbError::InvalidEvent(format!(
            "announce for '{}' is hashtag-rooted '{wrong}' but its name derives to root '{root}' (QURATOR-133 misfiling)",
            meta.name
        )));
    }
    meta.private = false;
    Ok(meta)
}

// ── MEMBERSHIP (durable roster) ──────────────────────────────────────────────────────────────────

/// Seal a membership event for `member` (their own `Identity`, so it can sign the real-key proof and
/// derive the secret-keyed pseudonym) in the Topic — encrypted under the topic key (domain 0x01),
/// signed on the wire by the **membership pseudonym** [`membership_sign_keys`] (QURATOR-292: derived
/// from the member's SECRET, so no key-holder can produce it), `d` = topic_id (replaceable per
/// member). The real npub + the proof live only inside the ciphertext; the proof's statement names
/// this pseudonym, which is what binds `event.pubkey` to the member on the reader side. You only
/// ever seal your **own** membership (you join).
pub fn seal_membership(key: &TopicKey, topic_id: &str, member: &Identity, now: u64) -> Result<Event, HbError> {
    let member_pk = member.public_key();
    let signer = membership_sign_keys(key, topic_id, member)?;
    let proof = build_proof(member, &membership_statement_v2(topic_id, &signer.public_key()), now)?;
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
/// kind, a valid signature (the pseudonym did sign it), the 0x01 domain byte, **and the B2 binding
/// (v2)** — the real-key proof must state `hbm:join2:{topic_id}:{event.pubkey}`, i.e. the member's
/// real key authorized the EXACT pseudonym that signed this event. The verifier cannot re-derive the
/// secret-keyed pseudonym (QURATOR-292), so the commitment rides the proof instead: a key-holder can
/// sign with their own pseudonym and claim any npub in the ciphertext, but then the statement names
/// THEIR pubkey and the claimed member's key never signed it. The v1 statement (`hbm:join:…`, no
/// pseudonym) is rejected by the distinct prefix — the downgrade guard. v2-ONLY by owner ruling
/// (republish-on-next-launch, no dual-read). A non-member (no key) gets `Err` (no decrypt).
pub fn open_membership(key: &TopicKey, event: &Event) -> Result<Membership, HbError> {
    if event.kind != Kind::from_u16(KIND_TOPIC_MEMBER) {
        return Err(HbError::InvalidEvent("not a topic membership event".into()));
    }
    verify_event(event)?;
    let crypto_v = crypto_v_of(event)?;
    let plain = topic_decrypt(key, MEMBERSHIP_DOMAIN, crypto_v, &event.content)?;
    let payload: MemberPayload = serde_json::from_slice(&plain)?;
    let member = parse_npub(&payload.member_npub)?;
    // B2 binding (v2) + chorus-1 in one check: only the member's own real key could have signed
    // `join2:{topic_id}:{this event's pubkey}` at `joined_at` — so a key-holder can neither enrol a
    // never-joiner nor rebind a member to a coordinate (pseudonym) of the key-holder's choosing.
    verify_proof(
        &payload.proof,
        &member,
        &membership_statement_v2(&topic_id_of(event)?, &event.pubkey),
        payload.joined_at,
    )?;
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

// ── CHANNEL (ephemeral 24h posts + member broadcasts, M13 Part A) ──────────────────────────────────

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

/// Seal a member BROADCAST (M13 Part A) — same shape as `seal_post`: encrypted under the topic key but
/// domain 0x03, signed by the derived pseudonym, same NIP-40 24h expiry, same real-key proof — just
/// bound to the DISTINCT `hbm:announce:` statement so it can never be confused with (or forged from) an
/// ordinary post's proof. The rate limit between two broadcasts (`ANNOUNCE_MIN_INTERVAL_SECS` /
/// `announce_cooldown_remaining`) is enforced by the CALLER (hb-app owns the clock + the persisted
/// `last_announce_at`) — this function does not check it, so misuse here is a caller bug, not a crypto
/// hole.
pub fn seal_announce(key: &TopicKey, topic_id: &str, author: &Identity, body: &str, now: u64) -> Result<Event, HbError> {
    let author_pk = author.public_key();
    let signer = member_sign_keys(key, &author_pk)?;
    let proof = build_proof(author, &announce_statement(topic_id, body), now)?;
    let payload = serde_json::to_string(&AnnounceMsgPayload {
        author_npub: author_pk.to_bech32().map_err(|e| HbError::Nostr(e.to_string()))?,
        body: body.to_string(),
        ts: now,
        proof: proof.as_json(),
    })?;
    let content = topic_encrypt(key, ANNOUNCE_DOMAIN, &payload)?;
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

/// Open a member broadcast → `Some(Announcement)` if fresh, `None` if older than 24h (same local filter
/// as `open_post`). Same kind + signature + B2-binding checks as `open_post`, but gated on the 0x03
/// domain byte and verified against the announce statement — so an ordinary post ciphertext (and
/// vice-versa) is rejected here on the domain byte, and a post's proof cannot be promoted into a
/// passing announce proof (the mandatory negative this closes).
pub fn open_announce(key: &TopicKey, event: &Event, now: u64) -> Result<Option<Announcement>, HbError> {
    if event.kind != Kind::from_u16(KIND_TOPIC_POST) {
        return Err(HbError::InvalidEvent("not a topic channel event".into()));
    }
    verify_event(event)?;
    let crypto_v = crypto_v_of(event)?;
    let plain = topic_decrypt(key, ANNOUNCE_DOMAIN, crypto_v, &event.content)?;
    let payload: AnnounceMsgPayload = serde_json::from_slice(&plain)?;
    let author = parse_npub(&payload.author_npub)?;
    let expected = member_sign_keys(key, &author)?.public_key();
    if event.pubkey != expected {
        return Err(HbError::InvalidEvent(
            "announce pubkey does not bind to the claimed author (B2 forgery guard)".into(),
        ));
    }
    // chorus-1-shaped: the real-key proof binds the topic + the body UNDER THE ANNOUNCE STATEMENT, so
    // a key-holder cannot impersonate another member NOR promote that member's own post proof.
    verify_proof(&payload.proof, &author, &announce_statement(&topic_id_of(event)?, &payload.body), payload.ts)?;
    if payload.ts > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
        return Ok(None);
    }
    if now.saturating_sub(payload.ts) > POST_TTL_SECS {
        return Ok(None);
    }
    Ok(Some(Announcement { author, body: payload.body, ts: payload.ts }))
}

/// The two kinds sharing the channel (kind 1117), as recovered by a single decrypt in
/// [`open_channel_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelItem {
    Post(Post),
    Announce(Announcement),
}

/// Open a channel event without knowing in advance whether it is a post or a broadcast: decrypt
/// **once** ([`topic_decrypt_any`]), then branch on the recovered domain byte into the same
/// B2-binding + proof-verify + future-skew/24h enforcement as `open_post`/`open_announce`
/// respectively. An unrecognised domain byte is a forward-compat `Err` — a future channel-item kind
/// must be recognised by this reader before it is trusted, never silently mis-typed as whichever
/// variant happens to parse.
pub fn open_channel_item(key: &TopicKey, event: &Event, now: u64) -> Result<Option<ChannelItem>, HbError> {
    if event.kind != Kind::from_u16(KIND_TOPIC_POST) {
        return Err(HbError::InvalidEvent("not a topic channel event".into()));
    }
    verify_event(event)?;
    let crypto_v = crypto_v_of(event)?;
    let (domain, plain) = topic_decrypt_any(key, crypto_v, &event.content)?;
    match domain {
        POST_DOMAIN => {
            let payload: PostPayload = serde_json::from_slice(&plain)?;
            let author = parse_npub(&payload.author_npub)?;
            let expected = member_sign_keys(key, &author)?.public_key();
            if event.pubkey != expected {
                return Err(HbError::InvalidEvent(
                    "post pubkey does not bind to the claimed author (B2 forgery guard)".into(),
                ));
            }
            verify_proof(&payload.proof, &author, &post_statement(&topic_id_of(event)?, &payload.body), payload.ts)?;
            if payload.ts > now.saturating_add(MAX_FUTURE_SKEW_SECS) || now.saturating_sub(payload.ts) > POST_TTL_SECS {
                return Ok(None);
            }
            Ok(Some(ChannelItem::Post(Post { author, body: payload.body, ts: payload.ts })))
        }
        ANNOUNCE_DOMAIN => {
            let payload: AnnounceMsgPayload = serde_json::from_slice(&plain)?;
            let author = parse_npub(&payload.author_npub)?;
            let expected = member_sign_keys(key, &author)?.public_key();
            if event.pubkey != expected {
                return Err(HbError::InvalidEvent(
                    "announce pubkey does not bind to the claimed author (B2 forgery guard)".into(),
                ));
            }
            verify_proof(&payload.proof, &author, &announce_statement(&topic_id_of(event)?, &payload.body), payload.ts)?;
            if payload.ts > now.saturating_add(MAX_FUTURE_SKEW_SECS) || now.saturating_sub(payload.ts) > POST_TTL_SECS {
                return Ok(None);
            }
            Ok(Some(ChannelItem::Announce(Announcement { author, body: payload.body, ts: payload.ts })))
        }
        _ => Err(HbError::InvalidEvent(format!("unknown channel domain byte 0x{domain:02x} (forward-compat refusal)"))),
    }
}

// ── ANNOUNCE RATE LIMIT (pure arithmetic; the timer + persistence live in hb-app) ────────────────────

/// The minimum spacing between two broadcasts from the same member. **OWNER-RATIFICATION DEFAULT**
/// (2026-07-03 ruling): the ruling fixes that a cooldown MUST exist and MUST NOT be optional — the
/// *value* below has not itself been ratified and may change before ship. This crate only supplies the
/// arithmetic + the constant; hb-app owns the clock and persists `last_announce_at` per member.
pub const ANNOUNCE_MIN_INTERVAL_SECS: u64 = 3_600;

/// Seconds remaining before a member may broadcast again, given their `last_announce_at` (`None` =
/// never broadcast ⇒ no cooldown). **Saturating, both ends:** `saturating_add` so a `last_announce_at`
/// near `u64::MAX` can't overflow into a bogus small number, and `saturating_sub` so a clock ROLLBACK
/// (`now` before `last_announce_at`) reads as a LARGER remaining cooldown, never underflows into a
/// huge/negative value a caller could misread as "go ahead" — a rollback can only ever look *more*
/// throttled, never less.
pub fn announce_cooldown_remaining(last_announce_at: Option<u64>, now: u64) -> u64 {
    match last_announce_at {
        None => 0,
        Some(last) => last.saturating_add(ANNOUNCE_MIN_INTERVAL_SECS).saturating_sub(now),
    }
}

// ── INVITE CREDENTIAL (sealed, single-use) + public-join ─────────────────────────────────────────

/// The seen-set key for an invite redemption — scoped `(issuer, topic_id, invitee, nonce)` (Decision
/// E). The caller inserts this after a successful `redeem_invite`; `redeem_invite` rejects an invite
/// whose key is already present (replay).
///
/// **The issuer MUST be the VERIFIED seal signer** [`redeem_invite`] recovers (never a
/// payload-declared value): the topic_id and nonce inside `InvitePayload` are self-declared, so a
/// hostile-but-validly-signed wrap can freely name a victim's real `(topic_id, invitee)`. Keying on
/// those alone let such a wrap burn the victim's single-use slot ("already redeemed" for the genuine
/// invite). Binding the VERIFIED issuer makes the slot unforgeable across issuers — a hostile wrap
/// occupies only the attacker's own slot — and the nonce keeps the "this-invite" identity, so
/// replaying the SAME invite still collides while a legitimate re-mint (fresh nonce) does not.
pub fn invite_seen_key(issuer: &PublicKey, topic_id: &str, invitee: &PublicKey, nonce: &str) -> String {
    format!("{}:{topic_id}:{}:{nonce}", issuer.to_hex(), invitee.to_hex())
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

/// Redeem an invite addressed to `me` → `(TopicMeta, TopicKey, issuer)`, where `issuer` is the invite
/// ISSUER's [`PublicKey`] (the seal signer — surfaced so a caller can run a consent gate on who is
/// vouching for the join). Rejects (clean `Err`, never a panic): not a gift-wrap, a forged **outer**
/// wrap signature, not sealed to me (ECDH mismatch), a forged **seal** signature, the wrong inner
/// kind, a future/zero version, **expired**, **replayed**, when `expected_topic_id` is `Some` — an
/// invite for a different topic than the one being joined (a validly-signed invite whose payload
/// names a different topic must never satisfy a join for another topic, else a lying relay could
/// shadow the real credential with an attacker-controlled one; mirrors [`newest_announce`]'s
/// `topic_id` binding) — or, when `expected_issuer` is `Some`, an invite whose VERIFIED seal signer
/// is not that issuer (QURATOR-227: the preview→redeem substitution — the user consented to a
/// specific issuer's npub in the W8 preview, so a forged wrap naming the same topic_id must not
/// satisfy the redeem).
///
/// **Single-use is enforced here, atomically (chorus-1):** for a **single-use** invite (`expires_at =
/// Some`), the `(issuer, topic_id, invitee, nonce)` key is checked-and-inserted into `seen` in one step, closing the
/// caller-side TOCTOU. A **public-join** credential (`expires_at = None`) is intentionally **reusable**
/// — it is exempt from the seen-set (every joiner derives the same name-scoped invitee, so a shared
/// key would collide). The caller persists `seen` after a successful redeem. **Honest limit:** `seen`
/// is device-local — a restore-to-new-device user could re-redeem an unexpired old single-use invite.
///
/// **QURATOR-298 — composition, not monolith:** this is now [`open_invite`] (the DETERMINISTIC open:
/// everything a pure function of `(me, wrap bytes)` — kind pins, both signature verifies, both
/// NIP-44 decrypts, payload parse) followed by [`redeem_opened_invite`] (the POLICY verdict:
/// everything consulting `now`, `seen` or the caller's expectations). The split lets a poller open
/// a wrap ONCE and then try the policy without re-paying the crypto; this single-event entry point
/// keeps its original signature for existing callers.
pub fn redeem_invite(
    me: &Identity,
    invite: &Event,
    seen: &mut NonceSet,
    now: u64,
    expected_topic_id: Option<&str>,
    expected_issuer: Option<&PublicKey>,
) -> Result<(TopicMeta, TopicKey, PublicKey), HbError> {
    let (issuer, payload) = open_invite(me, invite)?;
    redeem_opened_invite(me, issuer, payload, seen, now, expected_topic_id, expected_issuer)
}

/// The DETERMINISTIC half of [`redeem_invite`] (QURATOR-298): open an invite-shaped NIP-59 gift
/// wrap addressed to `me` and parse it, doing NO policy work. Everything here is a pure function
/// of `(me`'s keys, wrap bytes)` — the same wrap always yields the same verdict — which is exactly
/// the property a negative cache keys on (hb-net's `TOPIC_FAILED_OPENS`): the 1059 kind pin, the
/// outer signature verify (before any ECDH work, chorus-1 R-2), the wrap NIP-44 decrypt, the seal
/// parse + `Kind::Seal` pin + `seal.verify()`, the rumor decrypt + inner-kind pin (31_119), the
/// schema/crypto tags, the `InvitePayload` parse, the payload/tag version consistency, and the
/// 32-byte `topic_key` decode/length gate (moved here from the policy half by QURATOR-301 — it is
/// pure over the payload bytes, so a wrap with honest crypto and a garbage key now fails the OPEN
/// and is negatively cached instead of re-paying the crypto once per poll forever). Returns
/// the VERIFIED seal signer (the issuer — recovered from the seal AFTER `seal.verify()`, never
/// self-declared) and the parsed payload, ready for [`redeem_opened_invite`].
pub fn open_invite(me: &Identity, invite: &Event) -> Result<(PublicKey, InvitePayload), HbError> {
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
    // QURATOR-301: the 32-byte `topic_key` decode/length gate — a pure function of the payload
    // bytes — belongs in this DETERMINISTIC half, so a wrap with honest crypto, valid tags and
    // parseable JSON but a garbage `topic_key` fails HERE (the verdict hb-net's TOPIC_FAILED_OPENS
    // keys on) instead of re-paying 2× ECDH + 2× Schnorr once per poll for the process lifetime.
    // The gate must run BEFORE the replay-nonce insert in `redeem_opened_invite`, and does so BY
    // CONSTRUCTION: the insert exists only in that policy half, which no caller reaches unless
    // this function has already returned `Ok`, and this gate sits on the path to `Ok` — so a
    // malformed key can never burn the single-use seen-key. The policy half keeps its own call to
    // the same shared gate (it must decode anyway to build the `TopicKey` it returns), pinning the
    // decode-before-insert order there too, for payloads that did not arrive through here.
    decode_topic_key(&payload)?;
    Ok((issuer, payload))
}

/// The POLICY half of [`redeem_invite`] (QURATOR-298): the verdict over a wrap [`open_invite`]
/// already opened — takes the pre-opened `(issuer, payload)` so the caller pays the crypto ONCE
/// and may try this half against many caller contexts. Every check that is NOT a pure function of
/// (identity, wrap bytes) lives here because it consults the caller's `expected_topic_id` /
/// `expected_issuer` (W4 / QURATOR-227), the caller's `now` (expiry) or the mutable `seen` replay
/// set — so its verdict is deliberately NEVER negatively cacheable: the same wrap refused on a
/// join for topic B must still redeem for topic A. Takes the payload BY VALUE (the success path
/// moves `meta` out of it). Ordering inside is unchanged from the pre-split monolith.
pub fn redeem_opened_invite(
    me: &Identity,
    issuer: PublicKey,
    payload: InvitePayload,
    seen: &mut NonceSet,
    now: u64,
    expected_topic_id: Option<&str>,
    expected_issuer: Option<&PublicKey>,
) -> Result<(TopicMeta, TopicKey, PublicKey), HbError> {
    // W4: when the caller is joining a KNOWN topic, the payload's `topic_id` must match it — a
    // validly-signed invite naming a different topic must never satisfy a join for another topic
    // (else a lying relay could shadow the real credential). Checked BEFORE the replay-nonce insert
    // so a mismatched invite never burns a single-use invite's seen-key.
    if let Some(expected) = expected_topic_id {
        if payload.meta.topic_id != expected {
            return Err(HbError::InvalidEvent("invite is for a different topic than the one being joined".into()));
        }
    }
    // W-issuer (QURATOR-227): when the caller consented to a KNOWN issuer — the preview→redeem path
    // surfaces the seal signer's npub and the user acks it (W8) — the VERIFIED seal signer must be
    // that issuer. A racing attacker's forged wrap validly names the same topic_id (the W4 check
    // above cannot distinguish it), so accepting on first successful decrypt alone would redeem
    // whichever wrap the relay served first while the user consented to a different issuer. Checked
    // BEFORE the replay-nonce insert, next to and for the same reason as the topic-mismatch check
    // above: a mismatched invite must never burn a single-use invite's seen-key.
    if let Some(expected) = expected_issuer {
        if issuer != *expected {
            return Err(HbError::InvalidEvent("invite issuer is not the issuer the user consented to".into()));
        }
    }
    // Expiry is checked INDEPENDENTLY of the replay policy (chorus-2): a reusable credential may still
    // carry an expiry, and a single-use invite need not.
    if let Some(exp) = payload.expires_at {
        if now > exp {
            return Err(HbError::InvalidEvent("invite expired".into()));
        }
    }
    // Decode + length-validate the 32-byte `topic_key` BEFORE the replay-nonce insert — a hand-crafted
    // invite carrying a malformed key must fail here, not after burning the single-use seen-key (which
    // would permanently poison the (topic_id, invitee) slot and reject the genuine invite). Mirrors the
    // topic-mismatch check above, ordered the same way for the same reason. QURATOR-301: this gate is
    // shared with `open_invite` (which runs before this half in every composition, so a malformed key
    // normally never reaches this line); this call produces the key bytes below AND keeps the
    // decode-before-insert order pinned WITHIN the policy half — pinned by
    // `redeem_opened_invite_decodes_before_burning_the_seen_key`, which drives this half directly.
    let key_bytes: [u8; 32] = decode_topic_key(&payload)?;
    // Replay protection is keyed on the EXPLICIT `reusable` flag, not on `expires_at`. A single-use
    // invite (every private invite) is atomically checked-and-inserted (closing the caller TOCTOU); the
    // reusable public-join credential is exempt (every joiner derives the same name-scoped invitee, so a
    // shared seen-key would collide and block honest joiners).
    if !payload.reusable {
        // QURATOR-198: the seen-key binds the VERIFIED issuer (recovered from the seal above, after
        // `seal.verify()`) plus the payload nonce — NOT the self-declared topic_id alone. A hostile
        // wrap validly names any (topic_id, invitee), but cannot make its seal be signed by the
        // genuine issuer, so it can never occupy the genuine invite's slot.
        let seen_key = invite_seen_key(&issuer, &payload.meta.topic_id, &me.public_key(), &payload.nonce);
        if !seen.insert(seen_key) {
            return Err(HbError::InvalidEvent("invite already redeemed (replay)".into()));
        }
    }
    Ok((payload.meta, TopicKey(key_bytes), issuer))
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

/// Decode + length-validate an invite payload's 32-byte `topic_key` hex string (QURATOR-301). A
/// pure function of the payload bytes — no `now`, no `seen`, no caller expectations — so its
/// verdict is negatively cacheable, which is why the gate is invoked from BOTH halves: from
/// [`open_invite`] (making a garbage key part of the deterministic, cacheable open verdict) and
/// from [`redeem_opened_invite`] (which must decode anyway to build the [`TopicKey`] it returns,
/// and where the gate keeps decode-before-replay-insert pinned). One shared body, so the two call
/// sites cannot drift apart.
fn decode_topic_key(payload: &InvitePayload) -> Result<[u8; 32], HbError> {
    hex::decode(&payload.topic_key)
        .map_err(|_| HbError::InvalidEncryptedMessage)?
        .try_into()
        .map_err(|_| HbError::InvalidEncryptedMessage)
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

    /// Test-only mirror of `mint_invite_with_policy` (single-use, `reusable = false`) that accepts a
    /// raw `topic_key` string so a test can mint an invite carrying a malformed key — something the
    /// production `mint_invite` (which hex-encodes a real `TopicKey`) can never produce.
    fn mint_invite_with_raw_key(
        issuer: &Identity,
        invitee: &PublicKey,
        meta: &TopicMeta,
        topic_key: &str,
        nonce: &str,
        expires_at: Option<u64>,
        now: u64,
    ) -> Result<Event, HbError> {
        let payload = serde_json::to_string(&InvitePayload {
            meta: meta.clone(),
            topic_key: topic_key.to_string(),
            nonce: nonce.to_string(),
            expires_at,
            reusable: false,
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
        let at_cap = std::iter::once("video").chain(std::iter::repeat_n("x", MAX_TOPIC_DEPTH - 1)).collect::<Vec<_>>().join("/");
        assert!(new_topic(&at_cap, "", vec![], false).is_ok(), "a path at the depth cap is accepted");
        let too_deep = std::iter::once("video").chain(std::iter::repeat_n("x", MAX_TOPIC_DEPTH)).collect::<Vec<_>>().join("/");
        assert!(new_topic(&too_deep, "", vec![], false).is_err(), "a path past the depth cap is rejected");
    }

    #[test]
    fn normalized_public_name_agrees_with_topic_id_for_name_across_variants() {
        // devtest #11: the join-first lookup normalizes exactly like a create would, so the same
        // name (any case/space/extra-slash/NFKC variant) resolves to the SAME topic_id.
        let canonical = normalized_public_name("video/animation/anime").unwrap();
        assert_eq!(canonical, "video/animation/anime");
        for variant in [
            "Video / Animation / Anime",
            "  video/animation/anime  ",
            "video//animation///anime",
            "/video/animation/anime/",
            "VIDEO/Animation/ANIME",
            "ＶＩＤＥＯ/animation/anime",
        ] {
            let normalized = normalized_public_name(variant).unwrap();
            assert_eq!(
                topic_id_for_name(&normalized),
                topic_id_for_name(variant),
                "variant {variant:?} must still hash to the same id via topic_id_for_name"
            );
            assert_eq!(
                topic_id_for_name(&normalized),
                topic_id_for_name(&canonical),
                "variant {variant:?} must converge to the canonical id"
            );
        }
    }

    #[test]
    fn normalized_public_name_errors_on_a_non_category_root() {
        assert!(normalized_public_name("blah/test/video").is_err());
        assert!(normalized_public_name("anime").is_err());
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

    // ── QURATOR-133: an announce is only valid for the room its name derives to ──────────────────

    /// Test-only helper: an announce whose payload `meta` disagrees with its tags — exactly the
    /// attacker shape (take a real topic_id, relabel the room) — signed by an arbitrary key so it is
    /// otherwise a perfectly valid event. Built from raw parts rather than `build_announce` because
    /// production would refuse to emit it.
    fn forged_announce(
        author: &Identity,
        d_tag: &str,
        meta: TopicMeta,
        t_tags: Vec<String>,
        now: u64,
    ) -> Event {
        let payload = serde_json::to_string(&AnnouncePayload { v: SCHEMA_V, meta }).unwrap();
        let mut tags = vec![
            Tag::identifier(d_tag.to_string()),
            Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
        ];
        for t in t_tags {
            tags.push(Tag::hashtag(t));
        }
        author
            .sign(EventBuilder::new(Kind::from_u16(KIND_TOPIC_ANNOUNCE), payload).tags(tags).custom_created_at(Timestamp::from(now)))
            .unwrap()
    }

    #[test]
    fn announce_whose_name_does_not_derive_to_its_topic_id_is_rejected() {
        // QURATOR-133: anyone can re-announce an existing topic_id under an arbitrary name. Without
        // the derivation check the payload wins on recency and the directory shows their label.
        let attacker = Identity::generate();
        let (real, _key) = public_topic();
        let mut forged = real.clone();
        forged.name = "video/smear-campaign".to_string(); // a different room, same topic_id
        let ev = forged_announce(&attacker, &real.topic_id, forged, vec![], NOW + 1);
        assert!(
            parse_announce(&ev).is_err(),
            "an announce whose name does not derive to its topic_id must be rejected"
        );
    }

    #[test]
    fn announce_whose_hashtag_root_disagrees_with_topic_root_is_rejected() {
        // QURATOR-133 second half: even a correctly-named room must not be filed under a root it
        // does not belong to — no `#t` tag may be a category root other than topic_root(name).
        let attacker = Identity::generate();
        let (real, _key) = public_topic(); // root is `video`
        let ev = forged_announce(
            &attacker,
            &real.topic_id,
            real.clone(),
            vec!["audio".to_string()], // wrong root: `topic_root("video/80s-anime")` is `video`
            NOW + 1,
        );
        assert!(
            parse_announce(&ev).is_err(),
            "an announce #t-tagged with a root other than topic_root(name) must be rejected"
        );
    }

    #[test]
    fn announce_deeper_than_max_topic_depth_is_rejected() {
        // QURATOR-199: the reader must enforce the same depth cap as `new_topic`. Built via
        // `forged_announce` with a topic_id that IS derived from the too-deep name (so the
        // QURATOR-133 re-derivation passes) and a valid category root — the depth cap is the only
        // check that can fire. Mutation that must red this: remove the `validate_public_name` call
        // from `parse_announce` — the too-deep assert below then fails (parses Ok), while the
        // at-cap control stays green.
        let attacker = Identity::generate();
        let too_deep = std::iter::once("video").chain(std::iter::repeat_n("x", MAX_TOPIC_DEPTH)).collect::<Vec<_>>().join("/");
        let deep_meta = TopicMeta {
            topic_id: topic_id_for_name(&too_deep),
            name: too_deep,
            description: String::new(),
            tags: vec![],
            private: false,
        };
        let deep_id = deep_meta.topic_id.clone();
        let ev = forged_announce(&attacker, &deep_id, deep_meta, vec![], NOW);
        assert!(
            parse_announce(&ev).is_err(),
            "an announce deeper than MAX_TOPIC_DEPTH must be rejected by the reader, not just the creator"
        );

        // Control: the same forged shape at exactly MAX_TOPIC_DEPTH segments still parses — the
        // depth cap is the only thing firing above, not some other property of the forged event.
        let at_cap = std::iter::once("video").chain(std::iter::repeat_n("x", MAX_TOPIC_DEPTH - 1)).collect::<Vec<_>>().join("/");
        let cap_meta = TopicMeta {
            topic_id: topic_id_for_name(&at_cap),
            name: at_cap,
            description: String::new(),
            tags: vec![],
            private: false,
        };
        let cap_id = cap_meta.topic_id.clone();
        let ev = forged_announce(&attacker, &cap_id, cap_meta, vec![], NOW);
        assert!(
            parse_announce(&ev).is_ok(),
            "an announce at exactly MAX_TOPIC_DEPTH segments must still parse"
        );
    }

    #[test]
    fn well_formed_announce_still_parses() {
        let author = Identity::generate();
        let (meta, _key) = public_topic();
        let ev = build_announce(&author, &meta, NOW).unwrap();
        let parsed = parse_announce(&ev).unwrap();
        assert_eq!(parsed.topic_id, meta.topic_id);
        assert_eq!(parsed.name, meta.name);
        assert_eq!(parsed.description, meta.description);
        assert_eq!(parsed.tags, meta.tags);
        assert!(!parsed.private);
    }

    #[test]
    fn membership_pubkey_is_the_derived_pseudonym_not_the_real_npub() {
        // B2 (QURATOR-292): the Nostr `pubkey` field is the topic-scoped MEMBERSHIP pseudonym derived
        // from the member's SECRET; the real npub is ONLY inside the topic_key-encrypted content. A
        // holder of neither key reads no npub from the raw event.
        let (meta, key) = public_topic();
        let member = Identity::generate();
        let ev = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();

        // Call the production derivation — the expected value is NOT rebuilt by hand here.
        let expected_pseudonym = membership_sign_keys(&key, &meta.topic_id, &member).unwrap().public_key();
        assert_eq!(ev.pubkey, expected_pseudonym, "event.pubkey is the derived pseudonym");
        assert_ne!(ev.pubkey, member.public_key(), "event.pubkey is NOT the real npub");
        // QURATOR-292: the OLD public tweak (HMAC over the public npub, derivable by any key-holder)
        // must NO LONGER yield the membership pseudonym — that derivability was the eviction hole.
        let old_public_tweak = member_sign_keys(&key, &member.public_key()).unwrap().public_key();
        assert_ne!(ev.pubkey, old_public_tweak, "the public npub no longer derives the membership pseudonym");

        // The real npub (hex AND bech32) must not appear anywhere in the raw event (it is in ciphertext).
        let json = ev.as_json();
        assert!(!json.contains(&member.public_key().to_hex()), "real npub hex must not leak in the raw event");
        assert!(!json.contains(&member.npub()), "real npub bech32 must not leak in the raw event");
    }

    /// Acceptance 4 (QURATOR-292): the pseudonym is a pure function of (member secret, topic_key,
    /// topic_id) — a reinstall with a restored nsec + re-obtained topic key re-derives the SAME
    /// pseudonym, and both rotation inputs (key, topic) yield fresh pseudonyms as before.
    ///
    /// P-10 mutation anchors (orchestrator runs these — production edits, by line):
    /// - topic.rs line 508 (`info.extend_from_slice(&key.0);` in `membership_sign_keys`) → deleting
    ///   that line makes the "a different topic key yields a different pseudonym" assert RED.
    /// - topic.rs line 509 (`info.extend_from_slice(topic_id.as_bytes());`) → deleting it makes the
    ///   "a different topic_id yields a different pseudonym" assert RED.
    ///
    /// Line numbers verified against the tree that ships this test; re-verify before mutating.
    #[test]
    fn membership_pseudonym_rederives_after_reinstall() {
        let (meta, key) = public_topic();
        let member = Identity::generate();

        let a = membership_sign_keys(&key, &meta.topic_id, &member).unwrap().public_key();
        // Re-derive from the SAME inputs (the reinstall: restored nsec, re-obtained key) → identical.
        let b = membership_sign_keys(&key, &meta.topic_id, &member).unwrap().public_key();
        assert_eq!(a, b, "a reinstall re-derives the same membership pseudonym");
        assert_ne!(a, member.public_key(), "the pseudonym is still not the real npub (unlinkability)");

        // Rotation freshness: new topic key, or the same member in another topic → new pseudonym.
        let (_other_meta, other_key) = new_topic("other", "", vec![], false).unwrap();
        let c = membership_sign_keys(&other_key, &meta.topic_id, &member).unwrap().public_key();
        assert_ne!(a, c, "a different topic key yields a different pseudonym");
        let d = membership_sign_keys(&key, "deadbeef", &member).unwrap().public_key();
        assert_ne!(a, d, "a different topic_id yields a different pseudonym");

        // And it round-trips on the wire: the re-derived pseudonym is the one the sealed event carries.
        let ev = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        assert_eq!(ev.pubkey, a, "the re-derived pseudonym signs the membership event");
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
        assert!(redeem_invite(&stranger, &inv, &mut NonceSet::new(), NOW, None, None).is_err(), "a stranger cannot redeem");

        // (b) expired.
        let expired = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n2", Some(NOW - 1), NOW).unwrap();
        assert!(redeem_invite(&invitee, &expired, &mut NonceSet::new(), NOW, None, None).is_err(), "an expired invite is refused");

        // (c) replayed: the FIRST redeem atomically records the (issuer, topic_id, invitee, nonce)
        // seen-key, so the SECOND is rejected — single-use is enforced inside redeem_invite (no
        // caller TOCTOU).
        let mut seen = NonceSet::new();
        let _ = redeem_invite(&invitee, &inv, &mut seen, NOW, None, None).unwrap();
        assert!(redeem_invite(&invitee, &inv, &mut seen, NOW, None, None).is_err(), "a redeemed invite cannot be replayed");
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
        let (rmeta, rkey, _issuer) = redeem_invite(&invitee, &inv, &mut NonceSet::new(), NOW, None, None).unwrap();
        assert_eq!(rmeta, meta, "the meta round-trips");
        assert_eq!(rkey.0, key.0, "the topic key round-trips");
    }

    #[test]
    fn redeem_invite_composes_open_then_policy_qurator_298() {
        // QURATOR-298: `redeem_invite` is EXACTLY [`open_invite`] followed by
        // [`redeem_opened_invite`] — the decomposition hb-net's poller relies on to open a wrap
        // ONCE. The composed pair must yield the identical (meta, key, issuer) as the monolithic
        // entry point, and `open_invite` alone must return the VERIFIED seal signer.
        //
        // MUTATION (P-10, resolve by line number): at line 1019, the line
        // `    let issuer = seal.pubkey;` in `open_invite` — change it to
        // `let issuer = me.public_key();`. The FIRST assert below (open_invite returns the VERIFIED
        // seal signer) reds. Confirmed RED by the orchestrator 2026-09-19.
        //
        // ⚠ The originally-written anchor for this test was WRONG and is recorded here so it is not
        // re-tried: mutating `redeem_opened_invite`'s final line (`Ok((payload.meta,
        // TopicKey(key_bytes), issuer))` → `me.public_key()`) leaves this test GREEN. The three
        // asserts after the first compare COMPOSED against MONOLITHIC, and since the shim calls the
        // very same tail, that edit moves both sides of every equality identically — an equivalence
        // test is structurally blind to a change in the code both its sides share. Pick a mutation
        // in the half only ONE side reaches (as above), or the proof is vacuous.
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n-298a", Some(NOW + 100), NOW).unwrap();
        let (opened_issuer, payload) = open_invite(&invitee, &inv).unwrap();
        assert_eq!(opened_issuer, issuer.public_key(), "open_invite returns the VERIFIED seal signer");
        let composed =
            redeem_opened_invite(&invitee, opened_issuer, payload, &mut NonceSet::new(), NOW, None, None).unwrap();
        let monolithic = redeem_invite(&invitee, &inv, &mut NonceSet::new(), NOW, None, None).unwrap();
        assert_eq!(composed.0, monolithic.0, "composed and monolithic agree on the meta");
        assert_eq!(composed.1 .0, monolithic.1 .0, "composed and monolithic agree on the key");
        assert_eq!(composed.2, monolithic.2, "composed and monolithic agree on the issuer");
    }

    #[test]
    fn redeem_opened_invite_enforces_policy_on_the_pre_opened_pair_qurator_298() {
        // The POLICY half runs on the pre-opened (issuer, payload) alone — no `Event` in sight, so
        // no second open is even POSSIBLE. The W4 topic binding must fire identically here: the
        // same opened pair refused for topic B, redeemed for its own topic A.
        //
        // MUTATION (P-10, for the orchestrator to apply, resolve by line number): at line 1066
        // (`            if payload.meta.topic_id != expected {` — the W4 check inside
        // `redeem_opened_invite`), change the condition to `if false` — the (a) assert below reds
        // (a topic-B join is then satisfied by a topic-A invite).
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n-298b", Some(NOW + 100), NOW).unwrap();
        let inv2 = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n-298c", Some(NOW + 100), NOW).unwrap();
        let (opened_issuer, payload) = open_invite(&invitee, &inv).unwrap();
        let (opened_issuer2, payload2) = open_invite(&invitee, &inv2).unwrap();
        // (a) expected = a different topic id B → refused, on the pre-opened pair alone.
        assert!(
            redeem_opened_invite(&invitee, opened_issuer, payload, &mut NonceSet::new(), NOW, Some("a-different-topic-id-b"), None).is_err(),
            "the W4 topic binding lives in the policy half"
        );
        // (b) expected = A (the matching id) → redeems, and the issuer is the seal signer.
        let (rmeta, rkey, risser) =
            redeem_opened_invite(&invitee, opened_issuer2, payload2, &mut NonceSet::new(), NOW, Some(&meta.topic_id), None)
                .unwrap();
        assert_eq!(rmeta.topic_id, meta.topic_id);
        assert_eq!(rkey.0, key.0);
        assert_eq!(risser, issuer.public_key());
    }

    #[test]
    fn redeem_invite_rejects_an_invite_for_a_different_topic_id() {
        // W4: a validly-signed invite whose payload names a DIFFERENT topic than the one being joined
        // must never satisfy the join — else a lying relay could shadow the real credential with an
        // attacker-controlled one (mirrors newest_announce's topic_id binding). An invite minted for
        // topic A is rejected when the caller passes expected = B (attacker wins without the check),
        // accepted when expected = A (the returned issuer == the minting issuer), and accepted when
        // expected = None (the blind private-redeem path is unaffected).
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic(); // meta.topic_id = A
        let inv = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n", Some(NOW + 100), NOW).unwrap();

        // (a) expected = a different topic id B → attacker wins without the W4 check.
        assert!(
            redeem_invite(&invitee, &inv, &mut NonceSet::new(), NOW, Some("a-different-topic-id-b"), None).is_err(),
            "an invite for topic A must not satisfy a join for topic B"
        );
        // The mismatch is rejected BEFORE the replay-nonce insert, so a fresh seen-set still redeems it.
        let mut seen = NonceSet::new();
        assert!(seen.is_empty(), "a mismatched invite never burned the seen-key");

        // (b) expected = A (the matching id) → Ok, and the returned issuer == the minting issuer.
        let (_meta_r, _key_r, issuer_r) =
            redeem_invite(&invitee, &inv, &mut seen, NOW, Some(&meta.topic_id), None).unwrap();
        assert_eq!(issuer_r, issuer.public_key(), "the returned issuer is the invite's seal signer");

        // (c) expected = None (the blind private-redeem path) → Ok with a fresh seen-set.
        let inv2 = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n2", Some(NOW + 100), NOW).unwrap();
        assert!(redeem_invite(&invitee, &inv2, &mut NonceSet::new(), NOW, None, None).is_ok(), "blind redeem is unaffected");
    }

    #[test]
    fn redeem_invite_rejects_an_invite_from_a_different_issuer() {
        // QURATOR-227 — the issuer twin of the W4 topic_id test above. A validly-signed invite whose
        // VERIFIED seal signer is a DIFFERENT identity than the one the user consented to (the W8
        // preview surfaces the issuer's npub; the redeem binds to it) must never satisfy the redeem —
        // else a racing attacker's forged wrap naming the same topic_id wins by being served first.
        // MUTATION that reds this: in redeem_invite's W-issuer block, change `if issuer != *expected`
        // to `if false` — (a)'s refusal then accepts Mallory and the assert fires.
        let genuine = Identity::generate();
        let mallory = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let inv = mint_invite(&genuine, &invitee.public_key(), &meta, &key, "n", Some(NOW + 100), NOW).unwrap();

        // (a) expected issuer = Mallory → the genuine issuer's invite must be refused.
        let mut seen = NonceSet::new();
        assert!(
            redeem_invite(&invitee, &inv, &mut seen, NOW, Some(&meta.topic_id), Some(&mallory.public_key())).is_err(),
            "an invite from the genuine issuer must not satisfy a redeem consented to Mallory"
        );
        // The mismatch is rejected BEFORE the replay-nonce insert: the SAME seen-set (not a fresh
        // one) still redeems it afterwards — the rejected attempt burned no seen-key.
        let (_meta_r, key_r, issuer_r) =
            redeem_invite(&invitee, &inv, &mut seen, NOW, Some(&meta.topic_id), Some(&genuine.public_key())).unwrap();
        assert_eq!(key_r.0, key.0, "the genuine topic key is returned");
        assert_eq!(issuer_r, genuine.public_key(), "the returned issuer is the invite's seal signer");

        // (c) expected_issuer = None (the preview/public-join discovery path has nothing to expect
        // yet) → the blind redeem is unaffected.
        let inv2 = mint_invite(&genuine, &invitee.public_key(), &meta, &key, "n2", Some(NOW + 100), NOW).unwrap();
        assert!(
            redeem_invite(&invitee, &inv2, &mut NonceSet::new(), NOW, None, None).is_ok(),
            "a blind redeem (no expected issuer) is unaffected"
        );
    }

    #[test]
    fn issuer_bound_redeem_picks_the_genuine_wrap_regardless_of_order() {
        // QURATOR-227 — THE ATTACK. Two valid wraps to the same invitee naming the SAME topic_id: one
        // from the genuine issuer, one from an attacker (any party who can construct a decryptable
        // wrap addressed to the target can do this). `fetch_invite` (hb-net) redeems the FIRST wrap
        // that decrypts, so without the issuer binding whichever the relay serves first wins even
        // though the user consented to the genuine issuer's npub in the preview. hb-core cannot drive
        // `fetch_invite` (it needs a live relay), so this exercises `redeem_invite` over both wraps
        // in BOTH orders with expected_issuer = Some(genuine): the attacker is refused and the
        // genuine wrap redeemed whichever position it occupies. The ordering-independence of
        // `fetch_invite`'s first-valid loop itself is what the hb-it suite_topic row must cover.
        // MUTATION that reds this: in redeem_invite's W-issuer block, change `if issuer != *expected`
        // to `if false` — the attacker wrap then redeems in both orders and the key asserts fire.
        let genuine = Identity::generate();
        let attacker = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();
        let attack_key = TopicKey::generate();
        let g = mint_invite(&genuine, &invitee.public_key(), &meta, &key, "g", Some(NOW + 100), NOW).unwrap();
        let a = mint_invite(&attacker, &invitee.public_key(), &meta, &attack_key, "a", Some(NOW + 100), NOW).unwrap();

        // Genuine-first order.
        let mut seen = NonceSet::new();
        let (m1, k1, i1) =
            redeem_invite(&invitee, &g, &mut seen, NOW, Some(&meta.topic_id), Some(&genuine.public_key())).unwrap();
        assert_eq!(i1, genuine.public_key(), "genuine-first: the genuine issuer is returned");
        assert_eq!(k1.0, key.0, "genuine-first: the genuine topic key is returned");
        assert_eq!(m1.topic_id, meta.topic_id, "the genuine wrap names the genuine topic");
        assert!(
            redeem_invite(&invitee, &a, &mut seen, NOW, Some(&meta.topic_id), Some(&genuine.public_key())).is_err(),
            "genuine-first: the attacker's wrap is refused even though it names the same topic_id"
        );

        // Attacker-first order — the race the ticket describes: the forged wrap occupies the first
        // slot of the fetched vector, which is exactly what a relay serving the attacker's event
        // first produces.
        let mut seen2 = NonceSet::new();
        assert!(
            redeem_invite(&invitee, &a, &mut seen2, NOW, Some(&meta.topic_id), Some(&genuine.public_key())).is_err(),
            "attacker-first: the forged wrap is refused, not redeemed on first valid decrypt"
        );
        let (m2, k2, i2) =
            redeem_invite(&invitee, &g, &mut seen2, NOW, Some(&meta.topic_id), Some(&genuine.public_key())).unwrap();
        assert_eq!(i2, genuine.public_key(), "attacker-first: the genuine issuer is returned");
        assert_eq!(k2.0, key.0, "attacker-first: the genuine topic key is returned");
        assert_eq!(m2.topic_id, meta.topic_id, "the genuine wrap names the genuine topic");
    }

    #[test]
    fn malformed_topic_key_does_not_burn_the_single_use_seen_key() {
        // A hand-crafted single-use invite carrying a malformed `topic_key` passes every earlier check
        // (schema/crypto/expiry/topic-match), so the key decode/length gate is the LAST validation
        // before the replay-nonce insert. Ordered wrongly (insert-then-decode), the malformed invite
        // would burn the issuer's (issuer, topic_id, invitee, nonce) seen-key and then fail,
        // permanently poisoning the slot so the genuine invite is later rejected as "already
        // redeemed". The decode must precede the insert.
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic();

        // A malformed key (valid hex, wrong length) that sails past every earlier check.
        let bad = mint_invite_with_raw_key(
            &issuer,
            &invitee.public_key(),
            &meta,
            "deadbeef", // decodes to 4 bytes, not 32 — fails the length gate
            "n1",
            Some(NOW + 100),
            NOW,
        )
        .unwrap();

        let mut seen = NonceSet::new();
        assert!(
            matches!(
                redeem_invite(&invitee, &bad, &mut seen, NOW, None, None),
                Err(HbError::InvalidEncryptedMessage)
            ),
            "a malformed topic_key is rejected at the decode/length gate"
        );
        assert!(seen.is_empty(), "a malformed-key invite must NOT burn the seen-key");

        // The genuine invite for the SAME (topic_id, invitee) slot still redeems — the slot was not poisoned.
        let good = mint_invite(&issuer, &invitee.public_key(), &meta, &key, "n2", Some(NOW + 100), NOW).unwrap();
        assert!(
            redeem_invite(&invitee, &good, &mut seen, NOW, None, None).is_ok(),
            "the genuine invite redeems because the malformed one never claimed the seen-key"
        );
    }

    /// QURATOR-301 — the DETERMINISTIC-side gate. `open_invite` is the half whose verdict hb-net's
    /// `TOPIC_FAILED_OPENS` negatively caches (the poller records every open refusal and skips the
    /// wrap on every later poll), so the decode/length gate must fail HERE for a garbage-key wrap
    /// to be remembered after poll 1. Without this pin, deleting the open-side gate would leave
    /// every other test green — the policy half's own call still refuses the key, and the composed
    /// `malformed_topic_key_does_not_burn_the_single_use_seen_key` above stays green too — while
    /// the cache coverage silently regresses to re-paying the crypto once per poll forever. `"zz"`
    /// exercises the hex-decode arm of the shared gate; the existing test's `"deadbeef"` (valid
    /// hex, wrong length) exercises the length arm through the same body.
    ///
    /// MUTATION (P-10, resolve by LINE NUMBER — this anchor text is NOT unique in production, it
    /// names two call sites of the shared gate): in `open_invite`, DELETE the entire line 1054,
    /// `    decode_topic_key(&payload)?;` — the standalone gate line in `open_invite`'s tail (the
    /// FIRST occurrence, NOT the `let key_bytes: [u8; 32] = decode_topic_key(&payload)?;` line in
    /// `redeem_opened_invite`). This test reds: the open now succeeds for the `"zz"` wrap, so the
    /// `matches!(… Err(…))` assert fails.
    #[test]
    fn a_garbage_topic_key_fails_the_deterministic_open() {
        let issuer = Identity::generate();
        let invitee = Identity::generate();
        let (meta, _key) = private_topic();
        let bad = mint_invite_with_raw_key(
            &issuer,
            &invitee.public_key(),
            &meta,
            "zz", // not hex at all — fails hex::decode before the length gate
            "n301",
            Some(NOW + 100),
            NOW,
        )
        .unwrap();
        assert!(
            matches!(open_invite(&invitee, &bad), Err(HbError::InvalidEncryptedMessage)),
            "a garbage topic_key is refused by the deterministic open itself, so the poller's negative cache remembers the wrap"
        );
    }

    /// QURATOR-301 — the policy-half ordering, pinned WHERE IT LIVES. With the gate now also in
    /// `open_invite`, the composed test above is shielded by the open-side gate (a malformed key
    /// never reaches the policy half through `redeem_invite`), so only a DIRECT call of
    /// `redeem_opened_invite` can still observe the decode-before-insert order inside the policy
    /// half — and in-module construction is the only surface that can get a payload here without
    /// passing the open gate (the `InvitePayload` fields are module-private and there is no public
    /// constructor). Unlike the composed test, both payloads use the SAME nonce: the genuine one
    /// claims the exact `(issuer, topic_id, invitee, nonce)` slot the malformed one tried for.
    ///
    /// MUTATION (P-10, resolve by LINE NUMBER): in `redeem_opened_invite`, MOVE line 1111,
    /// `    let key_bytes: [u8; 32] = decode_topic_key(&payload)?;` (the occurrence inside
    /// `redeem_opened_invite`) to immediately AFTER the closing brace of the
    /// `if !payload.reusable { … }` block that starts at line 1116. The malformed payload now
    /// inserts its seen-key and only THEN fails the decode: `seen.is_empty()` reds, and the
    /// genuine payload is refused as a replay, redding the final assert too. (Deleting the
    /// open-side gate at line 1054 alone does NOT red this test — that mutation belongs to
    /// `a_garbage_topic_key_fails_the_deterministic_open` above.)
    #[test]
    fn redeem_opened_invite_decodes_before_burning_the_seen_key() {
        let issuer = Identity::generate();
        let me = Identity::generate();
        let (meta, key) = private_topic();
        let malformed = InvitePayload {
            meta: meta.clone(),
            topic_key: "zz".into(),
            nonce: "n301".into(),
            expires_at: Some(NOW + 100),
            reusable: false,
            schema_v: SCHEMA_V,
            crypto_v: CRYPTO_V,
        };
        let mut seen = NonceSet::new();
        assert!(
            matches!(
                redeem_opened_invite(&me, issuer.public_key(), malformed, &mut seen, NOW, None, None),
                Err(HbError::InvalidEncryptedMessage)
            ),
            "the policy half still refuses a malformed topic_key at its own gate"
        );
        assert!(seen.is_empty(), "a malformed-key payload must not burn the seen-key inside the policy half");
        // The genuine payload for the SAME (issuer, topic_id, invitee, nonce) slot still redeems.
        let genuine = InvitePayload {
            meta,
            topic_key: hex::encode(key.0),
            nonce: "n301".into(),
            expires_at: Some(NOW + 100),
            reusable: false,
            schema_v: SCHEMA_V,
            crypto_v: CRYPTO_V,
        };
        assert!(
            redeem_opened_invite(&me, issuer.public_key(), genuine, &mut seen, NOW, None, None).is_ok(),
            "the genuine payload redeems because the malformed one never claimed the seen-key"
        );
    }

    #[test]
    fn hostile_wrap_cannot_burn_another_issuers_single_use_invite_slot() {
        // QURATOR-198: the seen-set key must bind the VERIFIED issuer (+ payload nonce), not the
        // payload's self-declared topic_id alone. Mallory mints a perfectly-valid invite of her own
        // (her seal signature, her topic key) that DECLARES the genuine topic_id and names victim B
        // as invitee — both are knowable, so this wrap is fully constructible. Every crypto check in
        // redeem_invite passes it BY DESIGN; the returned issuer is what a caller consent-gates on.
        // What must NOT happen is the seen-set side effect: under the old topic_id-only key,
        // redeeming the hostile wrap first burned the "T:B" slot, so the genuine invite later
        // failed "already redeemed". With the issuer-bound key, Mallory's wrap occupies only
        // Mallory's own slot.
        //
        // MUTATION that reds this test: revert `invite_seen_key` to the topic_id-only form
        // (`format!("{topic_id}:{}", invitee.to_hex())`, ignoring issuer and nonce) — the genuine
        // redemption below then fails with "invite already redeemed (replay)".
        let genuine_issuer = Identity::generate();
        let mallory = Identity::generate();
        let invitee = Identity::generate();
        let (meta, key) = private_topic(); // T: the genuine topic
        let hostile_key = TopicKey::generate();

        // The hostile wrap: validly signed BY MALLORY, declaring the genuine (T, B).
        let hostile =
            mint_invite(&mallory, &invitee.public_key(), &meta, &hostile_key, "m1", Some(NOW + 100), NOW).unwrap();
        // The genuine invite from the real issuer: same declared topic and invitee, different issuer.
        let genuine =
            mint_invite(&genuine_issuer, &invitee.public_key(), &meta, &key, "g1", Some(NOW + 100), NOW).unwrap();

        // Hostile-first redemption (the blind private-redeem path). It succeeds — it IS a valid
        // invite — but it substitutes nothing: MALLORY is surfaced as the issuer and MALLORY's key
        // is returned, so the caller can refuse whose key it got; only Mallory's own seen slot is
        // claimed.
        let mut seen = NonceSet::new();
        let (h_meta, h_key, h_issuer) = redeem_invite(&invitee, &hostile, &mut seen, NOW, None, None).unwrap();
        assert_eq!(h_issuer, mallory.public_key(), "the hostile wrap's issuer is surfaced as Mallory");
        assert_ne!(h_key.0, key.0, "the hostile wrap carries Mallory's key, not the genuine one");
        assert_eq!(h_meta.topic_id, meta.topic_id, "the hostile wrap did declare the genuine topic_id");

        // THE PIN: the genuine invite still redeems afterwards — the hostile wrap could not burn
        // its slot — and returns the genuine issuer and genuine key.
        let (g_meta, g_key, g_issuer) = redeem_invite(&invitee, &genuine, &mut seen, NOW, None, None).unwrap();
        assert_eq!(g_issuer, genuine_issuer.public_key(), "the genuine issuer is recovered from the seal");
        assert_eq!(g_key.0, key.0, "the genuine topic key is returned");
        assert_eq!(g_meta.topic_id, meta.topic_id);

        // Single-use still holds PER INVITE: replaying the genuine event (same issuer + nonce ⇒
        // same seen-key) is refused, and so is replaying the hostile one.
        assert!(
            redeem_invite(&invitee, &genuine, &mut seen, NOW, None, None).is_err(),
            "a genuine invite still cannot be replayed"
        );
        assert!(
            redeem_invite(&invitee, &hostile, &mut seen, NOW, None, None).is_err(),
            "the hostile invite is itself single-use too"
        );
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
        let (rmeta, rkey, _issuer) = redeem_invite(&joiner_view, &cred, &mut NonceSet::new(), NOW, None, None).unwrap();
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

    #[test]
    fn staggered_post_expiry_drops_only_the_post_older_than_24h() {
        // W8.1 diagnosis (M17 devtest item 1): three posts stamped now-23h, now-25h, now-1h, read at
        // a SINGLE `now`. The local filter runs PER POST on its authenticated `payload.ts`, so exactly
        // the 23h and 1h posts survive — there is no day-aligned collective wipe and no scheduled
        // boundary anywhere in the path. If this test is green, the owner's "posts wipe at a set time"
        // mental model is a COPY defect (chat/+page.svelte), not an engine bug.
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let now = NOW;
        let post_23h = seal_post(&key, &meta.topic_id, &author, "23h old", now - 23 * 60 * 60).unwrap();
        let post_25h = seal_post(&key, &meta.topic_id, &author, "25h old", now - 25 * 60 * 60).unwrap();
        let post_1h = seal_post(&key, &meta.topic_id, &author, "1h old", now - 60 * 60).unwrap();
        let survivors: Vec<String> = [&post_23h, &post_25h, &post_1h]
            .into_iter()
            .filter_map(|ev| open_post(&key, ev, now).ok().flatten())
            .map(|p| p.body)
            .collect();
        assert_eq!(
            survivors,
            vec!["23h old".to_string(), "1h old".to_string()],
            "only the >24h post is dropped — expiry is per-message, not a collective wipe"
        );
        // The same per-event filter powers the multi-domain channel reader (`open_channel_item` at
        // :791, the checks cited at :809/:824); confirm it agrees with `open_post`.
        let channel_survivors: Vec<String> = [&post_23h, &post_25h, &post_1h]
            .into_iter()
            .filter_map(|ev| open_channel_item(&key, ev, now).ok().flatten())
            .map(|item| match item {
                ChannelItem::Post(p) => p.body,
                ChannelItem::Announce(a) => a.body,
            })
            .collect();
        assert_eq!(
            channel_survivors,
            vec!["23h old".to_string(), "1h old".to_string()],
            "open_channel_item agrees — the per-event 24h filter is the one mechanism"
        );
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
        // QURATOR-292 rewrite: the verifier can no longer re-derive the secret-keyed pseudonym, so the
        // npub↔pubkey binding now rides the v2 proof — the member's real key must have signed
        // `hbm:join2:{topic_id}:{event.pubkey}`. An insider can still sign with a pseudonym of their
        // OWN (member_sign_keys on their own npub — that derivation survives for posts), but it binds
        // nothing: every way of claiming the victim's npub under the attacker's pubkey is rejected.
        let (meta, key) = public_topic();
        let victim = Identity::generate();
        let attacker = Identity::generate();
        // A pseudonym the insider CAN produce: the old public tweak over their OWN npub.
        let attacker_signer = member_sign_keys(&key, &attacker.public_key()).unwrap();

        let forge = |proof: String, signer: &Keys| {
            let payload = serde_json::to_string(&MemberPayload {
                member_npub: victim.npub(),
                joined_at: NOW,
                proof,
            })
            .unwrap();
            EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &payload).unwrap())
                .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
                .custom_created_at(Timestamp::from(NOW))
                .sign_with_keys(signer)
                .unwrap()
        };

        // (a) Claim the victim's npub under the attacker's pseudonym, no proof → rejected (the proof
        // must be a real-key signature naming the ATTACKER's pubkey — the victim never made one).
        assert!(
            open_membership(&key, &forge(String::new(), &attacker_signer)).is_err(),
            "claiming the victim's npub under the attacker's own pseudonym is rejected"
        );

        // (b) Steal the victim's GENUINE v2 proof and re-wrap it in an event signed by the attacker's
        // pseudonym → rejected: the genuine proof names the VICTIM's pseudonym, the statement the
        // reader rebuilds names the attacker's (event.pubkey), so the commitment does not match.
        let genuine = seal_membership(&key, &meta.topic_id, &victim, NOW).unwrap();
        let plain = topic_decrypt(&key, MEMBERSHIP_DOMAIN, CRYPTO_V, &genuine.content).unwrap();
        let genuine_payload: MemberPayload = serde_json::from_slice(&plain).unwrap();
        assert!(
            open_membership(&key, &forge(genuine_payload.proof, &attacker_signer)).is_err(),
            "a genuine proof re-wrapped under the attacker's pseudonym is rejected (it names the victim's pseudonym)"
        );

        // (c) DOWNGRADE GUARD: a v1-shape proof (`hbm:join:{topic_id}`, names NO pseudonym) genuinely
        // signed by the victim's real key at the right time — if the reader ever rebuilt the v1
        // statement instead of the v2 one, this would OPEN and enrol the victim at the attacker's
        // coordinate. The distinct `join2` prefix is what rejects it.
        let v1_proof = build_proof(&victim, &format!("{PROOF_JOIN_PREFIX}{}", meta.topic_id), NOW).unwrap();
        assert!(
            open_membership(&key, &forge(v1_proof.as_json(), &attacker_signer)).is_err(),
            "a v1 statement (no pseudonym commitment) must be rejected by the v2-only reader"
        );

        // P-10 mutation anchor (orchestrator runs it): topic.rs line 716 — the
        // `&membership_statement_v2(&topic_id_of(event)?, &event.pubkey),` argument in
        // `open_membership`. Revert it to the v1 shape (drop the `&event.pubkey` commitment / use
        // PROOF_JOIN_PREFIX) and sub-case (c) turns GREEN-where-it-must-be-RED, failing this test.
        // Sub-cases (a)/(b) stay red under that mutation, so (c) is the discriminator.
    }

    #[test]
    fn redeem_rejects_non_gift_wrap_and_foreign_inner_kind() {
        let me = Identity::generate();
        let note = me.sign(EventBuilder::new(Kind::TextNote, "hi")).unwrap();
        assert!(matches!(redeem_invite(&me, &note, &mut NonceSet::new(), NOW, None, None), Err(HbError::InvalidEvent(_))));
    }

    // ───────────────────── chorus-1: insider forgery + reusable public-join + future ts ─────────────────────

    #[test]
    fn insider_cannot_forge_membership_for_a_never_joiner() {
        // QURATOR-292 rewrite. The OLD attack, replayed exactly: a topic-key holder derives the
        // victim's pseudonym from the PUBLIC npub via the old HMAC tweak and (a) enrols a never-joiner
        // or (b) — worse, since kind 31118 is ADDRESSABLE — publishes junk at
        // `(P_victim, 31118, topic_id)` with a later `created_at`, which every NIP-01 relay honours
        // as a supersession. The new derivation is over the member's SECRET, so the old attack's key
        // no longer reaches the victim's coordinate at all — and the event it does produce is
        // rejected on the v2 proof. (Vector 1, NIP-09 retraction, needs the same pseudonym key and
        // dies with it: an insider holding (topic_key, victim npub) can no longer sign AS the victim.)
        let (meta, key) = public_topic();
        let victim = Identity::generate(); // never joined; the insider knows only the public npub
        let insider = Identity::generate();
        let old_attack_key = member_sign_keys(&key, &victim.public_key()).unwrap(); // derivable by any keyholder — the OLD hole

        // (a) The old attack, no proof at all → reject.
        let no_proof = serde_json::to_string(&MemberPayload {
            member_npub: victim.npub(),
            joined_at: NOW,
            proof: String::new(),
        })
        .unwrap();
        let forged = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &no_proof).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW + 1)) // later created_at: the supersession shape
            .sign_with_keys(&old_attack_key)
            .unwrap();
        assert!(open_membership(&key, &forged).is_err(), "an insider with no real-key proof cannot enrol the victim");

        // (b) A proof the INSIDER signs (with their own key, claiming to be the victim) → reject: the
        // proof is not signed by the victim's real key.
        let bogus_proof = build_proof(&insider, &membership_statement_v2(&meta.topic_id, &forged.pubkey), NOW).unwrap();
        let with_bogus = serde_json::to_string(&MemberPayload {
            member_npub: victim.npub(),
            joined_at: NOW,
            proof: bogus_proof.as_json(),
        })
        .unwrap();
        let forged2 = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &with_bogus).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW + 1))
            .sign_with_keys(&old_attack_key)
            .unwrap();
        assert!(open_membership(&key, &forged2).is_err(), "a proof not signed by the victim's real key is rejected");

        // (c) THE VECTOR-2 DISCRIMINATOR: the coordinate the old attack reaches is NOT the coordinate
        // the member's real membership lives at. Under the old derivation these were EQUAL — that is
        // what made the addressable-kind supersession (and the NIP-09 retraction) constructible. If
        // this ever regresses to equality, the eviction hole is back regardless of the proof checks.
        let genuine = seal_membership(&key, &meta.topic_id, &victim, NOW).unwrap();
        assert_ne!(
            forged.pubkey, genuine.pubkey,
            "the publicly-derivable key must not reach the member's membership coordinate"
        );
        assert_ne!(
            member_sign_keys(&key, &victim.public_key()).unwrap().public_key(),
            genuine.pubkey,
            "no (topic_key, npub)-derived value equals the secret-keyed membership pseudonym"
        );

        // And the victim's OWN membership opens fine (positive control).
        assert_eq!(open_membership(&key, &genuine).unwrap().member, victim.public_key());

        // P-10 mutation anchor (orchestrator runs it): topic.rs line 673 —
        // `let signer = membership_sign_keys(key, topic_id, member)?;` in `seal_membership`. Revert
        // it to `member_sign_keys(key, &member_pk)?` and sub-case (c)'s `assert_ne!` pair turns RED
        // (the forged coordinate becomes the genuine one, i.e. the supersession is constructible
        // again), while the proof-based asserts in (a)/(b) would stay green, so (c) is the
        // discriminator for the derivation swap itself.
    }

    /// QURATOR-292 migration ruling: the reader verifies v2 ONLY — republish-on-next-launch, NO
    /// dual-read, because accepting v1 anywhere re-opens the exact hole this closes (a v1 proof
    /// names no pseudonym, so it is wrappable in an attacker-signed event). This test rebuilds,
    /// byte-faithfully, what the pre-QURATOR-292 `seal_membership` put on relays — the old
    /// HMAC pseudonym as `event.pubkey` + the member's own genuine `hbm:join:{topic_id}` proof —
    /// and pins that the v2-only reader drops it until the member republishes.
    ///
    /// P-10 mutation anchor (orchestrator runs it): topic.rs line 716 — the
    /// `&membership_statement_v2(&topic_id_of(event)?, &event.pubkey),` argument in
    /// `open_membership`. Make it fall back to the v1 shape (accept v2 OR
    /// `format!("{PROOF_JOIN_PREFIX}{topic_id}")`) — a dual-read. This test turns RED (the v1
    /// event opens), which is precisely the regression the ruling forbids.
    #[test]
    fn v1_membership_events_are_rejected_by_the_v2_only_reader() {
        let (meta, key) = public_topic();
        let member = Identity::generate();

        // The pre-QURATOR-292 shape: old public-tweak pseudonym, old statement (no pseudonym in it).
        let v1_signer = member_sign_keys(&key, &member.public_key()).unwrap();
        let v1_proof = build_proof(&member, &format!("{PROOF_JOIN_PREFIX}{}", meta.topic_id), NOW).unwrap();
        let v1_payload = serde_json::to_string(&MemberPayload {
            member_npub: member.npub(),
            joined_at: NOW,
            proof: v1_proof.as_json(),
        })
        .unwrap();
        let v1_event = EventBuilder::new(Kind::from_u16(KIND_TOPIC_MEMBER), topic_encrypt(&key, MEMBERSHIP_DOMAIN, &v1_payload).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&v1_signer)
            .unwrap();

        assert!(
            open_membership(&key, &v1_event).is_err(),
            "the v2-only reader rejects pre-migration membership events (republish-on-next-launch, no dual-read)"
        );

        // The member republishes through the current production path → accepted again.
        let republished = seal_membership(&key, &meta.topic_id, &member, NOW).unwrap();
        assert_eq!(open_membership(&key, &republished).unwrap().member, member.public_key());
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
        assert!(redeem_invite(&joiner, &cred, &mut seen, NOW, None, None).is_ok(), "first public-join redeem ok");
        assert!(redeem_invite(&joiner, &cred, &mut seen, NOW, None, None).is_ok(), "public-join is reusable (not consumed)");
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
        assert!(redeem_invite(&invitee, &inv, &mut seen, NOW, None, None).is_ok(), "first redeem ok");
        assert!(
            redeem_invite(&invitee, &inv, &mut seen, NOW, None, None).is_err(),
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
        assert!(matches!(redeem_invite(&invitee, &inv, &mut NonceSet::new(), NOW, None, None), Err(HbError::InvalidSignature)));
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

    // ───────────────────────── M13 Part A: ANNOUNCE (member broadcast) ─────────────────────────
    // Not the discovery ANNOUNCE (`TopicMeta`/`build_announce`/plaintext, kind 31117) — this is a
    // member's broadcast to the roster, riding the SAME channel kind (1117) as an ordinary post,
    // distinguished only by the ciphertext domain byte (0x03). Negatives first, per convention.

    #[test]
    fn post_ciphertext_rejected_by_open_announce_and_vice_versa() {
        // A post and a broadcast share the SAME kind (1117); only the ciphertext's domain byte (0x02
        // vs 0x03) tells them apart. Feeding one opener the other's event must fail on the domain
        // byte (F17, extended) — even with the RIGHT key, even though the kind check passes for both.
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let post = seal_post(&key, &meta.topic_id, &author, "hi", NOW).unwrap();
        let announce = seal_announce(&key, &meta.topic_id, &author, "hear ye", NOW).unwrap();
        assert!(matches!(open_announce(&key, &post, NOW), Err(HbError::InvalidEvent(_))), "a post ciphertext is not an announce");
        assert!(matches!(open_post(&key, &announce, NOW), Err(HbError::InvalidEvent(_))), "an announce ciphertext is not a post");
    }

    #[test]
    fn a_posts_proof_cannot_be_promoted_to_an_announce() {
        // THE mandatory negative: a topic-key holder decrypts a member's ordinary post, re-encrypts the
        // SAME plaintext (author/body/ts/proof unchanged) under the ANNOUNCE domain byte, and re-signs
        // with the same derivable pseudonym — trying to "promote" a post into an announce the member
        // never broadcast. The proof binds `hbm:post:{topic}:{sha256(body)}`; `open_announce` checks it
        // against `hbm:announce:{topic}:{sha256(body)}` — a DIFFERENT statement — so verification fails.
        // A shared proof prefix would NOT catch this forgery; the distinct prefix does.
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let real_post = seal_post(&key, &meta.topic_id, &author, "just a normal post", NOW).unwrap();
        let plain = topic_decrypt(&key, POST_DOMAIN, CRYPTO_V, &real_post.content).unwrap();
        let payload: PostPayload = serde_json::from_slice(&plain).unwrap();
        let signer = member_sign_keys(&key, &author.public_key()).unwrap();
        let promoted_payload = serde_json::to_string(&AnnounceMsgPayload {
            author_npub: payload.author_npub,
            body: payload.body,
            ts: payload.ts,
            proof: payload.proof, // the POST proof, unchanged — the forgery attempt
        })
        .unwrap();
        let promoted = EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), topic_encrypt(&key, ANNOUNCE_DOMAIN, &promoted_payload).unwrap())
            .tags([
                Tag::identifier(meta.topic_id.clone()),
                Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
                Tag::expiration(Timestamp::from(NOW + POST_TTL_SECS)),
            ])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(
            open_announce(&key, &promoted, NOW).is_err(),
            "a post's proof must not verify as an announce proof — the distinct hbm:announce: prefix stops the promotion"
        );
    }

    #[test]
    fn announce_pubkey_is_pseudonym_no_real_npub_in_raw_event() {
        // B2, restated for the broadcast: event.pubkey is the derived pseudonym; the real npub is
        // ONLY inside the topic_key-encrypted content.
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let ev = seal_announce(&key, &meta.topic_id, &author, "attention room", NOW).unwrap();
        let expected_pseudonym = member_sign_keys(&key, &author.public_key()).unwrap().public_key();
        assert_eq!(ev.pubkey, expected_pseudonym, "event.pubkey is the derived pseudonym");
        assert_ne!(ev.pubkey, author.public_key(), "event.pubkey is NOT the real npub");
        let json = ev.as_json();
        assert!(!json.contains(&author.public_key().to_hex()), "real npub hex must not leak in the raw event");
        assert!(!json.contains(&author.npub()), "real npub bech32 must not leak in the raw event");
    }

    #[test]
    fn non_member_cannot_open_announce() {
        // No key → ciphertext only → Err. (The wrong-key holder stands in for a non-member.)
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let ev = seal_announce(&key, &meta.topic_id, &author, "hi", NOW).unwrap();
        let (_other_meta, wrong_key) = new_topic("other/unrelated-announce", "", vec![], false).unwrap();
        assert!(matches!(open_announce(&wrong_key, &ev, NOW), Err(HbError::DecryptionFailed)));
    }

    #[test]
    fn keyholder_cannot_forge_announce_as_another_member() {
        // Mirrors insider_cannot_impersonate_another_in_the_channel: a key-holder cannot broadcast as
        // another member — the announce's real-key proof binds the author + body.
        let (meta, key) = public_topic();
        let victim = Identity::generate();
        let signer = member_sign_keys(&key, &victim.public_key()).unwrap();
        let payload = serde_json::to_string(&AnnounceMsgPayload {
            author_npub: victim.npub(),
            body: "I never said this".into(),
            ts: NOW,
            proof: String::new(),
        })
        .unwrap();
        let forged = EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), topic_encrypt(&key, ANNOUNCE_DOMAIN, &payload).unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(open_announce(&key, &forged, NOW).is_err(), "a key-holder cannot forge an announce as another member");
    }

    #[test]
    fn future_dated_announce_is_dropped() {
        // Mirrors future_dated_post_is_dropped_not_pinned_forever, for the broadcast.
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let future = NOW + 10 * POST_TTL_SECS;
        let ev = seal_announce(&key, &meta.topic_id, &author, "pinned?", future).unwrap();
        assert!(open_announce(&key, &ev, NOW).unwrap().is_none(), "a wildly future-dated announce is dropped");
    }

    #[test]
    fn announce_older_than_24h_filtered_locally() {
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let ev = seal_announce(&key, &meta.topic_id, &author, "stale announce", NOW).unwrap();
        let later = NOW + POST_TTL_SECS + 1;
        assert!(open_announce(&key, &ev, later).unwrap().is_none(), "a >24h announce is filtered to None");
        assert!(open_announce(&key, &ev, NOW + POST_TTL_SECS).unwrap().is_some(), "an announce at the 24h boundary still opens");
    }

    #[test]
    fn announce_carries_nip40_expiration_at_now_plus_ttl() {
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let ev = seal_announce(&key, &meta.topic_id, &author, "criterion sale!", NOW).unwrap();
        let exp = ev.tags.find(TagKind::Expiration).and_then(|t| t.content()).and_then(|s| s.parse::<u64>().ok());
        assert_eq!(exp, Some(NOW + POST_TTL_SECS), "the announce carries a NIP-40 expiration at +24h, same as a post");
    }

    #[test]
    fn announce_roundtrip_recovers_author_body_ts() {
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let ev = seal_announce(&key, &meta.topic_id, &author, "criterion sale!", NOW).unwrap();
        let opened = open_announce(&key, &ev, NOW).unwrap().unwrap();
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.body, "criterion sale!");
        assert_eq!(opened.ts, NOW);
    }

    #[test]
    fn open_channel_item_partitions_post_and_announce() {
        let (meta, key) = public_topic();
        let author = Identity::generate();
        let post = seal_post(&key, &meta.topic_id, &author, "a post", NOW).unwrap();
        let announce = seal_announce(&key, &meta.topic_id, &author, "an announce", NOW).unwrap();
        match open_channel_item(&key, &post, NOW).unwrap() {
            Some(ChannelItem::Post(p)) => assert_eq!(p.body, "a post"),
            other => panic!("expected a Post variant, got {other:?}"),
        }
        match open_channel_item(&key, &announce, NOW).unwrap() {
            Some(ChannelItem::Announce(a)) => assert_eq!(a.body, "an announce"),
            other => panic!("expected an Announce variant, got {other:?}"),
        }
        // Junk / unknown domain content → Err (forward-compat refusal), never silently mis-typed.
        let signer = member_sign_keys(&key, &author.public_key()).unwrap();
        let junk = EventBuilder::new(Kind::from_u16(KIND_TOPIC_POST), topic_encrypt(&key, 0x7f, "{}").unwrap())
            .tags([Tag::identifier(meta.topic_id.clone()), Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()])])
            .custom_created_at(Timestamp::from(NOW))
            .sign_with_keys(&signer)
            .unwrap();
        assert!(open_channel_item(&key, &junk, NOW).is_err(), "an unrecognised domain byte is a forward-compat Err");
    }

    #[test]
    fn announce_cooldown_remaining_boundaries() {
        // None ⇒ never announced ⇒ no cooldown.
        assert_eq!(announce_cooldown_remaining(None, NOW), 0);
        // Just under the interval: 1s left.
        assert_eq!(announce_cooldown_remaining(Some(NOW), NOW + ANNOUNCE_MIN_INTERVAL_SECS - 1), 1);
        // Exactly at the interval: cooldown over.
        assert_eq!(announce_cooldown_remaining(Some(NOW), NOW + ANNOUNCE_MIN_INTERVAL_SECS), 0);
        // Past the interval: still 0 (not negative/wrapped).
        assert_eq!(announce_cooldown_remaining(Some(NOW), NOW + ANNOUNCE_MIN_INTERVAL_SECS + 100), 0);
        // Clock rollback: `now` before `last_announce_at` reads as MORE cooldown remaining, never a
        // bypass (saturating arithmetic, not a wrap to a huge/negative number misread as "go ahead").
        let rolled_back_now = NOW - 1_000;
        assert_eq!(
            announce_cooldown_remaining(Some(NOW), rolled_back_now),
            ANNOUNCE_MIN_INTERVAL_SECS + 1_000,
            "a clock rollback reads as still cooling down, never a bypass"
        );
    }
}
