//! Private-collection seal (M10; spec §Private Collections) — **per-recipient, gift-wrapped,
//! enumeration-resistant**. The public path (`listing` / `event`) encrypts a listing once under the
//! shared **browse-key**; this module is the PRIVATE path: a listing sealed individually to each
//! trusted `npub`, which the browse-key explicitly **cannot** open.
//!
//! **The crypto contract (M10 Decisions A–D):**
//! ```text
//! 1. CEK  = fresh random 32-byte content-encryption key (per publish)
//! 2. body = SYMENC(CEK, listing_json)                      // HKDF-symmetric, listing::encrypt_with_cek
//!                                                          //   (A': NOT a raw NIP-44_encrypt(CEK,…))
//! 3. for each trusted npub R:
//!      wrap_R = NIP-44( ecdh(author, R), {CEK, schema_v, kdf_v} )   // genuine ECDH; CEK wrapped to R
//!      inner  = unsigned {kind=KIND_PRIV_LISTING, content=(body, wrap_R), tags=[hb-v, hb-cv]}
//!      seal_R = NIP-59 seal of `inner` to R, signed by the author
//!      wrap   = gift-wrap (1059) of seal_R, signed by a FRESH ephemeral key (F15), p-tag = R
//! ```
//! The body ciphertext is shared across recipients (one CEK — efficient for large listings); the
//! per-recipient CEK wrap and the per-recipient seal/gift-wrap are unique. The NIP-59 seal + 1059
//! gift-wrap are the *same* primitive the DM path uses (`hb-net::dm`) — reused, never re-derived.
//!
//! **Negatives this enforces (test them first, hardest):** the browse-key cannot open a private
//! listing (no `BrowseKey` is representable here — F6; the body is CEK-keyed, domain-separated from
//! the browse-key — see `listing::cek_and_browse_key_are_domain_separated`); a non-recipient gets a
//! reasoned `Err`; the outer 1059 author is the **ephemeral** key, never the real `npub`, and N
//! recipients yield N **distinct** ephemeral authors (F15 unlinkability); a bumped version is
//! recognised, never silently mis-decrypted (Decision D); every malformed/tampered/foreign input is
//! a clean `Err`, never a panic.

use nostr::nips::nip44;
use nostr::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::HbError;
use crate::identity::Identity;
use crate::listing::{decrypt_with_cek, encrypt_with_cek, ContentKey};
use crate::version::{check_crypto, check_schema, CRYPTO_V, SCHEMA_V};

/// Inner private-listing kind — **provisional** (Open Q#3: hard kind registration deferred). It is
/// carried only *inside* the NIP-59 seal (never published as a top-level event), so it occupies no
/// relay replaceable slot; it sits beside `KIND_LISTING` (31_111) in the app's range as a
/// recognisable discriminant. The **outer** wrap is the standard NIP-59 gift-wrap, kind **1059**.
pub const KIND_PRIV_LISTING: u16 = 31_113;

const TAG_SCHEMA: &str = "hb-v";
const TAG_CRYPTO: &str = "hb-cv";

/// The opened private listing: the decrypted listing JSON, the **inner** author (the real signer,
/// recovered from inside the *verified* seal — NOT the ephemeral outer-wrap author), and the inner
/// rumor's `created_at` (the true publish time; the outer wrap time is randomised by NIP-59). The
/// caller filters `inner_author` against its trusted allowlist **post-decrypt** — the outer author
/// is ephemeral, so a relay-side author filter is impossible (mirrors the NIP-17 sender-block rule).
#[derive(Debug, Clone)]
pub struct OpenedPrivate {
    pub listing_json: String,
    pub inner_author: PublicKey,
    pub created_at: u64,
}

/// The CEK wrapped to one recipient (genuine ECDH NIP-44), carrying the version discriminants so a
/// future-version blob is *recognised*, never silently mis-decrypted (Decision D).
#[derive(Serialize, Deserialize)]
struct CekWrap {
    /// Hex of the 32-byte content-encryption key.
    cek: String,
    schema_v: u8,
    kdf_v: u8,
}

