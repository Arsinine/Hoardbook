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
use crate::transport_payload::{MANIFEST_MAX_TRANSPORT_BYTES, MANIFEST_MAX_TRANSPORT_PARTS};
use crate::priv_listing;
use crate::sharecode;
use crate::ticket;
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
    // ADDED (QURATOR-160), never bumped: the inner key-grant kind — sealed inside a NIP-59 wrap,
    // never a top-level event, so adding it changes no existing wire traffic (no existing field's
    // meaning moved; this is a new pin, which is the only edit wire_freeze permits).
    assert_eq!(priv_listing::KIND_KEY_GRANT, 31_114, "KIND_KEY_GRANT — {FREEZE}");
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

/// **INV-4′ mechanism 2 — the manifest transport ceiling (M18).** Frozen because it is a
/// *negotiation-free* protocol constant: two peers that disagree about what is deliverable would
/// disagree about whether a transfer failed or was refused. Owner ruling 2026-07-30 fixed it
/// rather than version-negotiating it, on the reasoning that the binding constraint is human
/// browseability — see `transport_payload::MANIFEST_MAX_TRANSPORT_BYTES` for the full derivation.
///
/// **Re-frozen at a new value, deliberately, 2026-08-19** (8 MiB → 16 MiB): the browseability
/// premise held for a media library but not for a software/game collection, whose file count
/// tracks disk contents rather than anything a person browses one entry at a time — paired with
/// a new, independent `MAX_COLLECTION_ITEMS = 100_000` cap (hb-app) that guards the many-tiny-
/// files failure shape the byte ceiling alone doesn't. This assertion failing is *exactly* the
/// version-negotiation-plus-INV-4′-re-audit this comment warns about — this edit IS that audit,
/// not a bypass of it. The next change to this number must go through the same deliberate path,
/// never a silent edit.
#[test]
fn manifest_transport_ceiling_is_frozen() {
    assert_eq!(
        MANIFEST_MAX_TRANSPORT_BYTES,
        16 * 1024 * 1024,
        "MANIFEST_MAX_TRANSPORT_BYTES (M18 INV-4′ mechanism 2) — {FREEZE}"
    );
}

/// The companion part cap, and the reason it is **4096 and not a taste**: it must equal
/// `hb-net::MAX_LISTING_PARTS`, the producer's own limit. hb-core cannot import it (no hb-net dep),
/// so the number is duplicated — and a duplicated constant drifts unless something compares them.
/// Too low and the transport refuses manifests this very app builds; too high and the cap stops
/// bounding the work a 16 MiB frame can demand (millions of empty parts, hash honestly matching).
#[test]
fn manifest_transport_part_cap_matches_the_producer_cap() {
    assert_eq!(
        MANIFEST_MAX_TRANSPORT_PARTS, 4096,
        "MANIFEST_MAX_TRANSPORT_PARTS must equal hb-net::MAX_LISTING_PARTS (4096) — a manifest this \
         app can legitimately build must never be refused by its own transport. {FREEZE}"
    );
    // Both directions of the coupling stated where a reader will see it: the producer's cap lives in
    // `hb-net::split::MAX_LISTING_PARTS`. If that moves, this must move with it, and `hb-it` Suite MAN
    // is what exercises a real multi-part manifest end to end.
}

/// The transport ticket's version + DM discriminator (M18 W1). A ticket rides a NIP-17 DM, which a
/// relay stores like any other wrap, so one already sitting in an asker's inbox must stay readable —
/// and the discriminator is how that inbox tells a ticket from a `manifest_request` going the other
/// way. Renaming either silently turns a delivered approval into an unrecognised message.
#[test]
fn ticket_version_and_tag_are_frozen() {
    assert_eq!(ticket::TICKET_V, 1, "ticket::TICKET_V — {FREEZE}");
    assert_eq!(ticket::TICKET_TAG, "transport_ticket", "ticket::TICKET_TAG — {FREEZE}");

    // **Amendment 2026-07-31 (owner ruling ①): `ask_nonce` joined the ticket.** `TICKET_V` did NOT
    // bump, deliberately — the field is `Option` + `skip_serializing_if`, so a v1 ticket without it
    // still parses and an old client still reads a new ticket. What makes that safe is the REDEEM
    // side failing closed on `None`, not the wire being permissive.
    //
    // Pinned by serialized shape rather than by a version number: the name is the contract, and a
    // rename would silently stop every gate matching while every test kept passing.
    let with = ticket::TransportTicket::issue("r", "s", "addr", 1, Some("n0nce"));
    let json = serde_json::to_string(&with).expect("a ticket serializes");
    assert!(json.contains("\"ask_nonce\":\"n0nce\""), "ticket ask_nonce field name — {FREEZE}");

    // Absent, not null, when there is no nonce — so an old client's parser sees exactly what it saw
    // before this field existed.
    let without = ticket::TransportTicket::issue("r", "s", "addr", 1, None);
    let json = serde_json::to_string(&without).expect("a ticket serializes");
    assert!(!json.contains("ask_nonce"), "an absent ask nonce is omitted, not null — {FREEZE}");
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

/// The future-skew tolerance (`FUTURE_SKEW_SECS = 300`) used to be defined in BOTH `binding.rs` and
/// `count.rs`, and nothing compared them — a one-sided edit would silently split the two freshness
/// windows (a beacon the binding admits but the online tally drops, or vice versa). It is now
/// defined exactly once, in `binding`, and `count` imports it. A value asserted twice through the
/// same definition proves nothing, so this scans the two files that used to carry the duplicate and
/// pins the definition count to one.
#[test]
fn future_skew_is_defined_exactly_once_and_both_freshness_paths_agree() {
    let binding_src = include_str!("binding.rs");
    let count_src = include_str!("count.rs");
    let definitions = binding_src.matches("const FUTURE_SKEW_SECS").count()
        + count_src.matches("const FUTURE_SKEW_SECS").count();
    assert_eq!(
        definitions, 1,
        "FUTURE_SKEW_SECS must be defined in exactly one module — a second `const FUTURE_SKEW_SECS` \
         would let the two freshness windows drift apart"
    );

    // The value through the public paths: the binding module's constant and the crate-root
    // re-export agree on 300. The value is pinned here deliberately — it participates in
    // freshness/binding validation, so changing it is a wire-visible behaviour change, not an edit.
    assert_eq!(binding::FUTURE_SKEW_SECS, 300, "binding::FUTURE_SKEW_SECS");
    assert_eq!(crate::FUTURE_SKEW_SECS, 300, "crate::FUTURE_SKEW_SECS (re-export)");
}
