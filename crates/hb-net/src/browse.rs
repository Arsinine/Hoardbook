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
use crate::discover::{ingest_teasers_capped, select_newest_by_created_at, SearchHit};
use crate::error::NetError;
use crate::nip65::{bootstrap_order, inbox_order, parse_relay_list};
use crate::render::{render_listing, RenderedListing, MAX_LISTING_PARTS};
use crate::split::{split_listing, truncate_listing, MAX_RESTITCHED_BYTES};

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
    /// Why `listing` is `None` when a browse-key was held (QURATOR-127): the inner error
    /// `fetch_listing` returned, captured before BR1 leniency locks the listing — the old `.ok()`
    /// discarded it, collapsing a client/timeout failure, a missing index, an incoherent family
    /// and a decrypt failure into one indistinguishable `None`. `None` when the listing rendered
    /// or the code was follow-only (no key held). Diagnostic only, never a failure in itself.
    pub listing_error: Option<String>,
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
    let teaser = select_newest_teaser(teaser_events, &peer);

    // Listing (gated by the browse-key): a decrypt failure locks the listing without failing the
    // whole browse — the teaser still shows (BR1). The reason rides on the result so the failure
    // mode stays attributable (QURATOR-127).
    let (listing, listing_error) = match share_code.browse_key() {
        Some(bk) => {
            listing_or_lock_reason(slug, fetch_listing(client, &peer, slug, &bk, timeout).await)
        }
        None => (None, None),
    };

    Ok(BrowseResult { teaser, listing, listing_error, resolved_relays })
}

/// The BR1 fold, as ONE function both callers use (QURATOR-127): a listing failure locks the
/// listing (`None`) without failing the browse, but the reason is kept — rendered into a
/// `tracing::warn!` the harness's subscriber emits, and returned alongside so `hb-it` can name the
/// actual failure mode in its TAP line instead of the bare "did not browse". A success carries no
/// reason. `fetch_full_listing_from` (the big-relay twin) keeps its own silent `.ok()`-shaped
/// leniency — its caller has no diagnostic channel and BR1 there is pinned by its own tests.
pub fn listing_or_lock_reason(
    slug: &str,
    r: Result<RenderedListing, NetError>,
) -> (Option<RenderedListing>, Option<String>) {
    match r {
        Ok(listing) => (Some(listing), None),
        Err(e) => {
            let reason = e.to_string();
            tracing::warn!("browse: listing for '{slug}' locked (BR1) — {reason}");
            (None, Some(reason))
        }
    }
}

/// Phase-1 fetch budget for a slug's **index** event (`d = slug`): one parameterized-replaceable
/// address, so anything past a handful of stale duplicates is relay misbehaviour. This limit is a
/// **hint, not a bound** — nostr-sdk's `fetch_events` collects with `force_insert`, which grows
/// past a filter's capacity rather than enforcing `limit`, so a hostile relay can still return
/// more (Codex review 2026-08-24, finding 1). Correctness never depends on the hint: whatever
/// arrives is re-pinned by [`authored_by`] and collapsed by [`select_newest_by_created_at`].
const LISTING_INDEX_FETCH_LIMIT: usize = 8;

/// The phase-1 filter of a slug-scoped family fetch: the **index event only** (`#d = [slug]`),
/// author- and kind-pinned, with a small budget. Slug-scoping the index read is what removes the
/// author-wide eviction regression (Codex review 2026-08-24, finding 2): the old author-wide REQ
/// with a relay-side limit meant a legitimate peer publishing more than ten max-size families
/// could have an old collection's events silently omitted by a relay honoring the limit — this
/// REQ can only ever match ONE family's index, so no other collection's events are at stake.
/// Authorship is still re-pinned client-side by [`authored_by`] after the fetch: nostr-sdk does
/// not verify `author()` filters, and a relay can return a validly-signed foreign listing event.
fn slug_index_filter(peer: PublicKey, slug: &str) -> Filter {
    Filter::new()
        .author(peer)
        .kind(Kind::from_u16(KIND_LISTING))
        .identifier(slug)
        .limit(LISTING_INDEX_FETCH_LIMIT)
}

/// The declared content-part count of a **decrypted index payload**: `part_count` (v2) or a
/// numeric `parts` (v1); `None` for a plain unsplit listing (no phase 2 needed) or an unparseable
/// payload (left for [`render_slug_family`] to reject, exactly as the one-phase path did). This is
/// a *hint read*, not a parallel validator — the real validation of the declared count lives where
/// it already lived (`render.rs` / `split.rs`); the only cap applied here is the SAME
/// [`MAX_LISTING_PARTS`] refusal in [`slug_parts_filter`], used to bound the phase-2 d-tag list
/// before it is allocated.
fn index_declared_part_count(index_json: &str) -> Option<usize> {
    let v: serde_json::Value = serde_json::from_str(index_json).ok()?;
    let obj = v.as_object()?;
    if obj.get("split") != Some(&serde_json::Value::Bool(true)) {
        return None; // plain unsplit listing — the index IS the whole family
    }
    if let Some(n) = obj.get("part_count").and_then(serde_json::Value::as_u64) {
        return Some(n as usize);
    }
    obj.get("parts").and_then(serde_json::Value::as_u64).map(|n| n as usize)
}

/// The phase-2 filter: fetch **exactly the content-part `d`-tags the index named**
/// (`slug#part0 … slug#partN-1` — the same derivation `split_listing` publishes, pinned by
/// `phase_two_d_tags_match_what_split_listing_publishes`), author- and kind-pinned, with a
/// `.limit()` derived from that declared count — the budget is bounded by the family's OWN
/// declared size, so it can neither evict a compliant family nor license a hoard.
///
/// * `Ok(None)` — an unsplit listing (declared count 0) needs no second fetch.
/// * `Err` — the index declares more parts than [`MAX_LISTING_PARTS`]. **Refused, never clamped**
///   (a clamped count would silently fetch a prefix and render a partial tree a hostile index
///   could pass off as complete). `render_slug_family` would reject the same index later with the
///   same cap; this fails fast, before the d-tag list is allocated.
fn slug_parts_filter(
    peer: PublicKey,
    slug: &str,
    declared: usize,
) -> Result<Option<Filter>, NetError> {
    if declared == 0 {
        return Ok(None);
    }
    if declared > MAX_LISTING_PARTS {
        return Err(NetError::Split(format!(
            "index claims {declared} parts, exceeds the {MAX_LISTING_PARTS}-part cap"
        )));
    }
    let d_tags = (0..declared).map(|i| format!("{slug}#part{i}"));
    Ok(Some(
        Filter::new()
            .author(peer)
            .kind(Kind::from_u16(KIND_LISTING))
            .identifiers(d_tags)
            .limit(declared),
    ))
}

