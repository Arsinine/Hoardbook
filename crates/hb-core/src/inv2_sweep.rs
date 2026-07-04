//! INV-2 — the browse-key is never broadcast. Every public event builder MUST be exercised here;
//! adding a new public builder to hb-core means adding it to this enumeration (review checklist).
//! DMs are deliberately excluded: handing your share code to a person over an encrypted DM is the
//! intended flow.
//!
//! Method (INVARIANT_AUDIT.md I-5): fix the browse-key to KNOWN bytes, build every broadcast event
//! the crate can produce, serialize each to wire JSON, and assert the key appears in NO encoding a
//! leak could wear — hex (both cases), base64 (STANDARD / URL_SAFE, padded + unpadded), and the
//! full `hbk1…` share-code string.

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use nostr::prelude::*;

use crate::binding::build_binding;
use crate::event::{build_listing_event, build_teaser, Teaser};
use crate::identity::Identity;
use crate::listing::BrowseKey;
use crate::priv_listing::seal_private_listing;
use crate::sharecode::ShareCode;
use crate::topic::{
    build_announce, build_public_join, mint_invite, new_topic, seal_announce, seal_membership,
    seal_post,
};

const NOW: u64 = 1_700_000_000;

#[test]
fn no_public_event_broadcasts_the_browse_key_in_any_encoding() {
    let me = Identity::generate();
    let peer = Identity::generate();
    // Fixed known bytes — 0xB4, not the classic 0x42: hex(0x42…) is all digits, so the uppercase
    // needle would be identical to the lowercase one and the upper-hex check vacuous. 0xB4 → "b4…"
    // vs "B4…" keeps every needle distinct and load-bearing.
    let browse_key: BrowseKey = [0xB4u8; 32];

    // ── Every encoding an accidental embed could wear on the wire. ──
    let share_code = ShareCode::Full { pubkey: me.public_key(), browse_key }
        .encode()
        .expect("share-code fixture must encode");
    let needles: Vec<(&str, String)> = vec![
        ("hex-lower", hex::encode(browse_key)),
        ("hex-upper", hex::encode(browse_key).to_uppercase()),
        ("base64-standard", STANDARD.encode(browse_key)),
        ("base64-standard-nopad", STANDARD_NO_PAD.encode(browse_key)),
        ("base64-urlsafe", URL_SAFE.encode(browse_key)),
        ("base64-urlsafe-nopad", URL_SAFE_NO_PAD.encode(browse_key)),
        ("hbk-share-code", share_code),
    ];

    // ── EVERY public (broadcast) builder in hb-core. A new one is ADDED here, never skipped. ──
    let mut events: Vec<(&str, Event)> = Vec::new();

    // Public teaser (spec §The Profile) — plaintext discovery metadata.
    let teaser = Teaser {
        display_name: "archivebox_prime".into(),
        bio: "90s anime, VHS rips".into(),
        tags: vec!["anime".into(), "vhs".into()],
        content_types: vec!["video".into()],
    };
    events.push(("event::build_teaser", build_teaser(&me, &teaser).expect("teaser builds")));

    // Public listing — encrypted UNDER the fixed browse-key (the headline surface: the key is the
    // encryption input right here, so this is where an implementation slip would leak it).
    let listing_json = r#"{"slug":"criterion","content_types":["video"],"items":[{"name":"Ran"}]}"#;
    events.push((
        "event::build_listing_event",
        build_listing_event(&me, "criterion", &browse_key, listing_json).expect("listing builds"),
    ));

    // Presence beacon (freshness-only since v0.9.6 — no address, no node key, and no key here).
    events.push(("binding::build_binding", build_binding(&me, NOW, 30 * 60).expect("binding builds")));

    // Topics (M11/M12/M13): announce + membership + 24h post + broadcast + invite + public-join.
    let (meta, topic_key) =
        new_topic("video/80s-anime", "VHS rips & fansubs", vec!["anime".into()], false)
            .expect("public topic mints");
    events.push(("topic::build_announce", build_announce(&me, &meta, NOW).expect("announce builds")));
    events.push((
        "topic::seal_membership",
        seal_membership(&topic_key, &meta.topic_id, &me, NOW).expect("membership seals"),
    ));
    events.push((
        "topic::seal_post",
        seal_post(&topic_key, &meta.topic_id, &me, "hello room", NOW).expect("post seals"),
    ));
    events.push((
        "topic::seal_announce",
        seal_announce(&topic_key, &meta.topic_id, &me, "hello room broadcast", NOW).expect("announce seals"),
    ));
    events.push((
        "topic::mint_invite",
        mint_invite(&me, &peer.public_key(), &meta, &topic_key, "nonce-1", Some(NOW + 3_600), NOW)
            .expect("invite mints"),
    ));
    events.push((
        "topic::build_public_join",
        build_public_join(&me, &meta, &topic_key, NOW).expect("public-join builds"),
    ));

    // Private listing — takes NO browse key by design (F6: unrepresentable at the type level);
    // its wraps are scanned anyway, proving the seal path stays clean while a browse-key exists.
    for wrap in seal_private_listing(&me, &[peer.public_key()], listing_json, NOW)
        .expect("private listing seals")
    {
        events.push(("priv_listing::seal_private_listing", wrap));
    }

    // ── The sweep: the browse-key must appear in NO event under NO encoding. ──
    for (builder, ev) in &events {
        let json = ev.as_json();
        for (encoding, needle) in &needles {
            assert!(
                !json.contains(needle.as_str()),
                "INV-2 LEAK: {builder} broadcast the browse-key as {encoding} — the browse-key \
                 travels only person-to-person (share code / DM), never inside a public event"
            );
        }
    }
}
