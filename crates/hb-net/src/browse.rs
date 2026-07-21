//! The M3 application value loop — **publish → discover → browse** — composed over the M2 client
//! and M1 core. This is *application orchestration*, deliberately placed in `hb-net` (M3 decision
//! #1, option a) so the `hb-it` L2 suite drives the **real production code** `hb-app` will call,
//! not a parallel reimplementation. The pure pieces it leans on live in sibling modules
//! (`render`, `cache`, `discover`) so they stay unit-testable without a relay; the async functions
//! here are proven end-to-end at L2.
//!
//! A browse is a **relay read + a local decrypt** — it composes only `RelayClient::fetch` and
//! `hb-core` parsers, and **never** touches iroh or any peer socket (AB10). The browse-key gates
//! the listing: a follow-only share code (bare `npub`) yields the teaser only; a wrong browse-key
//! yields the teaser with the listing locked (decrypt fails cleanly, not a hard browse error).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use hb_core::event::{
    build_listing_event, parse_listing_event, parse_teaser, Teaser, KIND_LISTING, KIND_TEASER,
};
use hb_core::listing::BrowseKey;
use hb_core::{Identity, ShareCode};
use nostr::prelude::*;

use crate::client::{teaser_search_filter, RelayClient};
use crate::discover::{ingest_teasers, select_newest_by_created_at, SearchHit};
use crate::error::NetError;
use crate::nip65::{bootstrap_order, inbox_order, parse_relay_list};
use crate::render::{render_listing, RenderedListing};
use crate::split::{split_listing, truncate_listing};

/// Parse a pasted share code, surfacing `hb-core`'s codec rejection as a `NetError`. A bare `npub`
/// is follow-only (no browse-key); a full `hbk1…` carries the browse-key; anything malformed
/// (non-bech32, corrupt bytes, truncated, wrong version) returns a clean `Err`, never a panic.
pub fn parse_share_code(s: &str) -> Result<ShareCode, NetError> {
    Ok(ShareCode::parse(s)?)
}

/// The outcome of publishing a listing.
#[derive(Debug, Clone)]
pub struct PublishedListing {
    /// How many parts were published (1 = unsplit/truncated; >1 = split index + content parts).
    pub parts: usize,
    /// devtest #7 — whether the listing was truncated to fit a single event (a paywall teaser).
    pub truncated: bool,
    /// Item nodes shown vs total (only meaningful when `truncated`); `shown == total` otherwise.
    pub shown_items: usize,
    pub total_items: usize,
}

/// Publish a collection listing: encrypt under `browse_key`, split per-folder when it exceeds
/// `max_bytes`, and publish the index + every content part as parameterized-replaceable events
/// (re-publishing the same slug supersedes the prior listing — N3). Re-keying is just supplying a
/// fresh `browse_key` here (the per-collection symmetric key, not the `npub` identity).
pub async fn publish_listing(
    client: &RelayClient,
    identity: &Identity,
    slug: &str,
    browse_key: &BrowseKey,
    listing_json: &str,
    max_bytes: usize,
) -> Result<PublishedListing, NetError> {
    let parts = split_listing(slug, listing_json, max_bytes)?;
    for part in &parts {
        let event = build_listing_event(identity, &part.d_tag, browse_key, &part.json)?;
        client.publish(&event).await?;
    }
    Ok(PublishedListing { parts: parts.len(), truncated: false, shown_items: 0, total_items: 0 })
}

/// Publish a collection listing as a SINGLE event, truncating it (paywall-style) to `max_bytes`
/// instead of splitting an oversize listing across many part events (devtest #7). A listing that fits
/// publishes whole; an oversize one publishes a byte-bounded prefix of its tree tagged `truncated` +
/// `total_items` so a browser renders the kept items behind a "N more hidden" fade. One write, so the
/// relay-write rate limiter never sees a part flood — the whole reason the owner chose truncation
/// over the M13 split for large collections. Same parameterized-replaceable `d = slug` as the unsplit
/// fast path (re-publishing supersedes — N3).
pub async fn publish_listing_capped(
    client: &RelayClient,
    identity: &Identity,
    slug: &str,
    browse_key: &BrowseKey,
    listing_json: &str,
    max_bytes: usize,
) -> Result<PublishedListing, NetError> {
    let t = truncate_listing(listing_json, max_bytes)?;
    let event = build_listing_event(identity, slug, browse_key, &t.json)?;
    client.publish(&event).await?;
    Ok(PublishedListing {
        parts: 1,
        truncated: t.truncated,
        shown_items: t.shown_items,
        total_items: t.total_items,
    })
}

