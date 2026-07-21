//! WIRE FREEZE — these values are frozen the moment events exist in the wild. Changing one
//! silently breaks every event/backup/share code already published; every test would still pass
//! because tests round-trip with the same code. A change here is a version negotiation plus an
//! INV-8 audit (INVARIANT_AUDIT.md I-3) — never an edit. A NEW durable event kind must be added
//! here and must answer INV-8 (is this data safe to keep forever?) in the spec first.

use crate::backup::BACKUP_FORMAT_VER;
use crate::binding;
use crate::event;
use crate::listing::{HKDF_SALT, HKDF_SALT_CEK};
use crate::manifest::{MANIFEST_V, SIG_DOMAIN};
use crate::priv_listing;
use crate::sharecode;
use crate::topic;
use crate::version::{CRYPTO_V, SCHEMA_V};

const FREEZE: &str = "WIRE FREEZE (I-3): a change here is a version negotiation + INV-8 audit, never an edit";

/// Every durable Nostr `kind` this crate publishes (or seals inside a wrap). A relay stores events
/// under these numbers forever; a renumber orphans every event already in the wild.
#[test]
fn event_kinds_are_frozen() {
    assert_eq!(binding::KIND_PRESENCE, 11_111, "KIND_PRESENCE — {FREEZE}");
    assert_eq!(event::KIND_TEASER, 30_117, "KIND_TEASER — {FREEZE}");
    assert_eq!(event::KIND_LISTING, 31_111, "KIND_LISTING — {FREEZE}");
    assert_eq!(priv_listing::KIND_PRIV_LISTING, 31_113, "KIND_PRIV_LISTING — {FREEZE}");
    assert_eq!(topic::KIND_TOPIC_ANNOUNCE, 31_117, "KIND_TOPIC_ANNOUNCE — {FREEZE}");
    assert_eq!(topic::KIND_TOPIC_MEMBER, 31_118, "KIND_TOPIC_MEMBER — {FREEZE}");
    assert_eq!(topic::KIND_TOPIC_POST, 1_117, "KIND_TOPIC_POST — {FREEZE}");
    assert_eq!(topic::KIND_TOPIC_INVITE, 31_119, "KIND_TOPIC_INVITE — {FREEZE}");
    assert_eq!(topic::KIND_TOPIC_PROOF, 31_120, "KIND_TOPIC_PROOF — {FREEZE}");
}

/// The version discriminants carried in signed content / headers. Bumping any of these is a
/// deliberate flag-day (readers must *recognise and refuse* the new value first), never a drift.
#[test]
fn version_discriminants_are_frozen() {
    assert_eq!(SCHEMA_V, 1, "SCHEMA_V — {FREEZE}");
    assert_eq!(CRYPTO_V, 1, "CRYPTO_V — {FREEZE}");
    assert_eq!(BACKUP_FORMAT_VER, 1, "BACKUP_FORMAT_VER — {FREEZE}");
    // MANIFEST_V bumped 1→2 (M16 W4 residual): v1 was the pre-release single-`ciphertext` shape,
    // superseded before any producer shipped (export landed at v2 — the chunked `ciphertexts` body).
    // v2 is the frozen launch value; a v1 envelope no longer even deserializes (its field is gone).
    assert_eq!(MANIFEST_V, 2, "MANIFEST_V (M16 manifest envelope, chunked v2) — {FREEZE}");
}

/// The manifest envelope's `author_sig` pre-image domain tag (M16 W1). It is hashed into every
/// signature over an exported `.hbmanifest`; a change silently invalidates every manifest already
/// signed, so it is pinned here as a wire constant. It is a fixed domain-separation tag, deliberately
/// **independent of `MANIFEST_V`** — the envelope version is bound separately inside the signed digest
/// (`signing_digest` hashes `manifest_v`), so the tag stays stable across format revisions.
#[test]
fn manifest_sig_domain_is_frozen() {
    assert_eq!(SIG_DOMAIN, b"hoardbook/manifest-envelope/v1".as_slice(), "manifest::SIG_DOMAIN — {FREEZE}");
}

