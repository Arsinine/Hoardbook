//! Private-collection publish/fetch (M10; spec §Private Collections) — the relay seam for the
//! per-recipient gift-wrapped listings `hb-core::priv_listing` seals. **Publish** multi-publishes
//! the N wraps to all relays (F14). **Fetch** reads kind-1059 events `p`-tagged to me, opens each,
//! and keeps only those whose **inner** author is in the trusted allowlist — a *post-decrypt* check,
//! because the outer 1059 author is ephemeral and so can't be relay-filtered (mirrors the NIP-17
//! sender-block-after-unwrap rule, TEST_PLAN AB2). Retried publishes (distinct outer ids, same
//! inner content) are deduped, keeping the newest per `(inner_author, slug)`.
//!
//! QURATOR-160 receive side adds the same fetch/open/trust shape one inner kind over:
//! [`fetch_key_grants`] reads browse-key grants (inner kind 31_114) from the SAME kind-1059 inbox.

use std::collections::HashMap;
use std::time::Duration;

use hb_core::{open_key_grant, open_private_listing, Identity, OpenedKeyGrant, OpenedPrivate};
use nostr::prelude::*;

use crate::client::RelayClient;
use crate::error::NetError;

/// Publish the N gift-wrapped private-listing events (from `seal_private_listing`) to **all** relays
/// (F14 — each wrap multi-published like every Hoardbook event). Errors only if a wrap was accepted
/// by no relay (an all-reject / all-drop), surfacing the reason — never a silent drop.
pub async fn publish_private_listing(
    client: &RelayClient,
    events: &[Event],
) -> Result<(), NetError> {
    for ev in events {
        client.publish(ev).await?;
    }
    Ok(())
}

/// Fetch private listings addressed to `me`, keeping only those from a **trusted** author. The relay
/// filter is `{kinds:[1059], #p:[me]}`; the author check is **post-decrypt** (open each wrap, keep it
/// iff its inner author ∈ `allowlist`). A wrap not addressed to us, from an untrusted author, or
/// malformed is silently skipped — a relay mixes everyone's 1059s in this inbox, so a foreign or
/// junk wrap is *expected*, not an error. A non-recipient calling this gets an empty result: there
/// is no "this is private" hint to find. Retried publishes collapse via [`dedup_newest`].
pub async fn fetch_private_listings(
    client: &RelayClient,
    me: &Identity,
    allowlist: &[PublicKey],
    timeout: Duration,
) -> Result<Vec<OpenedPrivate>, NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    let mut opened: Vec<OpenedPrivate> = Vec::new();
    for w in wraps {
        if let Ok(o) = open_private_listing(me, &w) {
            if allowlist.contains(&o.inner_author) {
                opened.push(o);
            }
        }
    }
    Ok(dedup_newest(opened))
}

/// Fetch browse-key grants (QURATOR-160 receive side) addressed to `me`, keeping only those from a
/// **trusted** granter — the exact fetch/open/trust shape of [`fetch_private_listings`], one inner
/// kind over. Same `{kinds:[1059], #p:[me]}` inbox (a relay mixes everyone's 1059s, so a foreign or
/// junk wrap is expected, never an error); each wrap is opened with
/// `hb_core::priv_listing::open_key_grant` and kept iff its verified inner author ∈ `allowlist`.
/// The allowlist is the trust decision `open_key_grant` explicitly refuses to make itself (its
/// doc: "the caller MUST decide whether `inner_author` is allowed to hand it a browse key") —
/// hb-app passes `contact_author_allowlist` (hand-added contacts only, owner ruling 2026-07-03).
/// A wrap not addressed to us, from an untrusted author, of the wrong inner kind (event confusion
/// with a private listing), or malformed is silently skipped. Retried grants collapse via
/// [`dedup_grants_newest`]. A non-recipient calling this gets an empty result.
pub async fn fetch_key_grants(
    client: &RelayClient,
    me: &Identity,
    allowlist: &[PublicKey],
    timeout: Duration,
) -> Result<Vec<OpenedKeyGrant>, NetError> {
    let filter = Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key());
    let wraps = client.fetch(filter, timeout).await?;
    Ok(open_key_grant_wraps(me, &wraps, allowlist))
}