/// The result of browsing a share code: the peer's public teaser (if any), the decrypted listing
/// tree (if a browse-key was held and the listing decrypted), and the NIP-65-resolved relay order
/// the browse used (DISC2 evidence).
#[derive(Debug, Clone)]
pub struct BrowseResult {
    pub teaser: Option<Teaser>,
    pub listing: Option<RenderedListing>,
    pub resolved_relays: Vec<String>,
}

/// Resolve where a peer's events live (NIP-65 first-contact bootstrap, spec §Discovery): fetch the
/// peer's kind-10002 relay list, **pin it to the peer's npub** (a lying relay can't substitute
/// someone else's list), and order via [`bootstrap_order`] — peer outbox first, then seed + own.
/// `bootstrap_order` only *orders*; this function does the NIP-65 fetch itself.
pub async fn resolve_peer_relays(
    client: &RelayClient,
    peer: &PublicKey,
    seed: &[String],
    own: &[String],
    timeout: Duration,
) -> Vec<String> {
    let peer_list = match client.fetch(Filter::new().author(*peer).kind(Kind::RelayList), timeout).await
    {
        // Author-pin, then pick the **newest** relay-list (a non-compliant relay may return more
        // than one kind-10002 for an author — the latest is authoritative, never the first seen).
        Ok(events) => {
            let pinned: Vec<Event> = events.into_iter().filter(|e| &e.pubkey == peer).collect();
            select_newest_by_created_at(pinned).and_then(|e| parse_relay_list(&e).ok())
        }
        Err(_) => None,
    };
    bootstrap_order(seed, own, peer_list.as_ref())
}

/// Resolve a DM recipient's **read** relays (their inbox) for delivery (spec §9, M12 W2). The mirror
/// of [`resolve_peer_relays`], read side: fetch the recipient's kind-10002, **pin it to their npub**
/// (a lying relay can't substitute someone else's list), take the newest, and order via
/// [`inbox_order`] — recipient read first, then your own + seed (best-effort fallback when no list).
/// The returned set is what [`RelayClient::publish_to`] targets, so the wrap reaches the inbox and
/// your own relays but **no unrelated accreted relay** (chorus #3).
pub async fn resolve_recipient_relays(
    client: &RelayClient,
    recipient: &PublicKey,
    seed: &[String],
    own: &[String],
    timeout: Duration,
) -> Vec<String> {
    let read_list = match client
        .fetch(Filter::new().author(*recipient).kind(Kind::RelayList), timeout)
        .await
    {
        Ok(events) => {
            let pinned: Vec<Event> = events.into_iter().filter(|e| &e.pubkey == recipient).collect();
            select_newest_by_created_at(pinned).and_then(|e| parse_relay_list(&e).ok())
        }
        // No NIP-65 found / fetch error → best-effort to own + seed (chorus round-1: log, don't fail
        // silently — a DM may then not reach a recipient on a disjoint relay set).
        Err(e) => {
            tracing::debug!("DM delivery: could not resolve recipient read-relays ({e}); falling back to own/seed");
            None
        }
    };
    inbox_order(seed, own, read_list.as_ref())
}

/// Browse a share code as a pure relay read. Always returns the teaser when present; the listing is
/// `None` for a follow-only code or when the held browse-key can't decrypt it (locked, not an
/// error). `seed`/`own` seed the NIP-65 bootstrap.
pub async fn browse_share_code(
    client: &RelayClient,
    share_code: &ShareCode,
    slug: &str,
    seed: &[String],
    own: &[String],
    timeout: Duration,
) -> Result<BrowseResult, NetError> {
    let peer = share_code.pubkey();
    let resolved_relays = resolve_peer_relays(client, &peer, seed, own, timeout).await;
    // **Act on** the NIP-65 resolution: connect to the peer's advertised outbox before fetching, so
    // a peer who publishes only to their own relays (not the caller's seed set) is still reachable.
    let _ = client.ensure_relays(&resolved_relays, timeout).await;

    // Teaser (public): newest by created_at, then verify+parse.
    let teaser_events =
        client.fetch(Filter::new().author(peer).kind(Kind::from_u16(KIND_TEASER)), timeout).await?;
    let teaser = select_newest_by_created_at(teaser_events).and_then(|e| parse_teaser(&e).ok());

    // Listing (gated by the browse-key): a decrypt failure locks the listing without failing the
    // whole browse — the teaser still shows (BR1).
    let listing = match share_code.browse_key() {
        Some(bk) => fetch_listing(client, &peer, slug, &bk, timeout).await.ok(),
        None => None,
    };

    Ok(BrowseResult { teaser, listing, resolved_relays })
}