/// The two-phase, slug-scoped family fetch shared by [`fetch_listing`] and
/// [`fetch_full_listing_from`] so the pool-wide and big-relay reads cannot drift apart (one site
/// getting the fix and its twin not is this repo's standing fault shape). `fetch` abstracts
/// pool-wide vs targeted relay set. Phase 1 reads ONLY the index (`#d = [slug]`); its declared
/// part count then drives phase 2, which reads exactly the named part d-tags — so a relay-side
/// limit is bounded by the family's own declared size and can never evict a DIFFERENT
/// collection's events. Leniency is unchanged from the one-phase path: a family missing parts
/// still renders partial (phase 2 simply returns fewer), a missing index is still the same
/// "no listing found for slug" error, and a compliant listing produces exactly what it did before.
/// Stale orphan parts beyond the declared count are no longer fetched at all — they used to
/// hard-error v1 render as "foreign part"; under parameterized-replaceable semantics (N3) the
/// newest index governs, so ignoring them is the more faithful reading.
async fn fetch_slug_family<F, Fut>(
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
    fetch: F,
) -> Result<RenderedListing, NetError>
where
    F: Fn(Filter) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Event>, NetError>>,
{
    // Phase 1: the index event only. Re-pin authorship client-side (nostr-sdk does not verify
    // `author()` filters) BEFORE newest-wins selection, so a validly-signed foreign `d = slug`
    // event can't decide the part count.
    let index_events = authored_by(fetch(slug_index_filter(*peer, slug)).await?, peer);
    let index = match select_newest_by_created_at(index_events) {
        Some(ev) => ev,
        // Missing index — the same error the one-phase path raised for an empty family.
        None => return Err(NetError::Split(format!("no listing found for slug '{slug}'"))),
    };
    let (_, index_json) = parse_listing_event(&index, browse_key)?;
    let declared = index_declared_part_count(&index_json).unwrap_or(0);

    // Phase 2 (only when the index declares content parts): exactly the named d-tags.
    let mut events = match slug_parts_filter(*peer, slug, declared)? {
        Some(filter) => fetch(filter).await?,
        None => Vec::new(),
    };
    events.push(index);
    render_slug_family(events, peer, slug, browse_key, MAX_RESTITCHED_BYTES)
}

/// Fetch a slug's listing family (index + content parts), pick the newest event per `d`-tag (so a
/// non-compliant relay's stale replaceable duplicate can't win — N3/AB8), decrypt each with the
/// browse-key (which re-verifies the Schnorr signature), and render into a possibly-partial tree.
/// Slug-scoped and two-phase — see [`fetch_slug_family`].
async fn fetch_listing(
    client: &RelayClient,
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
    timeout: Duration,
) -> Result<RenderedListing, NetError> {
    fetch_slug_family(peer, slug, browse_key, |filter| client.fetch(filter, timeout)).await
}

/// Drop any event not authored by `peer` (CWE-346 / Codex review, M16 W2 + M19 W3). The relay-side
/// `author(peer)` filter is **not** enough — nostr-sdk does not verify that fetched events match the
/// requested `author()` filter, so a lying relay can return a *validly-self-signed* listing event from
/// a different key (a share-code holder can encrypt a family under the shared browse-key and sign it
/// themselves). Without this pin such a foreign event can win the newest-per-`d` selection and splice
/// spoofed content into the rendered tree, or hide a real collection. Used by both
/// [`render_slug_family`] and [`browse_peer_listings`] so the two can't drift apart again.
fn authored_by(events: Vec<Event>, peer: &PublicKey) -> Vec<Event> {
    events.into_iter().filter(|e| &e.pubkey == peer).collect()
}

/// Pick the browsed peer's newest teaser, pinning authorship (CWE-346) **before** the
/// newest-by-`created_at` selection — a lying relay could otherwise substitute a validly-self-signed
/// *foreign* teaser as the peer's identity, spoofing their name/bio/tags. Reuses [`authored_by`]
/// exactly as the listing paths do, so the teaser and listing paths can't drift apart.
fn select_newest_teaser(events: Vec<Event>, peer: &PublicKey) -> Option<Teaser> {
    select_newest_by_created_at(authored_by(events, peer)).and_then(|e| parse_teaser(&e).ok())
}

/// The accrue-or-refuse decision behind [`render_slug_family`]'s decrypted-byte bound (Residual A,
/// QURATOR-114's byte dimension). Extracted as a **pure** function precisely so the byte bound is
/// testable at its real 64 MiB numbers — reaching the cap on the wire would take >1000 real NIP-44
/// encryptions, which is why the bound shipped unpinned in 50d7bea. Production passes
/// [`MAX_RESTITCHED_BYTES`]; over-cap input is **refused**, never clamped.
fn accrue_decrypted_bytes(
    running: &mut usize,
    next_len: usize,
    cap: usize,
) -> Result<(), NetError> {
    *running += next_len;
    if *running > cap {
        return Err(NetError::Split(format!(
            "decrypted listing family exceeds the {cap}-byte cap"
        )));
    }
    Ok(())
}

