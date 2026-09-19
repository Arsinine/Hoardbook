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
//! QURATOR-247 adds a bounded negative cache ([`FAILED_OPENS`]) shared by both fetchers: a wrap
//! that fails to open once is not re-decrypted on the next poll of that shared inbox.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use hb_core::{open_key_grant, open_private_listing, HbError, Identity, OpenedKeyGrant, OpenedPrivate};
use nostr::prelude::*;

use crate::client::RelayClient;
use crate::error::NetError;
use crate::failed_wrap_cache::FailedWrapCache;

/// Relay-fetch budget for `me`'s personal gift-wrap inbox (QURATOR-291), shared by
/// [`fetch_private_listings`] and [`fetch_key_grants`] — both build the identical
/// `{kinds:[1059], #p:[me]}` filter documented at the top of this file as the same multiplexed
/// inbox `hb_net::topic`'s `fetch_join_requests` / `fetch_invite` also read (DMs, join requests,
/// invites, key grants and private listings all land on one `me`-tagged kind-1059 stream), so an
/// attacker who knows `me`'s pubkey can flood it with throwaway wraps; without a `.limit()` the
/// relay's own internal cap — not this budget — decides which of `me`'s real wraps come back.
/// Matches `topic::TOPIC_INBOX_FETCH_LIMIT` (500): same physical inbox, same traffic profile, no
/// basis for a different number here. Sized generously above any plausible backlog of pending
/// private listings / key grants a browse should ever need to consider — a bound on the fetch,
/// never on how many gift-wraps `me` may legitimately receive over time.
pub const PRIV_INBOX_FETCH_LIMIT: usize = 500;

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
/// junk wrap is *expected*, not an error — and, since QURATOR-247, an unopenable one is remembered
/// in [`FAILED_OPENS`] so the next poll does not re-decrypt it. A non-recipient calling this gets
/// an empty result: there is no "this is private" hint to find. Retried publishes collapse via
/// [`dedup_newest`].
pub async fn fetch_private_listings(
    client: &RelayClient,
    me: &Identity,
    allowlist: &[PublicKey],
    timeout: Duration,
) -> Result<Vec<OpenedPrivate>, NetError> {
    let wraps = client.fetch(priv_inbox_filter(me), timeout).await?;
    Ok(open_private_listing_wraps(me, &wraps, allowlist))
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
    let wraps = client.fetch(priv_inbox_filter(me), timeout).await?;
    Ok(open_key_grant_wraps(me, &wraps, allowlist))
}

/// Build the personal gift-wrap inbox filter — **pure** (no I/O), shared by
/// [`fetch_private_listings`] and [`fetch_key_grants`], which built byte-identical filters. One
/// builder means one budget and one place a mutation can bite, so the pinning test reds when
/// production loses its `.limit()`. ⚠ A test that rebuilds this filter inline instead of calling
/// here asserts against a lookalike it controls and stays green under that mutation — the
/// 2026-09-16 sweep caught exactly that (CLAUDE.md §9 P-6). Call this, never a copy.
pub(crate) fn priv_inbox_filter(me: &Identity) -> Filter {
    Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key()).limit(PRIV_INBOX_FETCH_LIMIT)
}

/// Negative-cache scope tag for private-listing opens (inner kind 31_113). Scopes exist because
/// the two consumers of this inbox have NON-interchangeable failure verdicts: a valid private
/// listing fails `open_key_grant`'s inner-kind pin (31_114) and a valid grant fails
/// `open_private_listing`'s — one undiscriminated key space would let whichever core polled first
/// blacklist the other's perfectly good wrap. Pinned by
/// `kind_pin_failures_do_not_cross_the_inner_kind_scopes`.
const LISTING_OPEN_SCOPE: &str = "priv-listing";
/// Negative-cache scope tag for key-grant opens (inner kind 31_114). See [`LISTING_OPEN_SCOPE`].
const GRANT_OPEN_SCOPE: &str = "key-grant";