/// Fetch a slug's listing family (index + content parts), pick the newest event per `d`-tag (so a
/// non-compliant relay's stale replaceable duplicate can't win — N3/AB8), decrypt each with the
/// browse-key (which re-verifies the Schnorr signature), and render into a possibly-partial tree.
async fn fetch_listing(
    client: &RelayClient,
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
    timeout: Duration,
) -> Result<RenderedListing, NetError> {
    // M4 optimisation: this fetches all of the peer's listing events and filters to the slug family
    // client-side. A two-phase fetch (the `d=slug` index first, then exactly its `d=slug#partI`
    // parts) would avoid pulling a prolific author's other collections — deferred (the read-side
    // `MAX_LISTING_PARTS` cap bounds the worst case regardless).
    let events =
        client.fetch(Filter::new().author(*peer).kind(Kind::from_u16(KIND_LISTING)), timeout).await?;
    render_slug_family(events, peer, slug, browse_key)
}

/// Group fetched `KIND_LISTING` events into one slug's family (the `d=slug` index/single + its
/// `d=slug#partN` content parts), take the **newest event per `d`** (a non-compliant relay's stale
/// replaceable duplicate can't win — N3/AB8), decrypt each with the browse-key (which re-verifies the
/// Schnorr signature), and render into a possibly-partial tree. Shared by the pool-wide
/// [`fetch_listing`] and the big-relay-targeted [`fetch_full_listing_from`] (M16 W2) so both read the
/// exact same family-assembly logic.
fn render_slug_family(
    events: Vec<Event>,
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
) -> Result<RenderedListing, NetError> {
    let part_prefix = format!("{slug}#part");
    let mut by_d: HashMap<String, Vec<Event>> = HashMap::new();
    for ev in events {
        // Author pin (Codex review, M16 W2): the relay-side `author(peer)` filter is not enough — a
        // lying relay can return a *validly-signed* listing from another key (a share-code holder can
        // encrypt a family under the shared browse-key and sign it themselves), and nostr-sdk does not
        // verify that fetched events match the requested filter. Drop anything not authored by the
        // browsed peer before it can win the newest-per-`d` selection or be decrypted + rendered.
        if ev.pubkey != *peer {
            continue;
        }
        if let Some(d) = ev.tags.identifier() {
            if d == slug || d.starts_with(&part_prefix) {
                by_d.entry(d.to_string()).or_default().push(ev);
            }
        }
    }
    if by_d.is_empty() {
        return Err(NetError::Split(format!("no listing found for slug '{slug}'")));
    }

    let mut payloads: Vec<String> = Vec::new();
    for (_d, group) in by_d {
        if let Some(ev) = select_newest_by_created_at(group) {
            let (_slug, json) = parse_listing_event(&ev, browse_key)?;
            payloads.push(json);
        }
    }
    render_listing(&payloads)
}

/// Publish a collection listing's FULL family to a **targeted** relay set only (M16 W2 — the
/// big-relay carrier). Identical per-folder splitting to [`publish_listing`], but every part event is
/// delivered via [`RelayClient::publish_to`] to `relays` (the owner's own big relay) — never the
/// whole connected pool. So the full family lands on the big relay while public relays keep only the
/// truncated paywall teaser (INV-5: the big-relay family never broadcasts to public relays). The
/// caller must have `relays` connected (as with [`RelayClient::publish_to`]).
///
/// `max_bytes` is the **same** per-part budget as the normal path (owner ruling 2026-07-16: the big
/// relay reuses it — its advantage is being owner-run with no ban risk and accepting the whole
/// family, not carrying bigger events; a part is a NIP-44-encrypted event, capped at the 65_408-byte
/// plaintext limit regardless of any relay's `maxEventSize`).
pub async fn publish_listing_to(
    client: &RelayClient,
    identity: &Identity,
    slug: &str,
    browse_key: &BrowseKey,
    listing_json: &str,
    max_bytes: usize,
    relays: &[String],
) -> Result<PublishedListing, NetError> {
    let parts = split_listing(slug, listing_json, max_bytes)?;
    for part in &parts {
        let event = build_listing_event(identity, &part.d_tag, browse_key, &part.json)?;
        client.publish_to(&event, relays).await?;
    }
    Ok(PublishedListing { parts: parts.len(), truncated: false, shown_items: 0, total_items: 0 })
}