/// The pure open/trust/dedup core of [`fetch_key_grants`] — everything after the relay fetch, so a
/// real `seal_key_grant`-produced wrap round-trips through production code without a relay (the
/// same discipline that makes [`dedup_newest`] the tested half of `fetch_private_listings`).
pub(crate) fn open_key_grant_wraps(
    me: &Identity,
    wraps: &[Event],
    allowlist: &[PublicKey],
) -> Vec<OpenedKeyGrant> {
    let mut opened: Vec<OpenedKeyGrant> = Vec::new();
    for w in wraps {
        if let Ok(g) = open_key_grant(me, w) {
            if allowlist.contains(&g.inner_author) {
                opened.push(g);
            }
        }
    }
    dedup_grants_newest(opened)
}

/// Collapse retried/duplicate grants: keep the **newest** (by inner `created_at`) opened grant per
/// granter. A grant carries no slug — the key is the whole payload — so the dedup key is the
/// granter alone (the same "newest wins" [`dedup_newest`] gives listings per `(author, slug)`;
/// distinct granters are never cross-collapsed). Pure. Deterministic output order: newest first,
/// then author hex.
pub(crate) fn dedup_grants_newest(opened: Vec<OpenedKeyGrant>) -> Vec<OpenedKeyGrant> {
    let mut best: HashMap<String, OpenedKeyGrant> = HashMap::new();
    for g in opened {
        let key = g.inner_author.to_hex();
        match best.get(&key) {
            Some(prev) if prev.created_at >= g.created_at => {}
            _ => {
                best.insert(key, g);
            }
        }
    }
    let mut out: Vec<OpenedKeyGrant> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.inner_author.to_hex().cmp(&b.inner_author.to_hex()))
    });
    out
}

/// Collapse retried/duplicate publishes: keep the **newest** (by inner `created_at`) opened listing
/// per `(inner_author, slug)`. A relay-retry yields a *new* event id (fresh ephemeral key) so
/// id-dedup alone leaves duplicates — the inner-content key is what collapses them. Distinct
/// collections (different slugs) from the same author are both kept; a newer republish of the same
/// slug supersedes the older one (the same "newest wins" the public replaceable path gets for free).
/// Pure → unit-tested without a relay. Output order is deterministic: newest first, then author hex.
pub fn dedup_newest(opened: Vec<OpenedPrivate>) -> Vec<OpenedPrivate> {
    let mut best: HashMap<(String, String), OpenedPrivate> = HashMap::new();
    for o in opened {
        let key = (o.inner_author.to_hex(), listing_slug(&o.listing_json));
        match best.get(&key) {
            Some(prev) if prev.created_at >= o.created_at => {}
            _ => {
                best.insert(key, o);
            }
        }
    }
    let mut out: Vec<OpenedPrivate> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.inner_author.to_hex().cmp(&b.inner_author.to_hex()))
    });
    out
}