/// The `hbk` share code: bech32 HRP + leading version byte (spec §The Key). Every code already
/// pasted into a chat decodes under exactly this framing.
#[test]
fn sharecode_format_is_frozen() {
    assert_eq!(sharecode::HRP_STR, "hbk", "share-code bech32 HRP — {FREEZE}");
    assert_eq!(sharecode::SHARECODE_VERSION, 1, "SHARECODE_VERSION (defined = CRYPTO_V) — {FREEZE}");
}

/// The signed tag names (`hb-v` / `hb-cv` / `hb-expires`). The literals are duplicated per module —
/// itself a drift risk — so ALL duplicates are pinned to the one frozen string: a rename in any
/// single module reddens here, not just a rename in all of them.
#[test]
fn tag_names_are_frozen_and_all_duplicates_agree() {
    for (site, tag) in [
        ("binding::TAG_SCHEMA", binding::TAG_SCHEMA),
        ("event::TAG_SCHEMA", event::TAG_SCHEMA),
        ("priv_listing::TAG_SCHEMA", priv_listing::TAG_SCHEMA),
        ("topic::TAG_SCHEMA", topic::TAG_SCHEMA),
    ] {
        assert_eq!(tag, "hb-v", "{site} — {FREEZE}");
    }
    for (site, tag) in [
        ("event::TAG_CRYPTO", event::TAG_CRYPTO),
        ("priv_listing::TAG_CRYPTO", priv_listing::TAG_CRYPTO),
        ("topic::TAG_CRYPTO", topic::TAG_CRYPTO),
    ] {
        assert_eq!(tag, "hb-cv", "{site} — {FREEZE}");
    }
    assert_eq!(binding::TAG_EXPIRES, "hb-expires", "binding::TAG_EXPIRES — {FREEZE}");
}

/// The HKDF salts that domain-separate the browse-key and CEK derivations. Changing a salt changes
/// every derived NIP-44 key — every listing already on a relay stops decrypting.
#[test]
fn hkdf_salts_are_frozen() {
    assert_eq!(HKDF_SALT, b"hoardbook/browse-key".as_slice(), "listing::HKDF_SALT — {FREEZE}");
    assert_eq!(HKDF_SALT_CEK, b"hoardbook/cek".as_slice(), "listing::HKDF_SALT_CEK — {FREEZE}");
}

/// The `hbm:` proof-statement domain prefixes (chorus-2 domain separation). A change invalidates
/// every proof already sealed inside members' durable roster events.
#[test]
fn proof_domain_prefixes_are_frozen() {
    assert_eq!(topic::PROOF_JOIN_PREFIX, "hbm:join:", "topic::PROOF_JOIN_PREFIX — {FREEZE}");
    assert_eq!(topic::PROOF_POST_PREFIX, "hbm:post:", "topic::PROOF_POST_PREFIX — {FREEZE}");
    assert_eq!(topic::PROOF_ANNOUNCE_PREFIX, "hbm:announce:", "topic::PROOF_ANNOUNCE_PREFIX — {FREEZE}");
}

/// The topic ciphertext DOMAIN BYTES (F17) — the first plaintext byte inside every topic_key
/// ciphertext, telling membership/post/broadcast apart. These are wire discriminants living INSIDE
/// signed, durable ciphertext (a membership/post/broadcast event already on a relay), so a renumber
/// silently reinterprets every such event under the new meaning. Previously unpinned; M13 Part A
/// (the broadcast domain byte) closes that gap.
#[test]
fn topic_domain_bytes_are_frozen() {
    assert_eq!(topic::MEMBERSHIP_DOMAIN, 0x01, "topic::MEMBERSHIP_DOMAIN — {FREEZE}");
    assert_eq!(topic::POST_DOMAIN, 0x02, "topic::POST_DOMAIN — {FREEZE}");
    assert_eq!(topic::ANNOUNCE_DOMAIN, 0x03, "topic::ANNOUNCE_DOMAIN — {FREEZE}");
}

/// The teaser-picture size cap (M13 item #13, additive under SCHEMA_V=1) — a raise/lower changes
/// what a signed teaser already in the wild is allowed to carry; pinned so it is a deliberate call,
/// not a drift.
#[test]
fn teaser_picture_cap_is_frozen() {
    assert_eq!(event::TEASER_PICTURE_MAX_BYTES, 16 * 1024, "event::TEASER_PICTURE_MAX_BYTES — {FREEZE}");
}