/// The inner event's content: the once-encrypted body (shared CEK) + the per-recipient CEK wrap.
#[derive(Serialize, Deserialize)]
struct PrivContent {
    /// base64 SYMENC(CEK, listing_json) — identical across recipients (Decision A; the m5 residual:
    /// two recipients who compare their *decrypted-seal* bodies can confirm a shared collection).
    body: String,
    /// base64 NIP-44(ecdh(author, R), CekWrap) — unique per recipient.
    wrap: String,
}

/// Seal a private listing to each trusted recipient (M10). Returns **N** gift-wrapped (kind 1059)
/// events — one per recipient, each signed by its **own fresh ephemeral key** (F15: the N wraps are
/// mutually unlinkable) and `p`-tagged to that recipient only.
///
/// **There is deliberately NO `BrowseKey` parameter (F6).** A private listing is *never* wrappable
/// under the browse-key; the type makes that unrepresentable. The body is sealed under a fresh
/// random CEK; the CEK is then wrapped to each recipient via genuine ECDH NIP-44.
pub fn seal_private_listing(
    author: &Identity,
    recipients: &[PublicKey],
    listing_json: &str,
    now: u64,
) -> Result<Vec<Event>, HbError> {
    // Fresh CEK per publish — two publishes of the same listing produce different ciphertext.
    let cek: ContentKey = rand::random();
    let body = encrypt_with_cek(&cek, listing_json)?;

    let author_sk = author.keys().secret_key();
    let mut wraps = Vec::with_capacity(recipients.len());
    for r in recipients {
        // CEK wrapped to R via genuine ECDH NIP-44 (the body-symmetric primitive is NOT used here).
        let cek_wrap = serde_json::to_string(&CekWrap {
            cek: hex::encode(cek),
            schema_v: SCHEMA_V,
            kdf_v: CRYPTO_V,
        })?;
        let wrapped_cek = nip44::encrypt(author_sk, r, cek_wrap, nip44::Version::V2)
            .map_err(|e| HbError::Nostr(e.to_string()))?;

        let content = serde_json::to_string(&PrivContent { body: body.clone(), wrap: wrapped_cek })?;

        // Inner rumor: the real author's unsigned KIND_PRIV_LISTING event — version tags (F18) +
        // the true created_at (the dedup key on the read side).
        let rumor: UnsignedEvent = EventBuilder::new(Kind::from_u16(KIND_PRIV_LISTING), content)
            .tags([
                Tag::custom(TagKind::custom(TAG_SCHEMA), [SCHEMA_V.to_string()]),
                Tag::custom(TagKind::custom(TAG_CRYPTO), [CRYPTO_V.to_string()]),
            ])
            .custom_created_at(Timestamp::from(now))
            .build(author.public_key());

        // Seal (NIP-59 kind 13): the rumor JSON encrypted author→R, signed by the author. Built
        // synchronously (the async `EventBuilder::seal` only wraps these same primitives) so this
        // whole module stays pure + synchronous, like the rest of hb-core.
        let seal_content = nip44::encrypt(author_sk, r, rumor.as_json(), nip44::Version::V2)
            .map_err(|e| HbError::Nostr(e.to_string()))?;
        let seal = EventBuilder::new(Kind::Seal, seal_content)
            .custom_created_at(Timestamp::from(now))
            .sign_with_keys(author.keys())
            .map_err(|e| HbError::Nostr(e.to_string()))?;

        // Gift-wrap (kind 1059): `gift_wrap_from_seal` generates a FRESH ephemeral key per call and
        // signs the wrap with it, p-tagged to R. So N recipients ⇒ N distinct ephemeral authors —
        // an observer can't see "one publisher sent N wraps at once" (F15).
        let wrap = EventBuilder::gift_wrap_from_seal(r, &seal, [])
            .map_err(|e| HbError::Nostr(e.to_string()))?;
        wraps.push(wrap);
    }
    Ok(wraps)
}