/// Extract the `slug` field from a listing JSON for dedup keying. A well-formed Hoardbook listing
/// always carries a string `slug` (`collection_to_listing_json` includes it); the fallback handles a
/// malformed/foreign listing **deterministically and with a bounded key** (chorus M10): the raw JSON
/// is canonicalised (serde_json `Value` sorts object keys) and **hashed** — so two byte-different
/// encodings of the *same* content collapse, and the key never balloons to the listing's full size.
fn listing_slug(listing_json: &str) -> String {
    use sha2::{Digest, Sha256};
    match serde_json::from_str::<serde_json::Value>(listing_json) {
        Ok(v) => {
            if let Some(slug) = v.get("slug").and_then(|s| s.as_str()) {
                return slug.to_string();
            }
            // Canonical (sorted-key) re-serialisation → stable across encodings, then hash to bound size.
            let canon = serde_json::to_string(&v).unwrap_or_else(|_| listing_json.to_string());
            format!("sha256:{}", hex::encode(Sha256::digest(canon.as_bytes())))
        }
        // Not even JSON — hash the raw bytes (still deterministic + bounded).
        Err(_) => format!("sha256:{}", hex::encode(Sha256::digest(listing_json.as_bytes()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(author: &PublicKey, slug: &str, created_at: u64) -> OpenedPrivate {
        OpenedPrivate {
            listing_json: format!(r#"{{"slug":"{slug}","entries":[]}}"#),
            inner_author: *author,
            created_at,
        }
    }

    #[test]
    fn dedup_keeps_newest_per_author_and_slug() {
        let a = Identity::generate().public_key();
        // Two publishes of the same (author, slug) — distinct events, the newer one wins.
        let v = dedup_newest(vec![opened(&a, "vault", 100), opened(&a, "vault", 200)]);
        assert_eq!(v.len(), 1, "a retried/updated publish collapses to one");
        assert_eq!(v[0].created_at, 200, "the newest survives");
    }

    #[test]
    fn dedup_keeps_distinct_slugs_from_same_author() {
        let a = Identity::generate().public_key();
        let v = dedup_newest(vec![opened(&a, "vault", 100), opened(&a, "films", 100)]);
        assert_eq!(v.len(), 2, "two different private collections from one author are both kept");
    }

    #[test]
    fn dedup_keeps_same_slug_from_distinct_authors() {
        let a = Identity::generate().public_key();
        let b = Identity::generate().public_key();
        let v = dedup_newest(vec![opened(&a, "vault", 100), opened(&b, "vault", 100)]);
        assert_eq!(v.len(), 2, "the same slug from two authors must not be cross-collapsed");
    }

    #[test]
    fn dedup_empty_is_empty() {
        assert!(dedup_newest(vec![]).is_empty());
    }

    #[test]
    fn slugless_listing_dedup_key_is_deterministic_and_bounded() {
        // chorus M10: a slug-less listing must not key dedup off its raw JSON (unbounded +
        // encoding-sensitive). The fallback canonicalises + hashes, so two byte-different encodings
        // of the same content collapse, and the key stays a fixed-length digest.
        let k_a = listing_slug(r#"{"b":2,"a":1,"entries":[]}"#);
        let k_b = listing_slug(r#"{ "a":1, "b":2, "entries":[] }"#); // same content, different bytes
        assert_eq!(k_a, k_b, "same content ⇒ same fallback key regardless of encoding/whitespace");
        assert!(k_a.starts_with("sha256:") && k_a.len() < 80, "key is a bounded digest, not the JSON");

        // Two slug-less listings with the same author + same content collapse to one.
        let a = Identity::generate().public_key();
        let same = |t: u64| OpenedPrivate {
            listing_json: r#"{"entries":[],"x":1}"#.into(),
            inner_author: a,
            created_at: t,
        };
        assert_eq!(dedup_newest(vec![same(100), same(200)]).len(), 1, "slug-less same content dedups");
    }

    #[test]
    fn non_string_slug_falls_back_to_hash_not_the_number() {
        // opencode #4: a typed-but-not-string slug (e.g. a number) is not treated as a slug; it
        // takes the deterministic hash fallback rather than keying off a coerced value.
        let k = listing_slug(r#"{"slug":123,"entries":[]}"#);
        assert!(k.starts_with("sha256:"), "non-string slug falls back to the hash, got: {k}");
    }

    // -----------------------------------------------------------------------
    // QURATOR-160 receive side — the key-grant fetch core (`open_key_grant_wraps`, the pure half
    // of `fetch_key_grants`; the async half is fetch-glue identical to `fetch_private_listings`').
    // -----------------------------------------------------------------------

    /// The fetcher's core opens a REAL `seal_key_grant`-produced wrap (the exact frozen wire shape
    /// `grant_browse_access` publishes) and keeps exactly the trusted grant. The three decoys in
    /// the same 1059 inbox are each refused at the right seam: a stranger-author grant sealed to me
    /// opens fine but fails the allowlist (the trust check `open_key_grant` pushes to its caller);
    /// a grant sealed to someone else is unopenable; a private listing sealed to me fails
    /// `open_key_grant`'s inner-kind pin (31_114) — event confusion is refused here, not just in
    /// hb-core.
    ///
    /// MUTATION (P-10, for the orchestrator to apply — this lane compiles nothing itself): in
    /// `open_key_grant_wraps`, change `if allowlist.contains(&g.inner_author)` to `if true` — the
    /// stranger's grant survives and this reds on the `assert_eq!(got.len(), 1)`.
    #[test]
    fn open_key_grant_wraps_keeps_only_the_trusted_real_grant() {
        use hb_core::{seal_key_grant, seal_private_listing};

        let me = Identity::generate();
        let granter = Identity::generate();
        let stranger = Identity::generate();
        let someone_else = Identity::generate();
        let key = [0x5Au8; 32];
        let now = 1_700_000_000;

        let mut wraps = seal_key_grant(&granter, &[me.public_key()], &key, now).unwrap();
        // Openable by me, but the verified seal signer is a stranger → the trust check drops it.
        wraps.extend(seal_key_grant(&stranger, &[me.public_key()], &[0x11; 32], now).unwrap());
        // Sealed to another recipient → `me` cannot decrypt it → skipped.
        wraps.extend(
            seal_key_grant(&granter, &[someone_else.public_key()], &[0x22; 32], now).unwrap(),
        );
        // A private listing in the same inbox → wrong inner kind → the kind pin skips it.
        wraps.extend(
            seal_private_listing(&granter, &[me.public_key()], r#"{"slug":"vault"}"#, now).unwrap(),
        );

        let got = open_key_grant_wraps(&me, &wraps, &[granter.public_key()]);
        assert_eq!(got.len(), 1, "only the one trusted grant survives (stranger/wrong-recipient/listing all dropped)");
        assert_eq!(got[0].browse_key, key, "the granted browse key round-trips");
        assert_eq!(got[0].inner_author, granter.public_key(), "inner_author is the verified seal signer");
        assert_eq!(got[0].created_at, now);
    }

    /// A retried grant is a NEW outer event (fresh ephemeral key) with new inner content — dedup
    /// keeps the newest key per granter, and a second granter is never collapsed away.
    ///
    /// MUTATION (P-10): in `dedup_grants_newest`, change `prev.created_at >= g.created_at` to
    /// `prev.created_at <= g.created_at` — the OLDEST key survives and this reds on the `key_new`
    /// assertion.
    #[test]
    fn grant_dedup_keeps_the_newest_key_per_granter() {
        use hb_core::seal_key_grant;

        let me = Identity::generate();
        let a = Identity::generate();
        let b = Identity::generate();
        let key_old = [0x01u8; 32];
        let key_new = [0x02u8; 32];

        let mut wraps = seal_key_grant(&a, &[me.public_key()], &key_old, 100).unwrap();
        wraps.extend(seal_key_grant(&a, &[me.public_key()], &key_new, 200).unwrap());
        wraps.extend(seal_key_grant(&b, &[me.public_key()], &[0x03; 32], 150).unwrap());

        let got = open_key_grant_wraps(&me, &wraps, &[a.public_key(), b.public_key()]);
        assert_eq!(got.len(), 2, "one key per granter — two granters, two grants");
        let a_grant = got.iter().find(|g| g.inner_author == a.public_key()).unwrap();
        assert_eq!(a_grant.browse_key, key_new, "the newest grant's key wins");
        assert_eq!(a_grant.created_at, 200);
        assert!(
            got.iter().any(|g| g.inner_author == b.public_key()),
            "a second granter is not collapsed away"
        );
    }
}