/// Group fetched `KIND_LISTING` events into one slug's family (the `d=slug` index/single + its
/// `d=slug#partN` content parts), take the **newest event per `d`** (a non-compliant relay's stale
/// replaceable duplicate can't win — N3/AB8), decrypt each with the browse-key (which re-verifies the
/// Schnorr signature), and render into a possibly-partial tree. Shared by the pool-wide
/// [`fetch_listing`] and the big-relay-targeted [`fetch_full_listing_from`] (M16 W2) so both read the
/// exact same family-assembly logic.
///
/// `family_byte_cap` is the decrypted-byte bound (Residual A, QURATOR-114's byte dimension): the
/// running total of decrypted plaintext across the family is refused — not clamped — the moment it
/// exceeds the cap, before `render_listing`'s matched-bytes cap is consulted. Production callers
/// pass [`MAX_RESTITCHED_BYTES`]; the parameter exists so the bound is drivable at test scale
/// (reaching the real 64 MiB cap would take >1000 real NIP-44 encryptions, which is why the bound
/// shipped unpinned in 50d7bea).
fn render_slug_family(
    events: Vec<Event>,
    peer: &PublicKey,
    slug: &str,
    browse_key: &BrowseKey,
    family_byte_cap: usize,
) -> Result<RenderedListing, NetError> {
    let part_prefix = format!("{slug}#part");
    let mut by_d: HashMap<String, Vec<Event>> = HashMap::new();
    for ev in authored_by(events, peer) {
        if let Some(d) = ev.tags.identifier() {
            if d == slug || d.starts_with(&part_prefix) {
                by_d.entry(d.to_string()).or_default().push(ev);
            }
        }
    }
    if by_d.is_empty() {
        return Err(NetError::Split(format!("no listing found for slug '{slug}'")));
    }
    // Bound the family before decrypting it. A hostile peer can publish thousands of distinct part
    // d-tags under one slug; the compliant ceiling is one index (`d = slug`) plus at most
    // MAX_LISTING_PARTS content parts (`d = slug#partN`). Refusing the count up front (and the
    // decrypted byte total as it accrues below) stops the whole batch being decrypted and
    // materialised here first — which would defeat `render_listing`'s own cap, since that cap
    // would fire only *after* the memory had already been taken.
    if by_d.len() > MAX_LISTING_PARTS + 1 {
        return Err(NetError::Split(format!(
            "listing family ships {} distinct d-tags, exceeds the {MAX_LISTING_PARTS}-part cap",
            by_d.len()
        )));
    }

    let mut payloads: Vec<String> = Vec::new();
    let mut decrypted_bytes: usize = 0;
    for (_d, group) in by_d {
        if let Some(ev) = select_newest_by_created_at(group) {
            let (_slug, json) = parse_listing_event(&ev, browse_key)?;
            // Total decrypted bytes across the family (index + parts), bounded as it accrues: a
            // family of near-max parts could otherwise materialise hundreds of MB of plaintext
            // before `render_listing`'s matched-bytes cap is consulted.
            accrue_decrypted_bytes(&mut decrypted_bytes, json.len(), family_byte_cap)?;
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
    fetch_slug_family(peer, slug, browse_key, |filter| {
        client.fetch_from(relays, filter, timeout)
    })
    .await
}

/// The full-tree **snapshot fingerprint** a rendered listing carries in its metadata (M16). The
/// hoarder writes it into the listing JSON at publish time (W3); it rides through split/restitch into
/// `RenderedListing.meta` for free (top-level metadata is preserved). `None` for a pre-M16 listing
/// that predates the field.
pub fn listing_snapshot_fingerprint(rendered: &RenderedListing) -> Option<&str> {
    rendered.meta.get("snapshot_fingerprint").and_then(serde_json::Value::as_str)
}

/// The **teaser digest** a full carrier (big-relay family index, `.hbmanifest` plaintext) carries
/// (audit #25 / QURATOR-123): the digest the truncated teaser of the SAME publish carries — over
/// the teaser's visible entries + elided count (`hb_core::teaser_fingerprint`), stamped by the
/// publish path beside the carrier's own full-tree `snapshot_fingerprint`. This is the only value a
/// teaser and a full carrier can still share: the teaser may no longer carry a digest of the content
/// truncation hides from it. `None` for a carrier that predates the field.
pub fn family_teaser_fingerprint(rendered: &RenderedListing) -> Option<&str> {
    rendered.meta.get("teaser_fingerprint").and_then(serde_json::Value::as_str)
}

/// The M16 staleness gate: a fetched full family supersedes the paywall teaser **only** if it is
/// **complete** AND its teaser digest (`teaser_fingerprint`) matches the teaser's. Completeness is load-bearing
/// (Codex review, W2): a big relay can serve the signed index but withhold content parts —
/// `render_listing` yields a *partial* tree that still carries the matching fingerprint in its meta,
/// and silently replacing the paywall with a partial tree presented as the full list would be a
/// downgrade. An incomplete, un-digested (pre-M16), or mismatched (stale) family does not
/// supersede — keep the teaser and surface "ask again".
fn full_supersedes(rendered: &RenderedListing, expected_fingerprint: &str) -> bool {
    rendered.complete() && family_teaser_fingerprint(rendered) == Some(expected_fingerprint)
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
///
/// **No relay-side `.limit()` on this fetch** (Codex review 2026-08-24, finding 2): enumeration is
/// author-wide by nature — every family the peer published is wanted — so a REQ limit here can
/// only ever *evict* a legitimate peer's older collections (a relay honoring the limit returns its
/// newest matching events, silently omitting the rest), which is a behaviour regression, not
/// hardening. Nor would a limit bound a hostile relay: nostr-sdk's `fetch_events` collects with
/// `force_insert`, which grows past a filter's capacity rather than enforcing `limit` — so the
/// limit is a hint the relay may ignore AND the client does not enforce. Instead the bounds are
/// CLIENT-SIDE, mirroring the ones its sibling [`render_slug_family`] already had and this path
/// lacked (two reviewers independently confirmed the gap): a per-family distinct-`d` count
/// refusal at the same `MAX_LISTING_PARTS + 1` ceiling, and a per-family decrypted-byte accrual
/// bound via [`accrue_decrypted_bytes`] at [`MAX_RESTITCHED_BYTES`] — a bound you enforce
/// yourself is real; a REQ limit is a hint.
pub async fn browse_peer_listings(
    client: &RelayClient,
    peer: &PublicKey,
    browse_key: &BrowseKey,
    timeout: Duration,
) -> Result<Vec<(String, RenderedListing, Option<String>)>, NetError> {
    let events = client
        .fetch(Filter::new().author(*peer).kind(Kind::from_u16(KIND_LISTING)), timeout)
        .await?;

    // Group by root slug, keeping every event per full `d` so the newest per replaceable
    // identifier wins below (BTreeMap ⇒ output sorted by root slug).
    let mut families: BTreeMap<String, HashMap<String, Vec<Event>>> = BTreeMap::new();
    for ev in authored_by(events, peer) {
        if let Some(d) = ev.tags.identifier() {
            let root = match d.find("#part") {
                Some(i) => &d[..i],
                None => d,
            };
            families.entry(root.to_string()).or_default().entry(d.to_string()).or_default().push(ev);
        }
    }

    let mut out = Vec::new();
    for (root, by_d) in families {
        if let Some((rendered, teaser_event_id)) =
            render_browsed_family(&root, by_d, browse_key, MAX_LISTING_PARTS, MAX_RESTITCHED_BYTES)
        {
            out.push((root, rendered, teaser_event_id));
        }
    }
    Ok(out)
}

/// Assemble + render ONE browsed family for [`browse_peer_listings`] — the enumeration path's
/// counterpart of [`render_slug_family`], with the same two client-side bounds that sibling has:
///
/// * a **distinct-`d` count bound**: `by_d.len() > family_part_cap + 1` refuses the whole family
///   (one index + at most `family_part_cap` content parts is the compliant ceiling). This is the
///   sibling gap two reviewers independently found: a hostile peer publishing arbitrarily many
///   `slug#partN` d-tags recreated audit #7's aggregate-allocation shape here — every newest-per-`d`
///   payload was decrypted into `payloads` with no count check at all.
/// * a **decrypted-byte accrual bound** via [`accrue_decrypted_bytes`]: the family's decrypted
///   total is refused — not clamped — the moment it exceeds `byte_cap`, before `render_listing`
///   materialises anything from it.
///
/// Both caps are parameters so tests can drive them at real numbers (production passes
/// [`MAX_LISTING_PARTS`] / [`MAX_RESTITCHED_BYTES`]) — a bound you enforce yourself is real; a
/// REQ `limit` is a hint (nostr-sdk's `force_insert` grows past it, so the client never enforces
/// it either — Codex review 2026-08-24, finding 1). Returns `None` for any family that must be
/// **skipped** (locked, corrupt, over-either-cap, or failing to render) — locked ≠ error, BR1,
/// exactly the `Err(_) => continue 'family` behaviour this path always had, now bounded.
fn render_browsed_family(
    root: &str,
    by_d: HashMap<String, Vec<Event>>,
    browse_key: &BrowseKey,
    family_part_cap: usize,
    byte_cap: usize,
) -> Option<(RenderedListing, Option<String>)> {
    // Count bound (mirror of `render_slug_family`'s refusal): refused — never truncated
    // (truncating a family would render a partial tree as if it were the collection).
    // Count bound (mirror of `render_slug_family`'s refusal): refused — never truncated
    // (truncating a family would render a partial tree as if it were the collection).
    // Count bound (mirror of `render_slug_family`'s refusal): refused — never truncated
    // (truncating a family would render a partial tree as if it were the collection).
    if by_d.len() > family_part_cap + 1 {
        tracing::warn!(
            "peer family '{root}' ships {} distinct d-tags, exceeds the \
             {family_part_cap}-part cap; skipping",
            by_d.len()
        );
        return None;
    }
    let mut payloads: Vec<String> = Vec::new();
    let mut teaser_event_id: Option<String> = None;
    let mut decrypted_bytes: usize = 0;
    for (d, group) in by_d {
        if let Some(ev) = select_newest_by_created_at(group) {
            // The index/single event (`d == root`) IS the teaser the browser renders; capture its
            // id so a manifest request can name the exact teaser event (M16 W4).
            if d == root {
                teaser_event_id = Some(ev.id.to_hex());
            }
            match parse_listing_event(&ev, browse_key) {
                Ok((_slug, json)) => {
                    // Byte bound: over-cap ⇒ skip the family, like a decrypt failure below.
                    // Byte bound: over-cap ⇒ skip the family, like a decrypt failure below.
                    if accrue_decrypted_bytes(&mut decrypted_bytes, json.len(), byte_cap).is_err() {
                        return None;
                    }
                    payloads.push(json);
                }
                // Wrong browse-key (locked) or malformed event → skip the whole family.
                Err(_) => return None,
            }
        }
    }
    let rendered = render_listing(&payloads).ok()?;
    Some((rendered, teaser_event_id))
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
    Ok(search_teasers_capped(client, tags, content_types, cap, timeout).await?.0)
}

/// Same as [`search_teasers`] but also returns whether the cap truncated the ranked set (M20 W3 —
/// the Discover UI surfaces a "showing first N" affordance when `capped` is `true`). The truncation
/// signal is authoritative: it comes from [`ingest_teasers_capped`], the only layer that sees both
/// the full deduped set and the cap.
pub async fn search_teasers_capped(
    client: &RelayClient,
    tags: &[String],
    content_types: &[String],
    cap: usize,
    timeout: Duration,
) -> Result<(Vec<SearchHit>, bool), NetError> {
    let filter = teaser_search_filter(tags, content_types)?;
    let events = client.fetch(filter, timeout).await?;
    Ok(ingest_teasers_capped(events, tags, content_types, cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hb_core::event::build_teaser;

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

    // ── 2026-08-24 redesign: the slug-scoped two-phase family fetch (per-slug fetch, index
    //    first, then exactly the declared part d-tags) and the client-side bounds
    //    `browse_peer_listings` now carries. Replaces the LISTING_FETCH_LIMIT tests — a relay-side
    //    limit on an author-wide fetch was decorative (nostr-sdk `force_insert` grows past it) AND
    //    a regression (it evicted a prolific peer's older collections).

    /// The d-tag list the phase-2 filter derives must be EXACTLY what `split_listing` publishes
    /// (`slug#part0 … slug#partN-1`) — the wire contract that makes the fetch slug-scoped. If
    /// either side drifts (a different separator, 1- vs 0-based), phase 2 would fetch d-tags that
    /// exist on no relay and every split family would render partial.
    #[test]
    fn phase_two_d_tags_match_what_split_listing_publishes() {
        let peer = Identity::generate().public_key();
        // A real split under a small budget, so the part count comes from the production packer.
        let json = serde_json::json!({
            "slug": "vault",
            "entries": (0..80).map(|i| serde_json::json!({ "name": format!("f{i}-pad-pad-pad-pad") }))
                .collect::<Vec<_>>(),
        })
        .to_string();
        let parts = split_listing("vault", &json, 2_000).unwrap();
        assert!(parts.len() > 2, "the listing must actually split");
        let n = parts.len() - 1; // split_listing ships index + n content parts

        let filter = slug_parts_filter(peer, "vault", n)
            .expect("a compliant declared count must yield a filter")
            .expect("a split family must need a phase-2 fetch");
        let fetched: Vec<String> = filter
            .generic_tags
            .get(&SingleLetterTag::lowercase(Alphabet::D))
            .expect("the #d tag values must be set")
            .iter()
            .cloned()
            .collect();
        let published: Vec<String> =
            parts.iter().skip(1).map(|p| p.d_tag.clone()).collect(); // skip the index
        assert_eq!(
            fetched, published,
            "phase 2 must fetch exactly the d-tags split_listing published"
        );
    }

    /// The phase-2 budget is bounded by the family's OWN declared size — never a multiple of it,
    /// never independent of it — so it can neither evict a compliant family nor license a hoard.
    #[test]
    fn phase_two_limit_is_derived_from_the_declared_part_count() {
        let peer = Identity::generate().public_key();
        for declared in [1usize, 7, 64] {
            let filter = slug_parts_filter(peer, "x", declared)
                .expect("compliant count")
                .expect("split family");
            assert_eq!(
                filter.limit,
                Some(declared),
                "the phase-2 budget must equal the declared count ({declared})"
            );
        }
        // An unsplit listing (declared 0) needs no second fetch at all.
        assert!(
            slug_parts_filter(peer, "x", 0).expect("declared 0").is_none(),
            "an unsplit listing must not issue a phase-2 fetch"
        );
    }

    /// An index declaring more than MAX_LISTING_PARTS parts is REFUSED, never clamped — a clamped
    /// count would fetch a prefix and render a partial tree a hostile index could pass off as
    /// complete ("never clamp a value an identifier is derived from": every d-tag IS an
    /// identifier). This is the same refusal the render layer applies, fired early, before the
    /// d-tag list is even allocated.
    #[test]
    fn phase_two_refuses_over_cap_declared_counts_without_clamping() {
        let peer = Identity::generate().public_key();
        let declared = MAX_LISTING_PARTS + 1;
        match slug_parts_filter(peer, "x", declared) {
            Err(NetError::Split(m)) => assert!(
                m.contains("exceeds the") && m.contains(&format!("{MAX_LISTING_PARTS}-part cap")),
                "expected the parts-cap refusal, got: {m}"
            ),
            other => panic!("an over-cap declared count must refuse, got {other:?}"),
        }
    }

    /// The phase-1 (index) filter must be slug-scoped (`#d = [slug]`), author-pinned, and
    /// kind-pinned — the slug-scoping is what removed the author-wide eviction regression. If
    /// this filter ever regresses to author-wide, the REQ once again races a relay-side limit
    /// against every other collection the peer published (Codex review 2026-08-24, finding 2).
    #[test]
    fn slug_index_filter_is_slug_scoped_author_and_kind_pinned() {
        let peer = Identity::generate().public_key();
        let f = slug_index_filter(peer, "vault");
        let d_values = f
            .generic_tags
            .get(&SingleLetterTag::lowercase(Alphabet::D))
            .expect("the index filter must carry #d = [slug]");
        assert_eq!(d_values.len(), 1, "exactly one identifier: the slug");
        assert!(d_values.contains("vault"), "#d must be the slug itself, not a part tag");
        assert!(!d_values.contains("vault#part0"), "the index filter must NOT name part tags");
        assert_eq!(f.authors.as_ref().map(|a| a.len()), Some(1), "author-scoped");
        assert!(f.authors.expect("authors pinned above").contains(&peer));
        assert_eq!(f.kinds.as_ref().map(|k| k.len()), Some(1), "one kind");
        assert!(
            f.kinds.expect("kinds pinned above").contains(&Kind::from_u16(KIND_LISTING)),
            "the listing kind"
        );
    }

    /// The two-phase fetch, end to end at the pure seam (`fetch_slug_family` takes its fetch as a
    /// closure, so the filter routing itself is drivable without a relay): a real split family —
    /// index + content parts, each fetchable by exactly one phase — renders COMPLETE, and the
    /// synthetic relay hands the two phases exactly what their filters ask for. This is the
    /// "do not change what a compliant listing produces" pin.
    #[tokio::test]
    async fn two_phase_fetch_renders_a_compliant_split_family_complete() {
        let victim = Identity::generate();
        let peer = victim.public_key();
        let browse_key: BrowseKey = [7; 32];

        // A real v1 split family: index (`d = tpf`) + two content parts.
        let index_json =
            serde_json::json!({ "slug": "tpf", "split": true, "parts": 2 }).to_string();
        let p0 = serde_json::json!({
            "slug": "tpf", "part": 0, "parts": 2, "entries": [{ "name": "a" }],
        })
        .to_string();
        let p1 = serde_json::json!({
            "slug": "tpf", "part": 1, "parts": 2, "entries": [{ "name": "b" }],
        })
        .to_string();
        let index = build_listing_event(&victim, "tpf", &browse_key, &index_json).unwrap();
        let part0 = build_listing_event(&victim, "tpf#part0", &browse_key, &p0).unwrap();
        let part1 = build_listing_event(&victim, "tpf#part1", &browse_key, &p1).unwrap();

        // A lying relay would ALSO return a foreign-author index and a stale part duplicate; both
        // must be handled (authored_by + newest-per-d) exactly as the one-phase path did.
        let attacker = Identity::generate();
        let foreign = build_listing_event(
            &attacker,
            "tpf",
            &browse_key,
            &serde_json::json!({ "slug": "tpf", "split": true, "parts": 9 }).to_string(),
        )
        .unwrap();
        let stale = {
            let older = EventBuilder::new(
                Kind::from_u16(KIND_LISTING),
                part0.content.clone(),
            )
            .tags(part0.tags.clone())
            .custom_created_at(part0.created_at - 1000);
            victim.sign(older).unwrap()
        };

        let phase_two_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fetch = |filter: Filter| {
            let seen = phase_two_seen.clone();
            // Clone BEFORE the async block so the closure captures by value (Fn, not FnOnce —
            // `fetch_slug_family` may call it twice).
            let (index, foreign, part0, part1, stale) = (
                index.clone(),
                foreign.clone(),
                part0.clone(),
                part1.clone(),
                stale.clone(),
            );
            async move {
                // Synthetic relay: serve exactly what the filter asks for — the compliant set,
                // plus the adversarial extras a lying relay adds regardless of the REQ.
                let d_wanted = filter
                    .generic_tags
                    .get(&SingleLetterTag::lowercase(Alphabet::D))
                    .cloned()
                    .unwrap_or_default();
                let is_phase_one = d_wanted.contains("tpf") && d_wanted.len() == 1;
                seen.store(!is_phase_one, std::sync::atomic::Ordering::SeqCst);
                let mut served: Vec<Event> = if is_phase_one {
                    vec![index, foreign.clone()]
                } else {
                    vec![part0, part1, stale]
                };
                // A lying relay ignores the REQ limit (force_insert) — pile on junk parts.
                served.push(foreign);
                Ok(served)
            }
        };

        let rendered = fetch_slug_family(&peer, "tpf", &browse_key, fetch)
            .await
            .expect("a compliant split family must render");
        assert!(
            phase_two_seen.load(std::sync::atomic::Ordering::SeqCst),
            "the declared part count must have driven a phase-2 fetch"
        );
        assert!(rendered.complete(), "index + both declared parts → complete tree");
        let s = serde_json::to_string(&rendered.entries).unwrap_or_default();
        assert!(s.contains("\"a\"") && s.contains("\"b\""), "both parts' entries: {s}");
    }

    /// The two-phase fetch's leniency pins: a family with a MISSING declared part still renders
    /// PARTIAL (never an error), and a missing index is still the same "no listing found" error.
    #[tokio::test]
    async fn two_phase_fetch_keeps_partial_and_missing_index_semantics() {
        let victim = Identity::generate();
        let peer = victim.public_key();
        let browse_key: BrowseKey = [7; 32];
        let index_json =
            serde_json::json!({ "slug": "pp", "split": true, "parts": 2 }).to_string();
        let p0 = serde_json::json!({
            "slug": "pp", "part": 0, "parts": 2, "entries": [{ "name": "a" }],
        })
        .to_string();
        let index = build_listing_event(&victim, "pp", &browse_key, &index_json).unwrap();
        let part0 = build_listing_event(&victim, "pp#part0", &browse_key, &p0).unwrap();

        // Phase 2 returns only part0 — part1 is lost. Partial, not error.
        let fetch_missing = |filter: Filter| {
            let (index, part0) = (index.clone(), part0.clone());
            async move {
                let wanted = filter
                    .generic_tags
                    .get(&SingleLetterTag::lowercase(Alphabet::D))
                    .cloned()
                    .unwrap_or_default();
                if wanted.contains("pp") && wanted.len() == 1 {
                    Ok(vec![index])
                } else {
                    Ok(vec![part0]) // part1 withheld by the relay
                }
            }
        };
        let rendered = fetch_slug_family(&peer, "pp", &browse_key, fetch_missing)
            .await
            .expect("a partial family must still render, not error");
        assert!(!rendered.complete(), "a withheld part must render incomplete");
        assert_eq!(rendered.parts_present, 1, "one of two parts present");

        // No index at all → the same error the one-phase path raised.
        let fetch_none = |_filter: Filter| async { Ok(Vec::<Event>::new()) };
        match fetch_slug_family(&peer, "pp", &browse_key, fetch_none).await {
            Err(NetError::Split(m)) => assert!(
                m.contains("no listing found for slug 'pp'"),
                "expected the missing-index error, got: {m}"
            ),
            other => panic!("a missing index must be the existing error, got {other:?}"),
        }
    }

    /// QURATOR-127 — BR1 leniency with an attributable reason, at the exact fold `browse_share_code`
    /// performs. A failure (here: the missing-index error a dead/incomplete relay produces) must
    /// yield `listing: None` **and** a non-empty reason distinguishing the mode; a follow-only code
    /// has no key and so no reason; a rendered listing carries no reason. Ends where production
    /// ends — the helper IS the site `browse_share_code` calls, not a re-emit of its shape.
    #[tokio::test]
    async fn br1_lock_yields_none_listing_and_names_the_inner_error() {
        let victim = Identity::generate();
        let peer = victim.public_key();
        let browse_key: BrowseKey = [7; 32];

        // Failure mode 2 (zero family events served): the empty-family error.
        let fetch_none = |_filter: Filter| async { Ok(Vec::<Event>::new()) };
        let r = fetch_slug_family(&peer, "gone", &browse_key, fetch_none).await;
        let (listing, reason) = listing_or_lock_reason("gone", r);
        assert!(listing.is_none(), "BR1: a failed fetch must lock, not error, the browse");
        let reason = reason.expect("the lock reason must be captured, not discarded");
        assert!(
            reason.contains("no listing found for slug 'gone'"),
            "the reason must name the actual inner error, got: {reason}"
        );

        // Failure mode 4 (decrypt failure on a served event): a real event under the WRONG key.
        let ev = build_listing_event(
            &victim,
            "locked",
            &browse_key,
            &serde_json::json!({ "slug": "locked", "entries": [] }).to_string(),
        )
        .unwrap();
        let fetch_wrong_key = move |_filter: Filter| {
            let ev = ev.clone();
            async move { Ok(vec![ev]) }
        };
        let wrong_key: BrowseKey = [99; 32];
        let r = fetch_slug_family(&peer, "locked", &wrong_key, fetch_wrong_key).await;
        let (listing, reason) = listing_or_lock_reason("locked", r);
        assert!(listing.is_none(), "BR1: a locked listing must not fail the browse");
        let reason = reason.expect("a decrypt failure must leave a reason");
        assert!(
            !reason.contains("no listing found"),
            "a decrypt failure must be distinguishable from an absent family, got: {reason}"
        );

        // A compliant render: no reason rides along.
        let ok_index = build_listing_event(
            &victim,
            "fine",
            &browse_key,
            &serde_json::json!({ "slug": "fine", "entries": [{ "name": "a" }] }).to_string(),
        )
        .unwrap();
        let fetch_ok = move |_filter: Filter| {
            let ok_index = ok_index.clone();
            async move { Ok(vec![ok_index]) }
        };
        let r = fetch_slug_family(&peer, "fine", &browse_key, fetch_ok).await;
        let (listing, reason) = listing_or_lock_reason("fine", r);
        assert!(listing.is_some(), "a compliant listing must render");
        assert!(reason.is_none(), "a rendered listing carries no lock reason");
    }

    /// `browse_peer_listings`'s count bound (the sibling gap two reviewers found): a family with
    /// more distinct `d`-tags than the ceiling must be SKIPPED **before any of its events are
    /// decrypted into `payloads`**. Ends where production ends — this drives
    /// `render_browsed_family`, the exact function `browse_peer_listings` calls per family. Every
    /// event here is a REAL, decryptable, renderable v1 part, so the ONLY thing that can make the
    /// over-cap leg skip is the count bound — with the bound removed, the identical family (bar
    /// its last part) renders `Some(..)` and the test reds.
    #[test]
    fn browsed_family_over_the_part_cap_is_skipped_before_decrypting() {
        let victim = Identity::generate();
        let browse_key: BrowseKey = [7; 32];

        // A real v1 split family: index (`d = doom`, parts: N) + N content parts, all
        // real-encrypted and renderable. Build it at over-cap size so the count bound is the
        // only possible refusal: the index declares N+1 parts but ships N+1 content parts...
        // instead, use the plain-unsplit shape per part so render stays simple: one index
        // (`split: true, parts: 1`) + ONE real part would render — but we need > cap+1 d-tags.
        // The faithful shape: an index plus (cap + 1) real parts whose payloads each carry
        // `part`/`parts` markers. render would fail on slotting (foreign part), so to keep the
        // "only the count bound can produce None" property we instead use cap+1 PLAIN unsplit
        // events with distinct d-tags — the enumeration path groups all of a root's d-tags
        // together, and > cap+1 of them is exactly the hostile hoard the bound exists for.
        let n_over = 4; // driven with family_part_cap = 2: 4 distinct d-tags > 2 + 1
        let mut by_d: HashMap<String, Vec<Event>> = HashMap::new();
        for i in 0..n_over {
            let json = serde_json::json!({
                "slug": "doom", "entries": [{ "name": format!("e{i}") }],
            })
            .to_string();
            by_d.insert(
                format!("doom#part{i}"),
                vec![build_listing_event(&victim, &format!("doom#part{i}"), &browse_key, &json)
                    .unwrap()],
            );
        }
        assert!(n_over > 2 + 1, "fixture must exceed the injected ceiling");
        assert!(
            render_browsed_family("doom", by_d, &browse_key, 2, MAX_RESTITCHED_BYTES).is_none(),
            "an over-cap family must be skipped by the count bound before any decrypt"
        );

        // Control leg: at the injected ceiling (3 d-tags == 2 + 1) the count bound does NOT fire.
        // Each event is a plain unsplit listing; render_listing picks... three plain singles
        // together are incoherent for v1 (stray parts without an index) — so use ONE plain
        // single as the whole family, the shape the count bound must never refuse.
        let single = serde_json::json!({
            "slug": "solo", "entries": [{ "name": "only" }],
        })
        .to_string();
        let mut solo_by_d: HashMap<String, Vec<Event>> = HashMap::new();
        solo_by_d.insert(
            "solo".to_string(),
            vec![build_listing_event(&victim, "solo", &browse_key, &single).unwrap()],
        );
        assert!(
            render_browsed_family("solo", solo_by_d, &browse_key, 2, MAX_RESTITCHED_BYTES)
                .is_some(),
            "a compliant single-event family must render — the count bound never bites at cap"
        );
    }

    /// `browse_peer_listings`'s byte bound: a family of REAL, valid, peer-signed NIP-44-encrypted
    /// events whose decrypted total exceeds a small injected cap is skipped with the byte-cap
    /// refusal, NOT rendered. Ends where production ends (`render_browsed_family` decrypts through
    /// `parse_listing_event`, exactly as the async loop does). With the bound removed this family
    /// renders `Some(..)` and the test reds.
    #[test]
    fn browsed_family_refuses_when_decrypted_bytes_exceed_the_cap() {
        let victim = Identity::generate();
        let browse_key: BrowseKey = [7; 32];

        // A compliant v1 split family: index (`d = big`) + one content part (`d = big#part0`).
        let index_json =
            serde_json::json!({ "slug": "big", "split": true, "parts": 1 }).to_string();
        let part_json = serde_json::json!({
            "slug": "big", "part": 0, "parts": 1,
            "entries": [{ "name": "file-one" }, { "name": "file-two" }],
        })
        .to_string();
        let mut by_d: HashMap<String, Vec<Event>> = HashMap::new();
        by_d.insert(
            "big".to_string(),
            vec![build_listing_event(&victim, "big", &browse_key, &index_json).unwrap()],
        );
        by_d.insert(
            "big#part0".to_string(),
            vec![build_listing_event(&victim, "big#part0", &browse_key, &part_json).unwrap()],
        );
        let cap = index_json.len() + part_json.len() - 1; // one byte under the true total

        // Control: with the production cap the same family renders fine.
        assert!(
            render_browsed_family("big", by_d.clone(), &browse_key, MAX_LISTING_PARTS, MAX_RESTITCHED_BYTES)
                .is_some(),
            "a compliant family under the real cap must render"
        );
        // One byte of headroom gone → the decrypted total crosses the injected cap → skipped.
        assert!(
            render_browsed_family("big", by_d, &browse_key, MAX_LISTING_PARTS, cap).is_none(),
            "a family whose decrypted total exceeds the byte cap must be skipped, never rendered"
        );
    }

    /// Source-scan pin (CLAUDE.md §9 — a bound not wired into production is decoration): the
    /// async enumeration fetch must carry NO `.limit()` (finding 2 — an author-wide REQ limit
    /// evicts a prolific peer's older collections), and the per-family bounds must actually be
    /// wired: `browse_peer_listings` must call `render_browsed_family` passing the two caps.
    /// Scanned at the SOURCE (this file), production section only, with the section length
    /// printed so "not found" is distinguishable from "nothing scanned".
    #[test]
    fn browse_peer_listings_bounds_are_enforced_and_no_listing_req_limit_remains() {
        let src = include_str!("browse.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or("");
        assert!(!prod.is_empty(), "source-scan guard is scanning nothing — include_str! broke");
        let scanned = prod.lines().count();
        // No KIND_LISTING fetch construction carries .limit() — the slug-scoped two-phase filters
        // declare their own budgets on separate lines; the author-wide enumeration carries none.
        let listing_limit_sites = prod
            .lines()
            .filter(|l| l.contains("KIND_LISTING"))
            .filter(|l| l.contains(".limit("))
            .count();
        assert_eq!(
            listing_limit_sites, 0,
            "a KIND_LISTING construction carries .limit() on one line — the author-wide \
             enumeration fetch must carry none (finding 2). Scanned {scanned} lines"
        );
        // The wiring: the async function calls the bounded pure helper with BOTH real caps.
        assert!(
            prod.contains("render_browsed_family(&root, by_d, browse_key, MAX_LISTING_PARTS, MAX_RESTITCHED_BYTES)"),
            "browse_peer_listings must route every family through render_browsed_family with \
             the real MAX_LISTING_PARTS + MAX_RESTITCHED_BYTES caps — an unwired bound is \
             decoration. Scanned {scanned} lines"
        );
    }

    // ── M16 W2: the snapshot-fingerprint staleness gate (pure) ──────────────────────────────────

    const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    /// A listing big enough to split under a 40 KiB budget, carrying a top-level
    /// `snapshot_fingerprint` (what the hoarder writes at publish time, W3). Entries carry the
    /// `item_type`/`tags`/`children` keys a real `collection_to_listing_json` tree emits, so the
    /// truncated output's digest re-derivation (audit #25) has a decodable `DirectoryItem` tree to
    /// hash — a fixture without them would take the "drop the fingerprint" path instead.
    fn big_listing_with_fp(slug: &str, n: usize, fp: &str) -> String {
        let entries: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "name": format!("title-{i:05}-padding-padding-padding-xx"),
                    "item_type": "File", "tags": [], "children": [],
                })
            })
            .collect();
        serde_json::json!({
            "slug": slug, "content_types": ["video"],
            "snapshot_fingerprint": fp, "entries": entries,
        })
        .to_string()
    }

    /// The full-carrier shape the publish path emits post audit #25: the full-tree
    /// `snapshot_fingerprint` (the family own) plus the `teaser_fingerprint` the truncated teaser
    /// of the same publish carries (visible entries + elided count). `tf` is the teaser digest.
    fn family_listing_with_fps(slug: &str, n: usize, tf: &str) -> String {
        let mut v: serde_json::Value =
            serde_json::from_str(&big_listing_with_fp(slug, n, FP)).unwrap();
        v.as_object_mut().unwrap().insert(
            "teaser_fingerprint".into(),
            serde_json::Value::String(tf.to_string()),
        );
        v.to_string()
    }

    #[test]
    fn snapshot_fingerprint_rides_through_the_big_relay_split() {
        // The W2 gate's premise, post audit #25 / QURATOR-123: a full listing carrying the
        // **teaser digest** (`teaser_fingerprint` — what the publish path stamps beside the
        // full-tree `snapshot_fingerprint`) survives the split (the big-relay carrier reuses the
        // 40 KiB budget) + restitch, so the digest is readable from the rendered family's meta and
        // the gate can compare it against the teaser's. The family still carries its own
        // full-tree `snapshot_fingerprint` — it is the teaser that may not.
        let json = family_listing_with_fps("vault", 1300, FP);
        let parts = split_listing("vault", &json, 40_000).unwrap();
        assert!(parts.len() > 2, "the listing must actually split, got {} part(s)", parts.len());
        let payloads: Vec<String> = parts.iter().map(|p| p.json.clone()).collect();
        let rendered = render_listing(&payloads).unwrap();
        assert!(rendered.complete(), "all parts present → complete tree");
        assert_eq!(rendered.entries.len(), 1300);
        assert_eq!(
            family_teaser_fingerprint(&rendered),
            Some(FP),
            "the teaser digest must survive the split into meta"
        );
        assert!(full_supersedes(&rendered, FP), "a matching teaser digest supersedes the teaser");
        assert!(!full_supersedes(&rendered, "deadbeef"), "a mismatched teaser digest does not supersede");
    }

    #[test]
    fn incomplete_family_never_supersedes_even_with_matching_fingerprint() {
        // Codex review (W2): a big relay can serve the signed index but WITHHOLD a content part. The
        // rendered tree is partial yet still carries the matching `snapshot_fingerprint` in its meta —
        // it must NOT replace the paywall (that would present a partial tree as the full list).
        let json = family_listing_with_fps("vault", 1300, FP);
        let parts = split_listing("vault", &json, 40_000).unwrap();
        assert!(parts.len() > 2, "the listing must split, got {} part(s)", parts.len());
        let mut payloads: Vec<String> = parts.iter().map(|p| p.json.clone()).collect();
        payloads.pop(); // withhold the last content part → an incomplete family
        let rendered = render_listing(&payloads).unwrap();
        assert!(!rendered.complete(), "a withheld part must render incomplete");
        assert_eq!(
            family_teaser_fingerprint(&rendered),
            Some(FP),
            "the teaser digest still rides in meta even when parts are missing"
        );
        assert!(
            !full_supersedes(&rendered, FP),
            "an incomplete family must keep the teaser despite the matching fingerprint"
        );
    }

    #[test]
    fn truncated_teaser_and_full_family_share_one_fingerprint() {
        // Audit #25 / QURATOR-123 redefined what a truncated teaser's digest may be: the VISIBLE
        // entries + elided count (`hb_core::teaser_fingerprint`), never the full-tree digest — the
        // latter was an offline confirm-or-deny oracle on the hidden content. So the teaser and the
        // big-relay full family no longer carry the same value by design; this test now pins both
        // halves of that contract:
        //   (a) the teaser's digest IS the visible-portion digest (not the full tree's);
        //   (b) the teaser's digest is NOT derivable from the full tree, so the teaser cannot
        //       confirm the family's hidden remainder.
        let json = big_listing_with_fp("vault", 2000, FP);
        let t = truncate_listing(&json, 40_000).unwrap();
        assert!(t.truncated, "this listing must truncate");
        let teaser = render_listing(&[t.json]).unwrap();
        let teaser_fp = listing_snapshot_fingerprint(&teaser).expect("a digest remains");
        assert_ne!(
            teaser_fp,
            FP,
            "the truncated teaser must NOT carry the full-tree fingerprint the family carries"
        );
        // The visible-portion digest, recomputed from the teaser's own kept entries: the artifact's
        // digest is what the re-stamp promises, derived from the artifact (not an internal).
        let visible: Vec<hb_core::types::DirectoryItem> =
            serde_json::from_value(serde_json::Value::Array(teaser.entries.clone())).unwrap();
        assert_eq!(
            teaser_fp,
            hb_core::teaser_fingerprint(&visible, (t.total_items - t.shown_items) as u64).0,
            "the teaser's digest must be the visible-portion + elided-count digest"
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

    // ── M19 W3: the author-pin (CWE-346) — a foreign-author event must never reach selection ──────

    /// A relay in the victim's pool can return a *validly-self-signed* listing event from a different
    /// key with a matching `d`-tag and a newer `created_at`. Without the author-pin in
    /// [`render_slug_family`] (shared with [`browse_peer_listings`] via [`authored_by`]),
    /// `select_newest_by_created_at` would pick the attacker's event and splice spoofed content into the
    /// rendered tree (or hide the real collection). This test asserts the foreign event is dropped
    /// before it can win — and that the victim's real (older) event is what renders.
    #[test]
    fn foreign_author_event_with_matching_d_tag_is_dropped() {
        let victim = Identity::generate();
        let attacker = Identity::generate();
        let peer = victim.public_key();
        let browse_key: BrowseKey = [7; 32];

        // The victim's real listing for `vault` (small enough to be a single unsplit event).
        let victim_json =
            serde_json::json!({ "slug": "vault", "entries": [{ "name": "real-file" }] }).to_string();
        let victim_event =
            build_listing_event(&victim, "vault", &browse_key, &victim_json).unwrap();

        // The attacker's spoofed listing: SAME `d=vault`, NEWER `created_at`, sealed under the SAME
        // browse-key (a share-code holder can do this) but signed by a DIFFERENT key. Its content is
        // distinct so we can tell which one rendered.
        let attacker_json =
            serde_json::json!({ "slug": "vault", "entries": [{ "name": "SPOOFED" }] }).to_string();
        let mut attacker_event =
            build_listing_event(&attacker, "vault", &browse_key, &attacker_json).unwrap();
        // Force a strictly-newer `created_at` than the victim's, so without the pin the attacker would
        // win `select_newest_by_created_at`. (build_listing_event uses wall-clock now; bump to be safe.)
        let newer_created_at = victim_event.created_at + 100;
        let rebuilt = EventBuilder::new(Kind::from_u16(KIND_LISTING), attacker_event.content.clone())
            .tags(attacker_event.tags.clone())
            .custom_created_at(newer_created_at);
        attacker_event = attacker.sign(rebuilt).unwrap();
        assert_ne!(attacker_event.pubkey, peer, "attacker must differ from victim");
        assert!(
            attacker_event.created_at > victim_event.created_at,
            "attacker must be newer so it would win without the pin"
        );

        // Feed BOTH into render_slug_family with `peer` pinned to the victim — the exact path
        // browse_peer_listings shares via authored_by. Attacker first to make a "first-seen wins" bug
        // would also surface here.
        let rendered = render_slug_family(
            vec![attacker_event.clone(), victim_event.clone()],
            &peer,
            "vault",
            &browse_key,
            MAX_RESTITCHED_BYTES,
        )
        .expect("the victim's real family must still render");

        // The attacker's content must NOT have won — the real collection renders.
        let rendered_str = serde_json::to_string(&rendered.entries).unwrap_or_default();
        assert!(
            rendered_str.contains("real-file") && !rendered_str.contains("SPOOFED"),
            "foreign-author event must be dropped before selection; got: {rendered_str}"
        );
        assert!(
            rendered.complete(),
            "the victim's single-event family must render complete"
        );

        // Directly prove the shared helper is the seam: the foreign event is filtered out, the victim's
        // is kept (order-independent).
        let kept = authored_by(vec![attacker_event, victim_event], &peer);
        assert_eq!(kept.len(), 1, "only the victim's event survives the pin");
        assert_eq!(kept[0].pubkey, peer);
    }

    /// Residual A (2026-08-24): a hostile peer publishing thousands of distinct part d-tags must
    /// be refused at the family-collection boundary, BEFORE the events are decrypted into
    /// `payloads`. The count check is what separates fixed from unfixed: the fixed path rejects on
    /// the distinct-d-tag count, while the unfixed path proceeds to decrypt and fails on the first
    /// undecryptable part (a different, later error). Every part here is validly signed by `peer`
    /// (so `authored_by` keeps it) but carries garbage content with no schema/crypto tags, so any
    /// decrypt attempt fails — proving the count bound fires without ever touching the ciphertext.
    #[test]
    fn slug_family_rejects_over_part_cap_before_decrypting() {
        let victim = Identity::generate();
        let peer = victim.public_key();
        let browse_key: BrowseKey = [7; 32];

        // One valid index event (`d = doom`) so the family has the canonical index-plus-parts shape.
        let index_json = serde_json::json!({
            "slug": "doom", "split": true, "parts_v": 2, "part_count": 0, "parts": [],
        })
        .to_string();
        let mut events = vec![build_listing_event(&victim, "doom", &browse_key, &index_json).unwrap()];

        // MAX_LISTING_PARTS + 1 content parts — distinct d-tags, validly signed by `peer`, garbage
        // content. With the index, that is MAX_LISTING_PARTS + 2 distinct d-tags, one over the cap.
        for i in 0..(MAX_LISTING_PARTS + 1) {
            let garbage =
                EventBuilder::new(Kind::from_u16(KIND_LISTING), format!("garbage-content-{i}"))
                    .tags([Tag::identifier(format!("doom#part{i}"))]);
            events.push(victim.sign(garbage).unwrap());
        }

        match render_slug_family(events, &peer, "doom", &browse_key, MAX_RESTITCHED_BYTES) {
            Err(NetError::Split(m)) => {
                assert!(
                    m.contains("distinct d-tags"),
                    "expected the family-level count cap, got: {m}"
                );
            }
            other => panic!("expected the family-level count-cap rejection, got {other:?}"),
        }
    }

    /// Residual A (2026-08-24), BYTE dimension — the half 50d7bea shipped unpinned because reaching
    /// the real 64 MiB cap on the wire would take >1000 real NIP-44 encryptions. Two pins:
    ///
    /// 1. The accrue-or-refuse decision at its REAL numbers (64 MiB exactly passes; +1 byte
    ///    refuses), so the bound's arithmetic — including that it refuses rather than clamps —
    ///    can't silently change.
    /// 2. The production loop ACTUALLY consults that decision: a family of real, valid,
    ///    peer-signed NIP-44-encrypted events whose decrypted total exceeds a small injected cap
    ///    is refused with the byte-cap error, NOT a downstream render error. Ends where production
    ///    ends (`render_slug_family` itself decrypts the events through `parse_listing_event`).
    #[test]
    fn slug_family_byte_bound_refuses_over_cap_at_real_numbers() {
        let cap = MAX_RESTITCHED_BYTES;
        // Exactly at the cap passes (the bound is `> cap`, not `>= cap`).
        let mut running = cap - 1024;
        assert!(
            accrue_decrypted_bytes(&mut running, 1024, cap).is_ok(),
            "a family totalling exactly the cap must pass"
        );
        assert_eq!(running, cap, "the running total accrues the next payload's bytes");
        // One byte over refuses.
        let mut over = cap - 1024;
        match accrue_decrypted_bytes(&mut over, 1025, cap) {
            Err(NetError::Split(m)) => assert!(
                m.contains(&format!("{cap}-byte cap")),
                "expected the byte-cap refusal, got: {m}"
            ),
            other => panic!("a family one byte over the cap must refuse, got {other:?}"),
        }
        assert_eq!(over, cap + 1, "the refused total is the true sum — never clamped back");
        // A single payload larger than the whole cap refuses on its own.
        let mut solo = 0;
        assert!(
            accrue_decrypted_bytes(&mut solo, cap + 1, cap).is_err(),
            "one part bigger than the cap must refuse on its own"
        );
    }

    /// The wiring half of the byte bound: real encrypted events, real decrypt path, small injected
    /// cap. `render_slug_family` is parameterised by the cap (production passes
    /// MAX_RESTITCHED_BYTES) exactly so this can drive the bound at test scale. The family is a
    /// compliant v1 index + one content part — well under the real cap, so with the bound REMOVED
    /// this family renders Ok and the test reds on the absence of the refusal.
    #[test]
    fn slug_family_refuses_when_decrypted_bytes_exceed_the_cap() {
        let victim = Identity::generate();
        let peer = victim.public_key();
        let browse_key: BrowseKey = [7; 32];

        // A compliant v1 split family: index (`d = big`) + one content part (`d = big#part0`).
        let index_json = serde_json::json!({
            "slug": "big", "split": true, "parts": 1,
        })
        .to_string();
        let part_json = serde_json::json!({
            "slug": "big", "part": 0, "parts": 1,
            "entries": [{ "name": "file-one" }, { "name": "file-two" }],
        })
        .to_string();
        let cap = index_json.len() + part_json.len() - 1; // one byte under the true total
        let events = vec![
            build_listing_event(&victim, "big", &browse_key, &index_json).unwrap(),
            build_listing_event(&victim, "big#part0", &browse_key, &part_json).unwrap(),
        ];

        // Sanity: with the production cap the same family renders fine (the bound never bites a
        // compliant family) — this is the control leg, proving the refusal below is the byte cap
        // and not something incidental to the family shape.
        render_slug_family(events.clone(), &peer, "big", &browse_key, MAX_RESTITCHED_BYTES)
            .expect("a compliant family under the real cap must render");

        // One byte of headroom gone → the decrypted total crosses the injected cap.
        match render_slug_family(events, &peer, "big", &browse_key, cap) {
            Err(NetError::Split(m)) => assert!(
                m.contains("-byte cap"),
                "expected the family-level decrypted-byte refusal, got: {m}"
            ),
            other => panic!("expected the byte-cap rejection, got {other:?}"),
        }
    }

    /// The teaser fetch is the listing paths' unhardened sibling (CWE-346): a relay in the victim's
    /// pool can return a *validly-self-signed* teaser from a different key with a newer `created_at`.
    /// Without the author-pin in [`select_newest_teaser`], `select_newest_by_created_at` would pick the
    /// attacker's event and spoof the browsed peer's name/bio/tags. This asserts the foreign teaser is
    /// dropped before selection, so the victim's real (older) teaser is what renders.
    #[test]
    fn foreign_author_teaser_is_dropped_from_teaser_selection() {
        let victim = Identity::generate();
        let attacker = Identity::generate();
        let peer = victim.public_key();

        let victim_teaser = Teaser {
            display_name: "victim".into(),
            bio: String::new(),
            tags: vec![],
            content_types: vec![],
            picture: None,
        };
        let attacker_teaser = Teaser {
            display_name: "SPOOFED".into(),
            bio: String::new(),
            tags: vec![],
            content_types: vec![],
            picture: None,
        };

        let victim_event = build_teaser(&victim, &victim_teaser, false).unwrap();
        let mut attacker_event = build_teaser(&attacker, &attacker_teaser, false).unwrap();
        // Force a strictly-newer `created_at` so without the pin the attacker would win selection.
        let newer_created_at = victim_event.created_at + 100;
        let rebuilt = EventBuilder::new(Kind::from_u16(KIND_TEASER), attacker_event.content.clone())
            .tags(attacker_event.tags.clone())
            .custom_created_at(newer_created_at);
        attacker_event = attacker.sign(rebuilt).unwrap();
        assert_ne!(attacker_event.pubkey, peer, "attacker must differ from victim");
        assert!(
            attacker_event.created_at > victim_event.created_at,
            "attacker must be newer so it would win without the pin"
        );

        // Attacker first so a "first-seen wins" bug would also surface here.
        let selected = select_newest_teaser(vec![attacker_event, victim_event], &peer)
            .expect("the victim's real teaser must still be selected");
        assert_eq!(
            selected.display_name, "victim",
            "foreign-author teaser must be dropped before selection; got {:?}",
            selected.display_name
        );
    }
}