/// Fetch a slug's FULL listing family from a **targeted** relay set (the big relay) and render it —
/// the read-side counterpart of [`publish_listing_to`] (M16 W2). Reads `relays` **exclusively** (via
/// [`RelayClient::fetch_from`]) so the big-relay split family is not collided with the `d=slug`
/// truncated teaser living on the public relays. Assembles + renders exactly like [`fetch_listing`].
pub async fn fetch_full_listing_from(
    client: &RelayClient,
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
    relays: &[String],
    timeout: Duration,
) -> Result<RenderedListing, NetError> {
    let events = client
        .fetch_from(relays, Filter::new().author(*peer).kind(Kind::from_u16(KIND_LISTING)), timeout)
        .await?;
    render_slug_family(events, peer, slug, browse_key)
}

/// The full-tree **snapshot fingerprint** a rendered listing carries in its metadata (M16). The
/// hoarder writes it into the listing JSON at publish time (W3); it rides through split/restitch into
/// `RenderedListing.meta` for free (top-level metadata is preserved). `None` for a pre-M16 listing
/// that predates the field.
pub fn listing_snapshot_fingerprint(rendered: &RenderedListing) -> Option<&str> {
    rendered.meta.get("snapshot_fingerprint").and_then(serde_json::Value::as_str)
}

/// The M16 staleness gate: a fetched full family supersedes the paywall teaser **only** if it is
/// **complete** AND its snapshot fingerprint matches the teaser's. Completeness is load-bearing
/// (Codex review, W2): a big relay can serve the signed index but withhold content parts —
/// `render_listing` yields a *partial* tree that still carries the matching fingerprint in its meta,
/// and silently replacing the paywall with a partial tree presented as the full list would be a
/// downgrade. An incomplete, unfingerprinted (pre-M16), or mismatched (stale) family does not
/// supersede — keep the teaser and surface "ask again".
fn full_supersedes(rendered: &RenderedListing, expected_fingerprint: &str) -> bool {
    rendered.complete() && listing_snapshot_fingerprint(rendered) == Some(expected_fingerprint)
}

/// Fetch the big-relay full family **only if it is current** — its snapshot fingerprint matches
/// `expected_fingerprint` (the truncated teaser's). A mismatch (the big relay holds a stale older
/// snapshot), an absent family, or any fetch failure yields `Ok(None)`, so the caller keeps the
/// paywall teaser rather than serving stale or un-gated data (M16 headline failure mode #1). Only a
/// current, verified full tree returns `Ok(Some(_))`.
pub async fn fetch_full_listing_if_current(
    client: &RelayClient,
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
    relays: &[String],
    expected_fingerprint: &str,
    timeout: Duration,
) -> Result<Option<RenderedListing>, NetError> {
    let rendered = match fetch_full_listing_from(client, peer, slug, browse_key, relays, timeout).await
    {
        Ok(r) => r,
        // Absent / locked / unreadable big-relay family → keep the teaser (never a hard browse error).
        Err(e) => {
            tracing::debug!("big-relay full listing for '{slug}' unavailable ({e}); keeping the teaser");
            return Ok(None);
        }
    };
    if full_supersedes(&rendered, expected_fingerprint) {
        Ok(Some(rendered))
    } else {
        tracing::debug!("big-relay full listing for '{slug}' is a stale/unfingerprinted snapshot; keeping the teaser");
        Ok(None)
    }
}