/// Open a gift-wrapped private listing addressed to `me`: unwrap the 1059 → verify + decrypt the
/// seal → recover the inner rumor + its **real author** → decrypt the CEK wrap (ECDH me↔author) →
/// decrypt the body. Returns the listing JSON + the **inner author** (the verified seal signer).
///
/// ⚠️ **SECURITY — the caller MUST check `inner_author` against a trusted allowlist before acting on
/// `listing_json`.** Anyone who can NIP-59-seal to `me` can produce a wrap this opens; the returned
/// `inner_author` is *cryptographically authentic* (it is `seal.pubkey`, validated by `seal.verify()`)
/// but it is **not** filtered here — a stranger's listing opens successfully and is returned. The
/// `hb-net::fetch_private_listings` seam does the `allowlist.contains(inner_author)` gate
/// post-decrypt; a future direct caller that skips it would accept unsolicited content from any
/// sender. (A wrapping `Unverified<…>` type could make this a compile-time obligation — deferred.)
///
/// Every failure is a reasoned `Err`, **never a panic**: not a gift-wrap, not addressed to us (ECDH
/// mismatch), tampered/forged seal signature, wrong inner kind, version mismatch, malformed or
/// truncated ciphertext — all rejected cleanly (the Suite-AB fuzz mandate).
pub fn open_private_listing(me: &Identity, wrap: &Event) -> Result<OpenedPrivate, HbError> {
    if wrap.kind != Kind::GiftWrap {
        return Err(HbError::InvalidEvent("not a NIP-59 gift wrap (kind 1059)".into()));
    }
    let me_sk = me.keys().secret_key();

    // (1) Outer: decrypt the 1059 content with ecdh(me, ephemeral outer author). A wrap not
    //     addressed to us derives the wrong shared secret → decrypt fails → clean Err.
    let seal_json =
        nip44::decrypt(me_sk, &wrap.pubkey, &wrap.content).map_err(|_| HbError::DecryptionFailed)?;
    let seal = Event::from_json(&seal_json).map_err(|e| HbError::InvalidEvent(e.to_string()))?;
    if seal.kind != Kind::Seal {
        return Err(HbError::InvalidEvent("inner event is not a NIP-59 seal".into()));
    }
    // (2) The seal is signed by the REAL author — verify the Schnorr sig (a tampered/forged seal
    //     fails here), so `seal.pubkey` is the cryptographically-authentic inner author. This is
    //     what makes the post-decrypt allowlist check sound: a non-trusted sender can seal to me,
    //     but cannot sign the seal as a trusted author.
    seal.verify().map_err(|_| HbError::InvalidSignature)?;
    let inner_author = seal.pubkey;

    // (3) Decrypt the seal content with ecdh(me, author) → the inner rumor JSON.
    let rumor_json = nip44::decrypt(me_sk, &inner_author, &seal.content)
        .map_err(|_| HbError::DecryptionFailed)?;
    let rumor =
        UnsignedEvent::from_json(&rumor_json).map_err(|e| HbError::InvalidEvent(e.to_string()))?;

    // NOTE: `inner_author` is `seal.pubkey` (the verified seal signer) — NEVER `rumor.pubkey`.
    // The rumor is an *unsigned* event, so its `pubkey` field is attacker-controlled and is
    // deliberately ignored here; only the Schnorr-verified seal author is trusted (opencode review).

    // (4) Inner kind pin — a wrong-kind inner after unwrap is rejected (event-confusion guard).
    if rumor.kind != Kind::from_u16(KIND_PRIV_LISTING) {
        return Err(HbError::InvalidEvent(format!(
            "expected private-listing kind {KIND_PRIV_LISTING}, got {}",
            rumor.kind.as_u16()
        )));
    }
    // (5) Inner version tags (F18) — recognise a future version, never mis-decrypt.
    let inner_schema = tag_u8_from(&rumor.tags, TAG_SCHEMA)
        .ok_or_else(|| HbError::InvalidEvent("inner event missing/malformed schema version".into()))?;
    check_schema(inner_schema)?;
    let inner_crypto = tag_u8_from(&rumor.tags, TAG_CRYPTO)
        .ok_or_else(|| HbError::InvalidEvent("inner event missing/malformed crypto version".into()))?;
    check_crypto(inner_crypto)?;

    // (6) Parse the inner content (shared body + per-recipient CEK wrap).
    let parsed: PrivContent = serde_json::from_str(&rumor.content)?;

    // (7) Decrypt the CEK wrap with ecdh(me, author) → CEK + its versions.
    let cek_wrap_json = nip44::decrypt(me_sk, &inner_author, &parsed.wrap)
        .map_err(|_| HbError::DecryptionFailed)?;
    let cek_wrap: CekWrap = serde_json::from_str(&cek_wrap_json)?;
    // The wrap's versions must be recognised AND match the *signed* inner tags (no silent downgrade).
    check_schema(cek_wrap.schema_v)?;
    check_crypto(cek_wrap.kdf_v)?;
    if cek_wrap.schema_v != inner_schema || cek_wrap.kdf_v != inner_crypto {
        return Err(HbError::InvalidEvent(
            "version mismatch between the CEK wrap and the signed inner tags".into(),
        ));
    }
    // `hex::decode` here resolves through the nostr prelude's hex re-export, so map its error
    // explicitly rather than relying on a `From` impl for that crate's error type.
    let cek: ContentKey = hex::decode(&cek_wrap.cek)
        .map_err(|_| HbError::InvalidEncryptedMessage)?
        .try_into()
        .map_err(|_| HbError::InvalidEncryptedMessage)?;

    // (8) Decrypt the body under the CEK at its declared version.
    let listing_json = decrypt_with_cek(&cek, cek_wrap.kdf_v, &parsed.body)?;

    Ok(OpenedPrivate { listing_json, inner_author, created_at: rumor.created_at.as_u64() })
}

