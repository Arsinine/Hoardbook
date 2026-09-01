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
use crate::priv_listing::{open_key_grant, seal_key_grant, seal_private_listing};
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
        picture: None,
    };
    events.push(("event::build_teaser", build_teaser(&me, &teaser, true).expect("teaser builds")));

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

    // Key grant (QURATOR-160) — THE headline surface for INV-2: the one event that deliberately
    // CARRIES the fixed browse-key as its payload. It must reach the relay only through the full
    // ECDH→NIP-44→seal→gift-wrap chain, so the wire bytes are scanned like every other event,
    // and then unwrapped layer by layer below to prove each layer is actually ciphertext.
    // A new wrap builder is ADDED here, never skipped.
    for wrap in seal_key_grant(&me, &[peer.public_key()], &browse_key, NOW).expect("key grant seals")
    {
        events.push(("priv_listing::seal_key_grant", wrap));
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

    // ── INV-2, the layer-strip half (QURATOR-160): the wire scan above can only see the OUTER
    // bytes. A dropped encryption one layer in (e.g. the NIP-44 key wrap replaced by plaintext)
    // still hides the key under the seal + gift-wrap, so the scan alone is blind to it. Unwrap the
    // grant as the recipient and assert the key appears in NO INTERMEDIATE layer a relay-side
    // observer could ever obtain: not the outer 1059 content (relay-visible), and not the seal
    // content (relay-visible). The rumor content and the key wrap are NOT obtainable without the
    // recipient's secret key, so they are gated by round-trip instead: the opened key must equal
    // the fixed browse-key through the full ECDH chain — which reds the moment the wrap stops
    // being the NIP-44 wrap, because a recipient's `nip44::decrypt` on plaintext fails outright.
    //
    // ⚠ P-10 MUTATION RECIPE (orchestrator applies; do NOT trust this test red until seen):
    //   file: crates/hb-core/src/priv_listing.rs, fn `seal_wrapped`, the per-recipient loop,
    //   replace  `let wrapped_cek = nip44::encrypt(author_sk, r, cek_wrap, nip44::Version::V2)`
    //            `.map_err(|e| HbError::Nostr(e.to_string()))?;`
    //   with     `let wrapped_cek = cek_wrap.clone();`
    //   (drop the per-recipient ECDH wrap; ship the wrap JSON as plaintext inside the seal).
    //   This MUST redden this test at the layer-3 `.expect("the recipient opens its own grant")`
    //   (the open fails: the wrap no longer decrypts) — note the wire-scan half above stays GREEN
    //   under this mutation (the seal still encrypts the rumor), which is exactly why the
    //   layer-strip half exists. It also reddens priv_listing's `key_grant_round_trip…` and every
    //   private-listing round-trip. Revert and confirm green.
    let grant_wraps =
        seal_key_grant(&me, &[peer.public_key()], &browse_key, NOW).expect("key grant seals");
    let grant = &grant_wraps[0];
    // Layer 1 — the relay-visible outer 1059 content must not carry the key in any encoding.
    for (encoding, needle) in &needles {
        assert!(
            !grant.content.contains(needle.as_str()),
            "INV-2 LEAK: key-grant outer 1059 content carries the browse-key as {encoding}"
        );
    }
    // Layer 2 — unwrap to the seal (needs the recipient key, so a relay cannot do this; the
    // assertion documents that the seal content is ciphertext, not a plaintext key carrier).
    let peer_sk = peer.keys().secret_key();
    let seal_json = nip44::decrypt(peer_sk, &grant.pubkey, &grant.content)
        .expect("the grant's outer layer unwraps for its recipient");
    for (encoding, needle) in &needles {
        assert!(
            !seal_json.contains(needle.as_str()),
            "INV-2 LEAK: key-grant seal content carries the browse-key as {encoding}"
        );
    }
    // Layer 3 — the full open returns exactly the fixed key (delivery), via the verified chain.
    let opened = open_key_grant(&peer, grant).expect("the recipient opens its own grant");
    assert_eq!(opened.browse_key, browse_key, "the grant delivers the exact browse key");
    assert_eq!(
        opened.inner_author,
        me.public_key(),
        "the grant's inner author is the verified seal signer"
    );
}