/// Fetch, decrypt, and render EVERY listing family a peer has published (grouped by root slug).
///
/// The multi-collection generalisation of [`fetch_listing`] (M13): one `KIND_LISTING` fetch by
/// author, grouped into families by **root slug** (the `d` up to `#part`), newest event per `d`
/// (so a non-compliant relay's stale replaceable duplicate can't win — N3/AB8), then decrypt +
/// render each family independently. A family that fails to decrypt or render is **skipped** —
/// locked ≠ error, mirroring BR1 — so one re-keyed or corrupt collection can't hide the rest.
/// Families come back sorted by root slug (deterministic across fetches). The third tuple element is
/// the **index (teaser) event id** — the id of the `d = root` event the browser sees — so a manifest
/// request (M16 W4) can name the exact teaser event; `None` if that event had no recoverable id.
pub async fn browse_peer_listings(
    client: &RelayClient,
    peer: &PublicKey,
    browse_key: &BrowseKey,
    timeout: Duration,
) -> Result<Vec<(String, RenderedListing, Option<String>)>, NetError> {
    let events =
        client.fetch(Filter::new().author(*peer).kind(Kind::from_u16(KIND_LISTING)), timeout).await?;

    // Group by root slug, keeping every event per full `d` so the newest per replaceable
    // identifier wins below (BTreeMap ⇒ output sorted by root slug).
    let mut families: BTreeMap<String, HashMap<String, Vec<Event>>> = BTreeMap::new();
    for ev in events {
        if let Some(d) = ev.tags.identifier() {
            let root = match d.find("#part") {
                Some(i) => &d[..i],
                None => d,
            };
            families.entry(root.to_string()).or_default().entry(d.to_string()).or_default().push(ev);
        }
    }

    let mut out = Vec::new();
    'family: for (root, by_d) in families {
        let mut payloads: Vec<String> = Vec::new();
        let mut teaser_event_id: Option<String> = None;
        for (d, group) in by_d {
            if let Some(ev) = select_newest_by_created_at(group) {
                // The index/single event (`d == root`) IS the teaser the browser renders; capture its
                // id so a manifest request can name the exact teaser event (M16 W4).
                if d == root {
                    teaser_event_id = Some(ev.id.to_hex());
                }
                match parse_listing_event(&ev, browse_key) {
                    Ok((_slug, json)) => payloads.push(json),
                    // Wrong browse-key (locked) or malformed event → skip the whole family.
                    Err(_) => continue 'family,
                }
            }
        }
        if let Ok(rendered) = render_listing(&payloads) {
            out.push((root, rendered, teaser_event_id));
        }
    }
    Ok(out)
}