/// Read a custom named tag from a rumor's `Tags` as a `u8` (None if absent or malformed). The
/// rumor is an `UnsignedEvent`, so `tag_util`'s `&Event` helpers don't apply — this is the one
/// inline accessor private to the seal path.
fn tag_u8_from(tags: &Tags, name: &str) -> Option<u8> {
    tags.find(TagKind::custom(name)).and_then(|t| t.content()).and_then(|s| s.parse::<u8>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::BrowseKey;

    const NOW: u64 = 1_700_000_000;
    const LISTING: &str =
        r#"{"slug":"vault","content_types":["video"],"entries":[{"name":"rare.mkv"}]}"#;

    /// Seal to a single recipient; return (author, recipient, the one wrap).
    fn seal_one() -> (Identity, Identity, Event) {
        let author = Identity::generate();
        let r = Identity::generate();
        let wraps = seal_private_listing(&author, &[r.public_key()], LISTING, NOW).unwrap();
        assert_eq!(wraps.len(), 1);
        (author, r, wraps.into_iter().next().unwrap())
    }

    // ───────────────────────── THE NEGATIVES (first, hardest) ─────────────────────────

    #[test]
    fn browse_key_holder_cannot_open_a_private_listing() {
        // THE HEADLINE FAILURE MODE: a share-code holder (who holds the author's browse-key but is
        // NOT a trusted recipient) cannot read a private collection. `open_private_listing` keys on
        // the *recipient's identity*, not on any browse-key — a non-recipient identity gets Err,
        // and there is no API path by which the browse-key opens it (F6, below).
        let (_author, _r, wrap) = seal_one();
        let share_code_holder = Identity::generate(); // holds a browse-key, but is not the recipient
        let _account_browse_key: BrowseKey = rand::random(); // exists, but is unrepresentable to `open`
        assert!(
            open_private_listing(&share_code_holder, &wrap).is_err(),
            "a browse-key / share-code holder who is not a recipient must NOT open a private listing"
        );
    }

    #[test]
    fn seal_takes_no_browse_key_only_recipient_pubkeys() {
        // F6 (seal-side, type-level): `seal_private_listing` accepts only `&[PublicKey]` recipients
        // — a `BrowseKey` ([u8;32]) cannot be passed as a wrapping key (it is not a `PublicKey`).
        // This compiles *because* the signature is recipient-pubkeys-only; the assertion documents
        // the invariant. The body's domain separation from the browse-key is proven in
        // `listing::cek_and_browse_key_are_domain_separated`.
        let author = Identity::generate();
        let r = Identity::generate();
        let recipients: &[PublicKey] = &[r.public_key()];
        let wraps = seal_private_listing(&author, recipients, LISTING, NOW).unwrap();
        assert_eq!(wraps.len(), 1);
    }

    #[test]
    fn non_recipient_cannot_open() {
        let (_author, _r, wrap) = seal_one();
        let stranger = Identity::generate();
        assert!(matches!(
            open_private_listing(&stranger, &wrap),
            Err(HbError::DecryptionFailed)
        ));
    }

    #[test]
    fn recipient_removed_on_republish_cannot_open_the_new_event() {
        // Revoke = re-seal on republish without R. The new event uses a fresh CEK; R can't open it.
        let author = Identity::generate();
        let r = Identity::generate();
        let s = Identity::generate();

        // Round 1: both R and S are trusted.
        let round1 = seal_private_listing(&author, &[r.public_key(), s.public_key()], LISTING, NOW)
            .unwrap();
        assert!(open_private_listing(&r, &round1[0]).is_ok(), "R opens its round-1 wrap");

        // Round 2: R removed from the trusted set; only S is sealed to.
        let round2 = seal_private_listing(&author, &[s.public_key()], LISTING, NOW + 1).unwrap();
        assert_eq!(round2.len(), 1, "only S receives a round-2 wrap");
        // R has NO wrap in round 2 (and cannot open S's, which isn't addressed to R).
        assert!(
            open_private_listing(&r, &round2[0]).is_err(),
            "a removed recipient cannot decrypt the republished event (old CEK is gone)"
        );
        assert!(open_private_listing(&s, &round2[0]).is_ok(), "S still opens the republish");
    }

    // ───────────────────────── round-trip + multi-recipient ─────────────────────────

    #[test]
    fn single_recipient_round_trip() {
        let (author, r, wrap) = seal_one();
        let opened = open_private_listing(&r, &wrap).unwrap();
        assert_eq!(opened.listing_json, LISTING);
        assert_eq!(opened.inner_author, author.public_key(), "inner author is the real signer");
        assert_eq!(opened.created_at, NOW, "the inner rumor carries the true publish time");
    }

    #[test]
    fn multiple_recipients_each_open_their_own_and_not_anothers() {
        let author = Identity::generate();
        let rs: Vec<Identity> = (0..4).map(|_| Identity::generate()).collect();
        let pubs: Vec<PublicKey> = rs.iter().map(|id| id.public_key()).collect();
        let wraps = seal_private_listing(&author, &pubs, LISTING, NOW).unwrap();
        assert_eq!(wraps.len(), 4, "one wrap per recipient");

        // Each recipient opens exactly one wrap (theirs), and none of the others'.
        for (i, ri) in rs.iter().enumerate() {
            let mut opened = 0;
            for (j, w) in wraps.iter().enumerate() {
                match open_private_listing(ri, w) {
                    Ok(o) => {
                        assert_eq!(o.listing_json, LISTING);
                        assert_eq!(i, j, "recipient {i} opened wrap {j} — must only open its own");
                        opened += 1;
                    }
                    Err(_) => assert_ne!(i, j, "recipient {i} failed to open its OWN wrap {j}"),
                }
            }
            assert_eq!(opened, 1, "recipient {i} must open exactly one wrap");
        }
    }

    // ───────────────────────── enumeration resistance + F15 unlinkability ─────────────────────────

    #[test]
    fn outer_wrap_author_is_ephemeral_never_the_real_npub() {
        let (author, r, wrap) = seal_one();
        assert_eq!(wrap.kind, Kind::GiftWrap, "outer is a standard 1059 gift-wrap");
        assert_ne!(wrap.pubkey, author.public_key(), "outer must NOT be signed by the real author");
        assert_ne!(wrap.pubkey, r.public_key(), "nor by the recipient");
    }

    #[test]
    fn n_recipients_have_n_distinct_ephemeral_authors() {
        // F15: the N wraps must be mutually unlinkable — no shared ephemeral key tying them to one
        // publish event.
        let author = Identity::generate();
        let pubs: Vec<PublicKey> = (0..6).map(|_| Identity::generate().public_key()).collect();
        let wraps = seal_private_listing(&author, &pubs, LISTING, NOW).unwrap();
        let mut ephemerals: Vec<PublicKey> = wraps.iter().map(|w| w.pubkey).collect();
        ephemerals.sort();
        ephemerals.dedup();
        assert_eq!(ephemerals.len(), 6, "N recipients must yield N DISTINCT ephemeral authors");
    }

    #[test]
    fn wrap_exposes_no_hoardbook_tag_in_the_clear() {
        // The observable wrap looks like any NIP-59 gift-wrap: kind 1059, a single `p` tag, and an
        // ephemeral author. No `hb-v`/`hb-cv`/listing marker leaks in the clear (those live inside
        // the encrypted seal). An observer can't tell it is a Hoardbook collection at all.
        let (_author, r, wrap) = seal_one();
        let json = wrap.as_json();
        for forbidden in ["hb-v", "hb-cv", "hoardbook", "KIND_PRIV", "31113"] {
            assert!(!json.contains(forbidden), "wrap leaked a Hoardbook marker in the clear: {forbidden}");
        }
        // The only tag a 1059 carries is the recipient `p` tag.
        let p_tags: Vec<_> = wrap.tags.public_keys().collect();
        assert_eq!(p_tags, vec![&r.public_key()], "exactly one p-tag, addressed to the recipient");
    }

    #[test]
    fn fresh_cek_per_publish_yields_different_ciphertext() {
        // Two publishes of the SAME listing to the SAME recipient → different body ciphertext
        // (fresh CEK each time), so a relay can't even tell two publishes carry the same listing.
        let author = Identity::generate();
        let r = Identity::generate();
        let a = seal_private_listing(&author, &[r.public_key()], LISTING, NOW).unwrap();
        let b = seal_private_listing(&author, &[r.public_key()], LISTING, NOW).unwrap();
        // Both open to the same plaintext...
        assert_eq!(open_private_listing(&r, &a[0]).unwrap().listing_json, LISTING);
        assert_eq!(open_private_listing(&r, &b[0]).unwrap().listing_json, LISTING);
        // ...but the wire bytes differ (distinct ephemeral key + nonce + CEK).
        assert_ne!(a[0].content, b[0].content, "fresh CEK ⇒ different ciphertext");
        assert_ne!(a[0].id, b[0].id, "distinct events");
    }

    // ───────────────────────── versioning (F18 / Decision D) ─────────────────────────

    #[test]
    fn inner_event_carries_matching_version_tags() {
        // After unwrap, the inner event MUST carry hb-v/hb-cv tags that match the CEK wrap. We
        // open as the recipient (the only one who can) and confirm the round-trip honoured them.
        let (_author, r, wrap) = seal_one();
        let opened = open_private_listing(&r, &wrap).unwrap();
        assert_eq!(opened.listing_json, LISTING, "version-tagged round-trip decrypts cleanly");
    }

    #[test]
    fn bumped_inner_version_is_recognised_not_misdecrypted() {
        // A hand-built private listing whose INNER tag claims a FUTURE schema version is recognised
        // and refused (UnsupportedVersion), never silently mis-decrypted under v1 — the inner-tag
        // version gate (the same forward-compat contract the public listing parser upholds).
        let author = Identity::generate();
        let r = Identity::generate();
        let wrap = forge_with_versions(&author, &r.public_key(), SCHEMA_V + 1, CRYPTO_V, SCHEMA_V + 1, CRYPTO_V, NOW);
        assert!(matches!(
            open_private_listing(&r, &wrap),
            Err(HbError::UnsupportedVersion(v)) if v == SCHEMA_V + 1
        ));
    }

    #[test]
    fn bad_cek_wrap_version_is_recognised_not_misdecrypted() {
        // The inner tags are valid (v1), but the CEK wrap's own version is out of range (0) — the
        // *wrap-side* version gate trips before the body is touched. Proves both discriminants are
        // checked (Decision D), not just the inner tag.
        let author = Identity::generate();
        let r = Identity::generate();
        let wrap = forge_with_versions(&author, &r.public_key(), SCHEMA_V, CRYPTO_V, SCHEMA_V, 0, NOW);
        assert!(matches!(open_private_listing(&r, &wrap), Err(HbError::UnsupportedVersion(0))));
    }

    // ───────────────────────── fuzz / adversarial (F9) ─────────────────────────

    #[test]
    fn malformed_wraps_are_reasoned_err_never_panic() {
        let (_author, r, wrap) = seal_one();
        let good = wrap.content.clone();

        // A battery of mutations on the OUTER 1059 content; each must Err, never panic.
        let mutations: Vec<(&str, String)> = vec![
            ("truncated@0", String::new()),
            ("truncated@1", good.chars().take(1).collect()),
            ("truncated@half", good.chars().take(good.len() / 2).collect()),
            ("truncated@last", good.chars().take(good.len().saturating_sub(1)).collect()),
            ("all-zero-b64", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()),
            ("not-base64", "!!!! not base64 @@@@".to_string()),
            ("flip-last-byte", flip_last_b64_byte(&good)),
        ];
        for (label, content) in mutations {
            let mut bad = wrap.clone();
            bad.content = content;
            let r2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                open_private_listing(&r, &bad)
            }));
            assert!(r2.is_ok(), "mutation {label} PANICKED — must be a reasoned Err");
            assert!(r2.unwrap().is_err(), "mutation {label} must be rejected");
        }
    }

    #[test]
    fn foreign_recipient_p_tag_does_not_help_an_attacker() {
        // A wrap p-tagged to someone else: an attacker who grabs it off the relay still can't open
        // it (the seal content is ECDH'd to the real recipient, not the attacker).
        let author = Identity::generate();
        let real = Identity::generate();
        let attacker = Identity::generate();
        let wraps = seal_private_listing(&author, &[real.public_key()], LISTING, NOW).unwrap();
        assert!(open_private_listing(&attacker, &wraps[0]).is_err(), "attacker cannot open it");
    }

    #[test]
    fn tampered_cek_wrap_is_rejected() {
        // Corrupt the inner CEK wrap (after a legit seal) — must fail cleanly, not surface garbage.
        let author = Identity::generate();
        let r = Identity::generate();
        let wrap = tamper_cek_wrap(&author, &r.public_key(), NOW);
        assert!(open_private_listing(&r, &wrap).is_err(), "a tampered CEK wrap must not decrypt");
    }

    #[test]
    fn wrong_inner_kind_after_unwrap_rejected() {
        // A correctly-wrapped, correctly-addressed event whose INNER kind is not KIND_PRIV_LISTING
        // (e.g. a DM kind-14 rumor) is rejected on the kind pin — never confused for a listing.
        let author = Identity::generate();
        let r = Identity::generate();
        let wrap = seal_inner_with_kind(&author, &r.public_key(), Kind::from_u16(14), NOW);
        assert!(matches!(open_private_listing(&r, &wrap), Err(HbError::InvalidEvent(_))));
    }

    #[test]
    fn non_gift_wrap_event_rejected() {
        // A plain text note handed to `open_private_listing` is refused on the outer kind check.
        let id = Identity::generate();
        let ev = id.sign(EventBuilder::new(Kind::TextNote, "hi")).unwrap();
        assert!(matches!(open_private_listing(&id, &ev), Err(HbError::InvalidEvent(_))));
    }

    // ───────────────────────── test-only forging helpers ─────────────────────────
    //
    // These mint a wrap with deliberately-wrong internals (future version, version mismatch,
    // tampered CEK, wrong inner kind) to drive the negative paths. They mirror the real
    // `seal_private_listing` construction so the ONLY difference is the injected fault.

    /// Forge a wrap with independently-set inner-tag versions and CEK-wrap versions, so the two
    /// version gates (inner tag vs wrap) can be exercised separately.
    fn forge_with_versions(
        author: &Identity,
        r: &PublicKey,
        inner_schema: u8,
        inner_kdf: u8,
        wrap_schema: u8,
        wrap_kdf: u8,
        now: u64,
    ) -> Event {
        let cek: ContentKey = rand::random();
        let body = encrypt_with_cek(&cek, LISTING).unwrap();
        let cek_wrap = serde_json::to_string(&CekWrap {
            cek: hex::encode(cek),
            schema_v: wrap_schema,
            kdf_v: wrap_kdf,
        })
        .unwrap();
        let wrapped_cek =
            nip44::encrypt(author.keys().secret_key(), r, cek_wrap, nip44::Version::V2).unwrap();
        let content = serde_json::to_string(&PrivContent { body, wrap: wrapped_cek }).unwrap();
        forge_wrap(author, r, content, Kind::from_u16(KIND_PRIV_LISTING), inner_schema, inner_kdf, now)
    }

    fn tamper_cek_wrap(author: &Identity, r: &PublicKey, now: u64) -> Event {
        let cek: ContentKey = rand::random();
        let body = encrypt_with_cek(&cek, LISTING).unwrap();
        let mut wrapped_cek = nip44::encrypt(
            author.keys().secret_key(),
            r,
            serde_json::to_string(&CekWrap { cek: hex::encode(cek), schema_v: SCHEMA_V, kdf_v: CRYPTO_V })
                .unwrap(),
            nip44::Version::V2,
        )
        .unwrap();
        // Flip a character in the middle of the base64 CEK wrap → AEAD/MAC failure on open.
        wrapped_cek = flip_last_b64_byte(&wrapped_cek);
        let content = serde_json::to_string(&PrivContent { body, wrap: wrapped_cek }).unwrap();
        forge_wrap(author, r, content, Kind::from_u16(KIND_PRIV_LISTING), SCHEMA_V, CRYPTO_V, now)
    }

    fn seal_inner_with_kind(author: &Identity, r: &PublicKey, kind: Kind, now: u64) -> Event {
        let cek: ContentKey = rand::random();
        let body = encrypt_with_cek(&cek, LISTING).unwrap();
        let wrapped_cek = nip44::encrypt(
            author.keys().secret_key(),
            r,
            serde_json::to_string(&CekWrap { cek: hex::encode(cek), schema_v: SCHEMA_V, kdf_v: CRYPTO_V })
                .unwrap(),
            nip44::Version::V2,
        )
        .unwrap();
        let content = serde_json::to_string(&PrivContent { body, wrap: wrapped_cek }).unwrap();
        forge_wrap(author, r, content, kind, SCHEMA_V, CRYPTO_V, now)
    }

    /// Build a 1059 wrap around a forged inner event (given content/kind/version-tags), exactly as
    /// the real seal does — only the injected fault differs.
    fn forge_wrap(
        author: &Identity,
        r: &PublicKey,
        content: String,
        inner_kind: Kind,
        schema_v: u8,
        kdf_v: u8,
        now: u64,
    ) -> Event {
        let rumor: UnsignedEvent = EventBuilder::new(inner_kind, content)
            .tags([
                Tag::custom(TagKind::custom(TAG_SCHEMA), [schema_v.to_string()]),
                Tag::custom(TagKind::custom(TAG_CRYPTO), [kdf_v.to_string()]),
            ])
            .custom_created_at(Timestamp::from(now))
            .build(author.public_key());
        let seal_content =
            nip44::encrypt(author.keys().secret_key(), r, rumor.as_json(), nip44::Version::V2).unwrap();
        let seal = EventBuilder::new(Kind::Seal, seal_content)
            .custom_created_at(Timestamp::from(now))
            .sign_with_keys(author.keys())
            .unwrap();
        EventBuilder::gift_wrap_from_seal(r, &seal, []).unwrap()
    }

    /// Flip the last byte of a base64 string to a different base64 char (a 1-bit-ish corruption).
    fn flip_last_b64_byte(s: &str) -> String {
        let mut chars: Vec<char> = s.chars().collect();
        if let Some(last) = chars.last_mut() {
            *last = if *last == 'A' { 'B' } else { 'A' };
        }
        chars.into_iter().collect()
    }
}