/// The private-inbox failed-open negative cache (QURATOR-247): gift-wrap ids that failed
/// `open_private_listing`/`open_key_grant`, so a repeated poll of the shared `{kinds:[1059],
/// #p:[me]}` inbox (QURATOR-291's flood surface) does not re-run NIP-44 decrypt + Schnorr verify
/// on attacker junk forever — bounded only by [`PRIV_INBOX_FETCH_LIMIT`] before this cache. One
/// bounded [`FailedWrapCache`] — the TYPE shared with hb-app's DM poller (see
/// `failed_wrap_cache`'s module doc for why the type is shared but entries never are) — keyed
/// (identity npub, inner-kind scope, wrap id). Module-scope static (house rule: never a
/// per-function static); `std::sync::Mutex`, not tokio — the check and the record are separate
/// synchronous spans, so the lock is never held across the fetch `.await`.
///
/// **"Failed to open" is a stable property per (identity, wrap)** — that is what makes a negative
/// cache safe here, and the question QURATOR-247 asks to answer explicitly. `open_private_listing`
/// / `open_key_grant` are offline-pure over `(me's keys, wrap bytes)`: NIP-44 decrypt
/// (ECDH + AES-GCM), Schnorr verify, the inner-kind pin and the JSON parse are all deterministic
/// — no clock, RNG or network enters the verdict — so there is NO transient failure to blacklist:
/// a wrap that failed cannot later succeed under the same keys. The two inputs that CAN change
/// are excluded by construction, each pinned by a test: the IDENTITY (keyed by npub — a wrap that
/// fails under A may simply be addressed to B; `failure_cache_does_not_cross_identity_boundaries`)
/// and the ALLOWLIST (trust is filtered AFTER the cache layer and never recorded — contacts can be
/// added later; `untrusted_but_openable_listing_is_not_negatively_cached`). In-memory only, never
/// persisted: a restart re-attempts each remembered wrap exactly once and never loses a listing.
///
/// Residual, deliberately uncovered: a well-formed wrap from an untrusted author re-opens on
/// every poll (it never enters this cache — see the allowlist note). That is the price of the
/// allowlist being mutable, unchanged from before this cache, and outside QURATOR-247's named
/// threat (junk wraps that fail to open).
static FAILED_OPENS: LazyLock<Mutex<FailedWrapCache>> =
    LazyLock::new(|| Mutex::new(FailedWrapCache::new()));

/// Compose the negative-cache key: (identity npub, scope, wrap id). `\x00` appears in none of
/// bech32 (npub), the scope tags, or hex (wrap id), so the join is collision-free.
fn failed_open_key(me_npub: &str, scope: &str, wrap_id: &str) -> String {
    format!("{me_npub}\u{0}{scope}\u{0}{wrap_id}")
}

/// O(1) "already failed to open under THIS identity and inner kind?" — checked before the open so
/// a previously-failed wrap is not re-decrypted on every poll.
fn failed_open_seen(me_npub: &str, scope: &str, id: &str) -> bool {
    FAILED_OPENS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&failed_open_key(me_npub, scope, id))
}

/// Record a (identity, scope, wrap id) that failed to open (bounded, FIFO-evicted). Only the
/// deterministic open verdict is ever recorded here — never an allowlist rejection.
fn record_failed_open(me_npub: &str, scope: &str, id: String) {
    FAILED_OPENS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(failed_open_key(me_npub, scope, &id));
}

/// Open one wrap through the negative cache — the shared per-wrap core of BOTH inbox consumers:
/// a remembered id is skipped without attempting the open; a failed open is remembered; an opened
/// wrap is returned for the CALLER's trust decision, which stays outside this fn on purpose (see
/// [`FAILED_OPENS`]'s allowlist note).
fn open_cached<T>(
    me: &Identity,
    me_npub: &str,
    wrap: &Event,
    scope: &str,
    open: fn(&Identity, &Event) -> Result<T, HbError>,
) -> Option<T> {
    let id = wrap.id.to_hex();
    if failed_open_seen(me_npub, scope, &id) {
        return None; // failed under THIS identity + inner kind before — deterministic, skip the re-open
    }
    match open(me, wrap) {
        Ok(t) => Some(t),
        Err(_) => {
            record_failed_open(me_npub, scope, id);
            None
        }
    }
}