/// Discover peers by tag-search over public teasers: build the filter (empty∧empty → `Err`,
/// DISC4), fetch, then ingest — bound size, discard bad-sig, AND-tags / OR-content-types, dedup by
/// `npub`, cap (AB3/DISC1). A hit yields the teaser only, never a listing (DISC3).
pub async fn search_teasers(
    client: &RelayClient,
    tags: &[String],
    content_types: &[String],
    cap: usize,
    timeout: Duration,
) -> Result<Vec<SearchHit>, NetError> {
    let filter = teaser_search_filter(tags, content_types)?;
    let events = client.fetch(filter, timeout).await?;
    Ok(ingest_teasers(events, tags, content_types, cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ShareCode deliberately has no Debug (it holds the secret browse-key), so unwrap its parse
    // through a match rather than `.unwrap()`.
    fn ok(s: &str) -> ShareCode {
        match parse_share_code(s) {
            Ok(p) => p,
            Err(e) => panic!("expected a valid share code, got {e}"),
        }
    }

    #[test]
    fn full_sharecode_yields_browse_key() {
        let pk = Identity::generate().public_key();
        let bk: [u8; 32] = [7; 32];
        let code = ShareCode::Full { pubkey: pk, browse_key: bk }.encode().unwrap();
        let parsed = ok(&code);
        assert_eq!(parsed.browse_key(), Some(bk), "a full code unlocks the browse-key");
        assert_eq!(parsed.pubkey(), pk);
    }

    #[test]
    fn bare_npub_is_follow_only_no_browse() {
        let id = Identity::generate();
        let parsed = ok(&id.npub());
        assert_eq!(parsed.browse_key(), None, "a bare npub is follow-only");
        assert_eq!(parsed.pubkey(), id.public_key());
    }

    #[test]
    fn malformed_sharecode_rejected_with_reason() {
        // Non-bech32, truncated, and garbage all return a clean Err (never a panic) — the
        // orchestration surfaces hb-core's codec rejection.
        for s in ["not-a-code", "npub1", "hbk1zzzz", "", "::::"] {
            match parse_share_code(s) {
                Err(e) => assert!(!e.to_string().is_empty(), "{s:?} must reject with a reason"),
                Ok(_) => panic!("{s:?} should not parse as a valid share code"),
            }
        }
    }

    // ── M16 W2: the snapshot-fingerprint staleness gate (pure) ──────────────────────────────────

    const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    /// A listing big enough to split under a 40 KiB budget, carrying a top-level
    /// `snapshot_fingerprint` (what the hoarder writes at publish time, W3).
    fn big_listing_with_fp(slug: &str, n: usize, fp: &str) -> String {
        let entries: Vec<serde_json::Value> = (0..n)
            .map(|i| serde_json::json!({ "name": format!("title-{i:05}-padding-padding-padding-xx") }))
            .collect();
        serde_json::json!({
            "slug": slug, "content_types": ["video"],
            "snapshot_fingerprint": fp, "entries": entries,
        })
        .to_string()
    }

    #[test]
    fn snapshot_fingerprint_rides_through_the_big_relay_split() {
        // The W2 gate's premise: a full listing carrying `snapshot_fingerprint` survives the split
        // (the big-relay carrier reuses the 40 KiB budget) + restitch, so the fingerprint is readable
        // from the rendered family's meta and the gate can compare it.
        let json = big_listing_with_fp("vault", 1300, FP);
        let parts = split_listing("vault", &json, 40_000).unwrap();
        assert!(parts.len() > 2, "the listing must actually split, got {} part(s)", parts.len());
        let payloads: Vec<String> = parts.iter().map(|p| p.json.clone()).collect();
        let rendered = render_listing(&payloads).unwrap();
        assert!(rendered.complete(), "all parts present → complete tree");
        assert_eq!(rendered.entries.len(), 1300);
        assert_eq!(
            listing_snapshot_fingerprint(&rendered),
            Some(FP),
            "the fingerprint must survive the split into meta"
        );
        assert!(full_supersedes(&rendered, FP), "a matching fingerprint supersedes the teaser");
        assert!(!full_supersedes(&rendered, "deadbeef"), "a mismatched fingerprint does not supersede");
    }

    #[test]
    fn incomplete_family_never_supersedes_even_with_matching_fingerprint() {
        // Codex review (W2): a big relay can serve the signed index but WITHHOLD a content part. The
        // rendered tree is partial yet still carries the matching `snapshot_fingerprint` in its meta —
        // it must NOT replace the paywall (that would present a partial tree as the full list).
        let json = big_listing_with_fp("vault", 1300, FP);
        let parts = split_listing("vault", &json, 40_000).unwrap();
        assert!(parts.len() > 2, "the listing must split, got {} part(s)", parts.len());
        let mut payloads: Vec<String> = parts.iter().map(|p| p.json.clone()).collect();
        payloads.pop(); // withhold the last content part → an incomplete family
        let rendered = render_listing(&payloads).unwrap();
        assert!(!rendered.complete(), "a withheld part must render incomplete");
        assert_eq!(
            listing_snapshot_fingerprint(&rendered),
            Some(FP),
            "the fingerprint still rides in meta even when parts are missing"
        );
        assert!(
            !full_supersedes(&rendered, FP),
            "an incomplete family must keep the teaser despite the matching fingerprint"
        );
    }

    #[test]
    fn truncated_teaser_and_full_family_share_one_fingerprint() {
        // The crux of the gate: the paywall teaser (a single truncated event) and the big-relay full
        // family both derive from the same source tree, so they carry the *same*
        // `snapshot_fingerprint` — that is what lets the browse side confirm the family is the full
        // version of exactly what the teaser previews. `truncate_listing` preserves top-level meta.
        let json = big_listing_with_fp("vault", 2000, FP);
        let t = truncate_listing(&json, 40_000).unwrap();
        assert!(t.truncated, "this listing must truncate");
        let teaser = render_listing(&[t.json]).unwrap();
        assert_eq!(
            listing_snapshot_fingerprint(&teaser),
            Some(FP),
            "the truncated teaser keeps the full-tree fingerprint the family also carries"
        );
    }

    #[test]
    fn unfingerprinted_listing_never_supersedes() {
        // A pre-M16 listing (no `snapshot_fingerprint`) must not be trusted as "current" — the gate
        // keeps the teaser rather than serving an un-gated full tree.
        let json = serde_json::json!({ "slug": "old", "entries": [{ "name": "a" }] }).to_string();
        let rendered = render_listing(&[json]).unwrap();
        assert_eq!(listing_snapshot_fingerprint(&rendered), None);
        assert!(!full_supersedes(&rendered, "anything"));
    }
}