/// The pure open/trust/dedup core of [`fetch_private_listings`] — everything after the relay
/// fetch, the exact sibling of [`open_key_grant_wraps`] (QURATOR-247 extracted it so both inbox
/// consumers are open/trust/dedup cores over the same [`open_cached`] seam, testable identically
/// without a relay).
pub(crate) fn open_private_listing_wraps(
    me: &Identity,
    wraps: &[Event],
    allowlist: &[PublicKey],
) -> Vec<OpenedPrivate> {
    let me_npub = me.npub();
    let mut trusted: Vec<OpenedPrivate> = Vec::new();
    for w in wraps {
        // None = cached skip (failed under this identity + scope on a prior poll).
        if let Some(o) = open_cached(me, &me_npub, w, LISTING_OPEN_SCOPE, open_private_listing) {
            if allowlist.contains(&o.inner_author) {
                trusted.push(o);
            }
            // Deliberately NOT recorded when merely untrusted: the open SUCCEEDED (the
            // deterministic verdict is "openable") and the allowlist is mutable state that can
            // grow to include this author later — a cached skip would then hide their listing
            // until restart. Pinned by `untrusted_but_openable_listing_is_not_negatively_cached`.
        }
    }
    dedup_newest(trusted)
}

/// The pure open/trust/dedup core of [`fetch_key_grants`] — everything after the relay fetch, so a
/// real `seal_key_grant`-produced wrap round-trips through production code without a relay (the
/// same discipline that makes [`dedup_newest`] the tested half of `fetch_private_listings`).
/// Shares [`open_cached`] with [`open_private_listing_wraps`] so the negative-cache skip/record
/// behaviour cannot drift between the two consumers of this inbox (QURATOR-247).
pub(crate) fn open_key_grant_wraps(
    me: &Identity,
    wraps: &[Event],
    allowlist: &[PublicKey],
) -> Vec<OpenedKeyGrant> {
    let me_npub = me.npub();
    let mut trusted: Vec<OpenedKeyGrant> = Vec::new();
    for w in wraps {
        if let Some(g) = open_cached(me, &me_npub, w, GRANT_OPEN_SCOPE, open_key_grant) {
            if allowlist.contains(&g.inner_author) {
                trusted.push(g);
            }
            // Not recorded when merely untrusted — same allowlist-mutability reason as
            // [`open_private_listing_wraps`].
        }
    }
    dedup_grants_newest(trusted)
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
    fn inbox_fetch_filter_declares_an_explicit_limit_qurator_291() {
        // mutation: delete the `.limit(...)` call from the chain inside `priv_inbox_filter` (or
        // change the const's declared value) — either reds this.
        //
        // ⚠ This test CALLS the production builder, which `fetch_private_listings` and
        // `fetch_key_grants` now BOTH use. An earlier version rebuilt the filter inline and the
        // 2026-09-16 mutation sweep proved it vacuous — deleting production's `.limit()` left it
        // green. Never reconstruct this filter here.
        let me = Identity::generate();
        let f = priv_inbox_filter(&me);
        assert_eq!(f.limit, Some(PRIV_INBOX_FETCH_LIMIT), "inbox fetch budget is declared explicitly");
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

    // -----------------------------------------------------------------------
    // QURATOR-247 — the failed-open negative cache. Every test generates a FRESH `me`, so its
    // (npub, scope, id) slice of `FAILED_OPENS` starts empty — the same hygiene note chat.rs's
    // FAILED_WRAPS tests carry. All of these call the production cores (`open_private_listing_wraps`
    // / `open_key_grant_wraps`) and the production cache accessors, never a rebuilt copy.
    // -----------------------------------------------------------------------

    /// A junk wrap (sealed to someone else — `me` cannot decrypt it) is skipped silently AND
    /// remembered, so the next poll of the same inbox does not re-run the open. This is the
    /// record half of acceptance #1; the skip half is
    /// `cached_failure_skips_the_open_entirely_qurator_247`.
    ///
    /// MUTATION (P-10, for the orchestrator to apply): in `open_cached` (priv_browse.rs, the
    /// `match open(me, wrap)` block), delete the `Err(_)` arm's `record_failed_open(me_npub,
    /// scope, id);` — the junk wrap is no longer remembered and the `failed_open_seen` assert
    /// reds.
    #[test]
    fn failed_listing_open_is_remembered_for_the_next_poll_qurator_247() {
        use hb_core::seal_private_listing;

        let me = Identity::generate();
        let author = Identity::generate();
        let someone_else = Identity::generate();

        let mut wraps =
            seal_private_listing(&author, &[me.public_key()], r#"{"slug":"vault"}"#, 100).unwrap();
        let junk =
            seal_private_listing(&author, &[someone_else.public_key()], r#"{"slug":"not-for-me"}"#, 100)
                .unwrap();
        let junk_id = junk[0].id.to_hex();
        wraps.extend(junk);

        let got = open_private_listing_wraps(&me, &wraps, &[author.public_key()]);
        assert_eq!(got.len(), 1, "the junk wrap is skipped silently — expected foreign traffic");
        assert_eq!(got[0].inner_author, author.public_key(), "the surviving listing is the trusted author's");
        assert!(
            failed_open_seen(&me.npub(), LISTING_OPEN_SCOPE, &junk_id),
            "the unopenable wrap is remembered in the negative cache"
        );
        // The next poll of the same inbox: same output.
        let got2 = open_private_listing_wraps(&me, &wraps, &[author.public_key()]);
        assert_eq!(got2.len(), 1, "poll 2 is output-identical (the junk wrap now a cache hit)");
    }

    /// The skip half of acceptance #1: a wrap remembered as failed is not opened at all. The
    /// manual `record_failed_open` simulates a prior poll's failure for an otherwise fully valid,
    /// trusted wrap — which is exactly what production state looks like to the second poll.
    ///
    /// MUTATION (P-10): in `open_cached`, delete the `if failed_open_seen(me_npub, scope, &id)
    /// { return None; }` guard — the cached wrap is re-opened (it is genuinely valid), comes
    /// back, and the `is_empty` assert reds.
    #[test]
    fn cached_failure_skips_the_open_entirely_qurator_247() {
        use hb_core::seal_private_listing;

        let me = Identity::generate();
        let author = Identity::generate();
        let wraps =
            seal_private_listing(&author, &[me.public_key()], r#"{"slug":"vault"}"#, 100).unwrap();
        // Simulate a prior poll that failed to open this wrap under THIS identity.
        record_failed_open(&me.npub(), LISTING_OPEN_SCOPE, wraps[0].id.to_hex());

        let got = open_private_listing_wraps(&me, &wraps, &[author.public_key()]);
        assert!(
            got.is_empty(),
            "a cached failure is skipped without attempting the open — cache gates BEFORE decrypt"
        );
    }

    /// A wrap sealed to B fails under A (foreign traffic, remembered under A) but must still open
    /// under B — the cache key's identity segment is what keeps one identity's failure from
    /// blacklisting another's valid listing (wipe/restore, or a relay feeding B's wrap while A
    /// polls). Mirrors chat.rs's `negative_cache_does_not_cross_identity_boundaries`.
    ///
    /// MUTATION (P-10): in `failed_open_key`, drop the identity segment — change the format! to
    /// `format!("{scope}\u{0}{wrap_id}")` — B now sees A's entry, the listing is skipped, and the
    /// final assert reds.
    #[test]
    fn failure_cache_does_not_cross_identity_boundaries() {
        use hb_core::seal_private_listing;

        let a = Identity::generate();
        let b = Identity::generate();
        let author = Identity::generate();
        let wraps = seal_private_listing(&author, &[b.public_key()], r#"{"slug":"for-b"}"#, 100).unwrap();
        let id = wraps[0].id.to_hex();

        let got_a = open_private_listing_wraps(&a, &wraps, &[author.public_key()]);
        assert!(got_a.is_empty(), "A cannot open a wrap sealed to B — expected, not an error");
        assert!(failed_open_seen(&a.npub(), LISTING_OPEN_SCOPE, &id), "…remembered under A");

        let got_b = open_private_listing_wraps(&b, &wraps, &[author.public_key()]);
        assert_eq!(got_b.len(), 1, "B's genuinely valid listing is NOT poisoned by A's failure");
    }

    /// The two inner kinds share one inbox but must not share one failure entry: a valid private
    /// listing fails `open_key_grant`'s kind pin (31_114) — that refusal is remembered under the
    /// GRANT scope only, and the listing core must still open it. Without the scope segment in
    /// the key, whichever core polled first would blacklist the other's perfectly good wrap.
    ///
    /// MUTATION (P-10): in `failed_open_key`, drop the scope segment — change the format! to
    /// `format!("{me_npub}\u{0}{wrap_id}")` — the grant core's refusal now poisons the listing
    /// open and the final assert reds.
    #[test]
    fn kind_pin_failures_do_not_cross_the_inner_kind_scopes() {
        use hb_core::seal_private_listing;

        let me = Identity::generate();
        let author = Identity::generate();
        let wraps = seal_private_listing(&author, &[me.public_key()], r#"{"slug":"vault"}"#, 100).unwrap();
        let id = wraps[0].id.to_hex();

        let grants = open_key_grant_wraps(&me, &wraps, &[author.public_key()]);
        assert!(grants.is_empty(), "a listing is not a grant — refused by the kind pin");
        assert!(
            failed_open_seen(&me.npub(), GRANT_OPEN_SCOPE, &id),
            "the kind-pin refusal is remembered under the GRANT scope"
        );
        assert!(
            !failed_open_seen(&me.npub(), LISTING_OPEN_SCOPE, &id),
            "…and NOT under the listing scope"
        );
        let got = open_private_listing_wraps(&me, &wraps, &[author.public_key()]);
        assert_eq!(got.len(), 1, "the grant core's kind-pin refusal does not poison the listing core");
    }

    /// A wrap that OPENS but whose author is untrusted is dropped by the trust check WITHOUT being
    /// negatively cached: the open's verdict ("openable") is the only deterministic, cacheable
    /// fact, while the allowlist is mutable — add the author as a contact and their listing must
    /// surface on the very next poll, which a cached skip would hide until restart.
    ///
    /// MUTATION (P-10): in `open_private_listing_wraps`, add an `else { record_failed_open(
    /// &me.npub(), LISTING_OPEN_SCOPE, w.id.to_hex()); }` to the `if allowlist.contains(
    /// &o.inner_author)` — poll 1 now caches the stranger's wrap and poll 2 (allowlist grown)
    /// returns empty, redding the final assert (and the `!failed_open_seen` assert before it).
    #[test]
    fn untrusted_but_openable_listing_is_not_negatively_cached() {
        use hb_core::seal_private_listing;

        let me = Identity::generate();
        let stranger = Identity::generate();
        let wraps = seal_private_listing(&stranger, &[me.public_key()], r#"{"slug":"theirs"}"#, 100).unwrap();
        let id = wraps[0].id.to_hex();

        // Poll 1: nobody trusted — the stranger's wrap opens fine, then fails the trust check.
        let got = open_private_listing_wraps(&me, &wraps, &[]);
        assert!(got.is_empty(), "an untrusted author is dropped by the trust check");
        assert!(
            !failed_open_seen(&me.npub(), LISTING_OPEN_SCOPE, &id),
            "an OPENED wrap is never recorded: the allowlist can grow to include the author"
        );

        // Poll 2: the allowlist grew — the listing now arrives, impossible had poll 1 cached it.
        let got2 = open_private_listing_wraps(&me, &wraps, &[stranger.public_key()]);
        assert_eq!(
            got2.len(),
            1,
            "adding the author to the allowlist surfaces their listing on the next poll"
        );
    }

    /// The grant core remembers its own junk symmetrically with the listing core (acceptance #2:
    /// both fetchers share the inbox, so both must carry the cache).
    ///
    /// MUTATION (P-10): in `open_key_grant_wraps`, change the `open_cached(...)` call's
    /// `GRANT_OPEN_SCOPE` argument to `LISTING_OPEN_SCOPE` — the record lands in the wrong scope
    /// and the `failed_open_seen(... GRANT_OPEN_SCOPE ...)` assert reds.
    #[test]
    fn failed_grant_open_is_remembered_qurator_247() {
        use hb_core::seal_key_grant;

        let me = Identity::generate();
        let granter = Identity::generate();
        let someone_else = Identity::generate();

        let mut wraps = seal_key_grant(&granter, &[me.public_key()], &[0x5Au8; 32], 100).unwrap();
        let junk = seal_key_grant(&granter, &[someone_else.public_key()], &[0x11; 32], 100).unwrap();
        let junk_id = junk[0].id.to_hex();
        wraps.extend(junk);

        let got = open_key_grant_wraps(&me, &wraps, &[granter.public_key()]);
        assert_eq!(got.len(), 1, "only the real grant survives — the junk wrap skipped silently");
        assert!(
            failed_open_seen(&me.npub(), GRANT_OPEN_SCOPE, &junk_id),
            "the unopenable grant wrap is remembered under the grant scope"
        );
    }
}
