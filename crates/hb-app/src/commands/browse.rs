//! Contacts: paste a share code, follow, refresh — rewired onto the M3 `hb-net` browse API.
//!
//! A "contact" is now keyed on the peer's **npub** (+ the account browse-key captured from a full
//! `hbk` code, which unlocks their listings + presence address). Resolving a peer is a **relay
//! read**: fetch their public teaser (`browse_share_code`) and their presence binding (for online
//! status). Full collection browsing is the dedicated M3 browse route (now the default) — the
//! inline `collections` on a contact is no longer populated here.

use chrono::Utc;
use nostr::prelude::ToBech32;
use tauri::State;

use hb_core::event::Teaser;
use hb_core::fingerprint::Fingerprint;
use hb_core::types::Collection;
use hb_core::{ShareCode, Identity};
use hb_net::{
    browse_peer_listings, browse_peer_listings_state, browse_share_code,
    fetch_full_listing_if_current, listing_snapshot_fingerprint, search_teasers_capped,
    ListingsState, RelayClient, RenderedListing, SearchHit,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{CmdResult, cmd_err},
    manifest_cache,
    net::{self, SharedRelay},
    store::{CachedPeer, DataStore},
    identity_state::SharedIdentity,
};

/// Current unix time in seconds (manifest-cache access stamps). A clock before 1970 reads as 0.
fn now_secs() -> u64 {
    Utc::now().timestamp().max(0) as u64
}

/// Discovery result cap — mirrors the teaser/discovery cap; a flood of teasers can't make the result
/// set unbounded.
const SEARCH_CAP: usize = 100;

/// A §6 Discovery teaser card (M12 W3). Carries **only** the opt-in public teaser — name/bio/tags/
/// content-types + the §7 fingerprint (the impersonation distinguisher for a stranger). It carries
/// **no listing and no browse-key** (DISC3): a search hit surfaces the advertisement, never the
/// hoard. The stash stays 🔒 browse-key-locked.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PeerSearchHit {
    pub npub: String,
    pub display_name: String,
    pub bio: Option<String>,
    pub tags: Vec<String>,
    pub content_types: Vec<String>,
    /// Optional data-URI avatar from the teaser (validated/sanitized at parse — never a remote URL).
    pub picture: Option<String>,
    /// The §7 word+color fingerprint, derived from the npub alone (no listing access).
    pub fingerprint: Option<Fingerprint>,
}

/// The discovery result: the ranked hit cards + whether the cap truncated the set (M20 W3). When
/// `capped` is `true`, more candidates existed than `SEARCH_CAP` kept, and the UI surfaces a
/// "showing first N" affordance — silently presenting 100-of-many as "everyone" is the bug this fixes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PeerSearchResult {
    pub hits: Vec<PeerSearchHit>,
    pub capped: bool,
}

/// Normalize + validate search filters (M12 W3, Decision I). Trims, lowercases tags, drops empties,
/// and enforces **≥1 filter** at the trust boundary (defense-in-depth — also enforced inside
/// `teaser_search_filter`; Gemini). Returns `(tags, content_types)` or an error string.
fn normalize_search_filters(
    tags: Vec<String>,
    content_types: Vec<String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let tags: Vec<String> =
        tags.into_iter().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
    let content_types: Vec<String> =
        content_types.into_iter().map(|c| c.trim().to_lowercase()).filter(|c| !c.is_empty()).collect();
    if tags.is_empty() && content_types.is_empty() {
        return Err("Enter at least one tag or content type to search.".into());
    }
    Ok((tags, content_types))
}

/// Drop discovery hits that are the searcher's own npub (devtest #4) or an already-added contact
/// (devtest #6) — Discover should only ever surface strangers, never yourself or someone already on
/// the roster. Pure and unit-testable without a relay.
fn filter_hits(hits: Vec<SearchHit>, me_npub: &str, contact_npubs: &[String]) -> Vec<SearchHit> {
    hits.into_iter().filter(|h| h.npub != me_npub && !contact_npubs.contains(&h.npub)).collect()
}

/// Map a verified discovery `SearchHit` → a teaser card, deriving the §7 fingerprint from the npub.
/// **No listing / browse-key is carried** (DISC3) — the card type structurally cannot hold one.
fn hit_to_card(hit: SearchHit) -> PeerSearchHit {
    let fingerprint =
        hb_core::identity::parse_npub(&hit.npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk));
    PeerSearchHit {
        npub: hit.npub,
        display_name: hit.teaser.display_name,
        bio: if hit.teaser.bio.is_empty() { None } else { Some(hit.teaser.bio) },
        tags: hit.teaser.tags,
        content_types: hit.teaser.content_types,
        picture: hit.teaser.picture,
        fingerprint,
    }
}

/// Map a public teaser into the local `Profile` shape the contacts UI renders.
fn teaser_to_profile(t: Teaser) -> hb_core::types::Profile {
    hb_core::types::Profile {
        display_name: t.display_name,
        bio: if t.bio.is_empty() { None } else { Some(t.bio) },
        tags: t.tags,
        content_types: t.content_types,
        picture: t.picture,
        since: None,
        est_size: None,
        languages: vec![],
        contact_hint: None,
        email: None,
        location: None,
        social_links: vec![],
        willing_to: vec![],
        updated: Utc::now(),
    }
}

/// A peer's collection as browsed with a full share code (M13 HANDOVER gap #5) — the `Collection`
/// plus the K-of-N part counts `hb-net::browse_peer_listings` returned for it. Mirrors the
/// `CollectionEntry` pattern (REGRESSION #90): the part-availability info is a **local browse-time
/// signal**, never folded into the hb-core wire `Collection` type itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCollection {
    #[serde(flatten)]
    pub collection: Collection,
    /// Total parts the peer's index claims for this collection. `None` for a pre-M13 cached entry
    /// (never fabricate a K-of-N badge for stale cache data — see `browse-view.ts::collectionAvailability`).
    #[serde(default)]
    pub parts_total: Option<usize>,
    /// Parts actually present. `None` alongside `parts_total` for a pre-M13 cached entry.
    #[serde(default)]
    pub parts_present: Option<usize>,
    /// devtest #7 — true when the author published only a truncated paywall teaser of this collection
    /// (too large to publish whole). `total_items` is the full item count; the browser shows the kept
    /// entries followed by a "N more hidden" fade. `None` for a listing without the marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_items: Option<usize>,
    /// M16 W4 — the full-tree snapshot fingerprint carried in the listing meta (both the teaser and
    /// the full manifest carry the same value). Surfaced as a browse-time signal so the import path
    /// can gate a manifest for staleness against the teaser the browser is currently showing. `None`
    /// for a listing without the marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_fingerprint: Option<String>,
    /// M16 W4 — unix-secs `created_at` of a manifest file the user imported to upgrade this truncated
    /// teaser to the full tree; the UI tags "full manifest imported · <created_at>". `None` on a
    /// normally-browsed collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_imported_at: Option<u64>,
    /// M16 W4 — the id of the teaser (index) event this collection was browsed from, so an "ask the
    /// owner for the full list" request can name the exact teaser event. Set by `resolve_peer`; `None`
    /// for a cached / pre-M16 collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teaser_event_id: Option<String>,
}

/// Map a `RenderedListing` (meta + entries, from `hb_net::browse_peer_listings`) back into a
/// `PeerCollection` — the inverse of `collection_to_listing_json`'s `listing` → `entries` remap,
/// mirroring `private_listing_to_collection`'s shape trick. Pure — unit-tested without a relay.
/// Unparseable meta (a family that doesn't decode as a `Collection`) → `None`, never a hard error.
pub(crate) fn rendered_to_peer_collection(r: &RenderedListing) -> Option<PeerCollection> {
    let mut map = r.meta.clone();
    // devtest #7: pull the paywall-teaser markers out of the meta before it's decoded as a Collection
    // (which has no such fields) — they become PeerCollection browse-time signals, like the K-of-N counts.
    let truncated = map.remove("truncated").and_then(|v| v.as_bool());
    let total_items = map.remove("total_items").and_then(|v| v.as_u64()).map(|n| n as usize);
    // M16 W4: the full-tree fingerprint rides in the meta (W3) — pull it out as a browse-time signal
    // (like the K-of-N counts) so the import path can gate staleness against the teaser being shown.
    let snapshot_fingerprint =
        map.remove("snapshot_fingerprint").and_then(|v| v.as_str().map(String::from));
    map.insert("listing".into(), serde_json::Value::Array(r.entries.clone()));
    let collection: Collection = serde_json::from_value(serde_json::Value::Object(map)).ok()?;
    Some(PeerCollection {
        collection,
        parts_total: Some(r.parts_total),
        parts_present: Some(r.parts_present),
        truncated,
        total_items,
        snapshot_fingerprint,
        manifest_imported_at: None,
        // Set by `resolve_peer` (which holds the fetched event ids); a bare render carries none.
        teaser_event_id: None,
    })
}

/// Ordered, de-duplicated big-relay candidates for browse-side full-manifest resolution (M16 W3),
/// per the owner ruling **(a) then (b)**: (a) the browser's OWN configured big relay first, then
/// (b) the peer's big relay advertised in the (browse-key-encrypted) teaser meta. Blank entries are
/// dropped and a peer relay identical to our own is not retried (the shared-community case, where the
/// hoarder's advertised relay equals ours). Pure — unit-tested without a relay.
fn big_relay_fetch_order<'a>(own_big: &'a str, peer_big: &'a str) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for candidate in [own_big.trim(), peer_big.trim()] {
        // SSRF guard: the peer's `big_relay_url` is attacker-authored — drop any candidate that
        // points at loopback/private/link-local (the own relay was already validated on save).
        if !candidate.is_empty()
            && !out.contains(&candidate)
            && net::validate_relay_url(candidate).is_ok()
        {
            out.push(candidate);
        }
    }
    out
}

/// For a browsed collection that came back as a truncated paywall teaser, try to upgrade it to the
/// FULL listing by fetching the big-relay family (M16 W3). Tries the big relays in
/// [`big_relay_fetch_order`] order — the browser's own (a), then the peer's advertised one (b) —
/// gating each on [`fetch_full_listing_if_current`] (fingerprint matches the teaser AND the tree is
/// complete). Returns the full `RenderedListing` on the first success, or `None` when the teaser is
/// not truncated, carries no fingerprint to gate on, or no big relay yields a current full tree — in
/// which case the caller keeps the teaser. Never a hard error: a big-relay hiccup just keeps the
/// teaser (the pre-M16 behaviour).
async fn resolve_full_if_truncated(
    peer: &nostr::PublicKey,
    slug: &str,
    browse_key: &[u8; 32],
    teaser: &RenderedListing,
    own_big: &str,
) -> Option<RenderedListing> {
    // Only a truncated teaser has a hidden remainder worth fetching.
    if teaser.meta.get("truncated").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    // Without the teaser's snapshot fingerprint there is nothing to gate staleness on — keep the teaser.
    let fingerprint = listing_snapshot_fingerprint(teaser)?;
    let peer_big = teaser.meta.get("big_relay_url").and_then(|v| v.as_str()).unwrap_or("");
    for candidate in big_relay_fetch_order(own_big, peer_big) {
        let relays = [candidate.to_string()];
        // Codex finding 1: read the big relay through a DEDICATED, EPHEMERAL client connected only to
        // it — never `ensure_relays` onto the shared pool. A big relay left in the shared pool would let
        // a later untargeted `browse_peer_listings` mix its split family with public teasers and bypass
        // this very completeness/fingerprint gate. The ephemeral identity also keeps our real npub off a
        // peer-advertised relay (the option-b privacy note). Best-effort: any connect/fetch miss just
        // tries the next candidate, else the caller keeps the teaser.
        let ephemeral = Identity::generate();
        let big_client = match RelayClient::connect(&ephemeral, &relays, net::RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let result = fetch_full_listing_if_current(
            &big_client, peer, slug, browse_key, &relays, fingerprint, net::RELAY_TIMEOUT,
        )
        .await;
        big_client.disconnect().await;
        if let Ok(Some(full)) = result {
            return Some(full);
        }
    }
    None
}

/// Resolve a share code to a `CachedPeer`: fetch the public teaser + the presence binding (online
/// status), as a pure relay read. Falls back to the local cache (stale, offline) when the relays
/// yield nothing.
///
/// `pub(crate)` (M20 W6.4): the WAN-U suite (`wan_it::suite_wan_u::u1_profile_resolve_funnel`)
/// drives this fn directly to exercise the production add-contact funnel over a live relay — the
/// same path `paste_key` calls. Promoted from private to `pub(crate)` so the in-crate harness can
/// reach it without widening to full `pub`; no new external surface is exposed.
pub(crate) async fn resolve_peer(
    share_code: &ShareCode,
    me: &Identity,
    store: &DataStore,
    relay: &SharedRelay,
) -> Result<CachedPeer, String> {
    let peer = share_code.pubkey();
    let npub = peer.to_bech32().map_err(cmd_err)?;
    let seed = net::relay_urls(store);

    let client = net::client(me, store, relay).await.map_err(cmd_err)?;
    let browse = browse_share_code(&client, share_code, "", &seed, &seed, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err);
    // Online = a fresh, valid presence binding exists for this npub.
    let online = match crate::presence::fetch_peer_presence(&client, &peer, net::RELAY_TIMEOUT).await {
        Ok(Some(ev)) => hb_core::verify_binding(
            &ev,
            &peer,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
        .is_ok(),
        _ => false,
    };

    let profile = browse.ok().and_then(|b| b.teaser).map(teaser_to_profile);
    // The §7 fingerprint is a pure function of the npub — always derivable, even for a peer who has
    // published no teaser (it is the impersonation distinguisher you check before trusting a stranger).
    let fingerprint = Some(hb_core::fingerprint::fingerprint(&peer));

    // A full share code (carrying a browse-key) can browse every listing family the peer has
    // published (M13 HANDOVER gap #5). A locked/failed family is already skipped inside
    // `browse_peer_listings` (BR1) — best-effort here too, mirroring the teaser fetch above:
    // unreadable listings must never fail the whole resolve.
    //
    // QURATOR-134: the KEYLESS arm is the one this bug lived in. `browse_peer_listings_state`
    // (hb-net, the one implementation — the UI never re-derives it) tells "they published
    // nothing" (Fetched) apart from "they published listings we can't decrypt" (Sealed) apart
    // from "the enumeration itself failed" (FetchFailed). For a keyed contact `collections` is
    // authoritative and the state stays `Fetched`.
    let mut listings_state = crate::store::ListingsStatus::Fetched;
    let collections = match share_code.browse_key() {
        Some(bk) => {
            // The browser's OWN big relay (option a) — tried before the peer's advertised one (b).
            let own_big = store.load_settings().map_err(cmd_err)?.unwrap_or_default().big_relay_url;
            let families = browse_peer_listings(&client, &peer, &bk, net::RELAY_TIMEOUT)
                .await
                .unwrap_or_default();
            let cache_dir = store.manifest_cache_dir();
            let now = now_secs();
            let mut out = Vec::with_capacity(families.len());
            for (root, teaser, teaser_event_id) in &families {
                // M16 resolution order for a truncated teaser: (W4) the local manifest cache first —
                // an offline, once-imported full tree — then (W3) the big relay (a → b); on either
                // success browse the full tree, otherwise the teaser as-is (unchanged).
                let full = match resolve_from_cache(&cache_dir, &peer, &npub, root, &bk, teaser, now) {
                    Some(r) => Some(r),
                    None => resolve_full_if_truncated(&peer, root, &bk, teaser, &own_big).await,
                };
                if let Some(mut pc) = rendered_to_peer_collection(full.as_ref().unwrap_or(teaser)) {
                    // Carry the teaser event id so an "ask the owner" request can name the exact event.
                    pc.teaser_event_id = teaser_event_id.clone();
                    out.push(pc);
                }
            }
            out
        }
        None => {
            // Keyless: run the enumeration read purely for its tri-state — it only fetches the
            // peer's own KIND_LISTING events (author-pinned) and classifies them; the throwaway
            // zero key decrypts nothing, so `Sealed` is reported for any existing family, which
            // is exactly the keyless reading. Never fails the resolve (BR1).
            let (_, state) = browse_peer_listings_state(
                &client,
                &peer,
                &[0u8; 32], // throwaway key — decrypts nothing; the read is for the tri-state
                net::RELAY_TIMEOUT,
            )
            .await
            .unwrap_or((Vec::new(), ListingsState::FetchFailed(String::new())));
            listings_state = state.into();
            vec![]
        }
    };

    // Fall back to the cached contact if the relay yielded no teaser.
    if profile.is_none() {
        if let Some(mut stale) = store.load_contact(&CachedPeer::pubkey_hash(&npub)).map_err(cmd_err)? {
            stale.online = online;
            stale.fingerprint = fingerprint;
            // A full share code just handed us a browse-key — merge it even though the teaser fetch
            // flaked (devtest #4: add-by-npub then paste the full code must not lose the key, or the
            // contact stays permanently unbrowseable).
            merge_browse_key(&mut stale, share_code);
            // If the keyed listings fetch above still succeeded, prefer the fresh listings over the
            // stale cache (devtest #3: a hiccup at add-time must not cache "empty" forever).
            if !collections.is_empty() {
                stale.collections = collections;
            }
            // QURATOR-134: the fresh tri-state supersedes the stale classification even when the
            // teaser flaked (same devtest-#3 reasoning — a keyless empty must not cache as
            // `Sealed` forever just because a later refresh's teaser fetch hiccuped).
            stale.listings_state = listings_state;
            return Ok(stale);
        }
    }

    Ok(CachedPeer {
        npub,
        source: crate::store::ContactSource::Manual,
        browse_key_hex: share_code.browse_key().map(hex::encode),
        petname: profile.as_ref().map(|p| p.display_name.clone()),
        profile,
        collections,
        listings_state,
        online,
        last_fetched: Utc::now(),
        // W5.2: presence age is stamped by the online poll, not by a browse (which proves nothing
        // about whether they are around).
        last_presence: None,
        local_tags: vec![],
        fingerprint,
    })
}

/// Snapshot the loaded identity (cloned) or error if none.
async fn identity_clone(identity: &SharedIdentity) -> Result<Identity, String> {
    identity
        .read()
        .await
        .as_ref()
        .map(|id| id.identity.clone())
        .ok_or_else(|| "No identity loaded. Generate a keypair first.".to_string())
}

/// R2: a peer with no published teaser cannot be added — unconditional reject at the trust
/// boundary (devtest #17/#18), regardless of online status. Pure and unit-testable without a
/// relay. The one deliberate exception is Q7 chat request-accept (`chat.rs
/// dm_request_accept_inner`), which builds its own local peer stub with `profile: None` — that
/// seam does not call this gate.
fn reject_profileless(peer: &CachedPeer) -> Result<(), String> {
    if peer.profile.is_none() {
        return Err(
            "This person hasn't published a profile yet, so there's nothing to add here. Ask them to publish a profile first."
                .into(),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn paste_key(
    code: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<CachedPeer> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    if me.public_key() == share_code.pubkey() {
        return Err("You cannot look up your own code".into());
    }
    let peer = resolve_peer(&share_code, &me, &store, &relay).await?;
    reject_profileless(&peer)?;
    Ok(peer)
}

/// M17 W3 — the purely-local share-code inspector. Parses a pasted `hbk…` / `npub1…` code and returns
/// the embedded npub, its §7 word+color fingerprint (derived from the npub ALONE), and whether the
/// code carries a browse-key — so a received-code card can render with the impersonation distinguisher
/// **at render time with ZERO network**. This is the structural answer to the W3 design decision: the
/// card never calls `paste_key` (which NETWORKS via `resolve_peer`) to draw itself; `paste_key` fires
/// only on the user's click. `ShareCode::parse` is the same checksum-validating codec
/// `validate_share_code` / `paste_key` use; `hb_core::fingerprint::fingerprint` is the single source
/// of the fingerprint algorithm (M3 decision #7 — never re-derived in JS).
#[derive(Debug, Clone, Serialize)]
pub struct ShareCodeInfo {
    pub npub: String,
    pub fingerprint: Fingerprint,
    /// A full `hbk…` code carries the account browse-key (unlocks listings); a bare `npub1` does not.
    pub has_browse_key: bool,
}

/// M17 W3 — parse a share code locally and return its npub + fingerprint + browse-key flag. **No
/// relay, no `resolve_peer`, no store read** — the card render path is zero-network by construction
/// (the only Tauri commands it invokes at render are this and `validate_share_code`, both local).
#[tauri::command]
pub async fn share_code_info(code: String) -> CmdResult<ShareCodeInfo> {
    share_code_info_inner(&code).map_err(cmd_err)
}

/// The pure core the [`share_code_info`] command wraps — extracted so the zero-network invariant +
/// golden-fingerprint agreement are unit-testable without Tauri `State`. `ShareCode::parse` is the
/// same checksum-validating codec `validate_share_code` / `paste_key` use; `fingerprint` is the
/// single source of the word+color algorithm (M3 decision #7 — never re-derived in JS).
pub(crate) fn share_code_info_inner(code: &str) -> Result<ShareCodeInfo, String> {
    let share_code = ShareCode::parse(code).map_err(|e| format!("Invalid share code: {e}"))?;
    let pubkey = share_code.pubkey();
    let npub = pubkey.to_bech32().map_err(|e| e.to_string())?;
    Ok(ShareCodeInfo {
        npub,
        fingerprint: hb_core::fingerprint::fingerprint(&pubkey),
        has_browse_key: share_code.browse_key().is_some(),
    })
}

/// Merge a freshly-resolved share code's browse-key onto a stale cached contact (devtest #4): when
/// the teaser fetch yields nothing we fall back to the cache, but a full share code just handed us a
/// browse-key — dropping it would leave an npub-added contact permanently unbrowseable even after the
/// user pastes the full code. A `FollowOnly`/bare code carries no key and leaves the field untouched.
/// Pure — unit-tested without a relay.
fn merge_browse_key(stale: &mut CachedPeer, share_code: &ShareCode) {
    if let Some(bk) = share_code.browse_key() {
        stale.browse_key_hex = Some(hex::encode(bk));
    }
}

/// Apply an optional follow-time petname edit onto a resolved peer (M13 W5 item 4): a `Some`
/// non-empty petname overrides whatever `resolve_peer` auto-derived from the teaser display_name;
/// `None` or an empty string leaves it untouched. Pure — unit-tested without a relay.
fn apply_follow_petname(peer: &mut CachedPeer, petname: Option<String>) {
    if let Some(p) = petname.filter(|p| !p.is_empty()) {
        peer.petname = Some(p);
    }
}

/// M17 W3 — preserve local-only state when `follow` re-adds an existing contact (the M15 keyless-add
/// bug lived in exactly this funnel). When a user follows a peer who is already in their contacts
/// (typically: added keyless via `npub1`, now re-added with a full `hbk…` code from a received
/// share-code card), `resolve_peer` may rebuild a fresh `CachedPeer` from the relay teaser and wipe
/// the locally-bound petname, local tags, and `source`. Mirrors `refresh_contact`'s preservation
/// semantics: an explicit follow-time petname edit (already applied by `apply_follow_petname`) wins
/// (signalled by `had_explicit_petname = true`); otherwise the freshly auto-derived teaser petname
/// is discarded in favour of the existing local petname. Local tags and source always carry over
/// (they are local-only, never re-derivable from a relay teaser). Pure — unit-tested without a relay.
fn merge_local_state(peer: &mut CachedPeer, existing: &CachedPeer, had_explicit_petname: bool) {
    // When the user supplied NO follow-time petname, `peer.petname` is resolve_peer's auto-derived
    // teaser display_name — discard it in favour of the existing local petname (which may itself be
    // None: a keyless npub-add with no nickname, where render falls back to the display_name). An
    // explicit follow-time edit (already applied by `apply_follow_petname`) always wins.
    if !had_explicit_petname {
        peer.petname = existing.petname.clone();
    }
    // Local tags + source are local-only — a relay teaser cannot re-derive them, so always carry over.
    peer.local_tags = existing.local_tags.clone();
    // A fresh resolve always produces `Manual`; a pre-existing `Topic` source is local-only state
    // (joining a Topic auto-added this peer) that a relay teaser cannot re-derive — preserve it.
    peer.source = existing.source;
    // QURATOR-119 #35: no-downgrade carry-over. A bare-npub (keyless) re-follow rebuilds the peer
    // with `browse_key_hex: None` and empty `collections`; falling back to the stored key/cache keeps
    // the contact browseable without re-sharing a full code. A full code carrying a *different,
    // non-empty* key is `Some` here and must win (a legitimate key rotation), so only an absent key
    // defers to storage — mirroring `merge_browse_key`'s absent-incoming-key rule.
    if peer.browse_key_hex.is_none() {
        peer.browse_key_hex = existing.browse_key_hex.clone();
    }
    if peer.collections.is_empty() {
        peer.collections = existing.collections.clone();
    }
}

/// The pure save-tail of [`follow`]: petname apply → R2 gate → local-state merge → store write →
/// optional group add. Extracted (M20 W2) so the follow path is testable without a relay, AND so the
/// "don't resolve twice" fix can hand a pre-resolved peer straight here, skipping `resolve_peer`.
/// Never calls the relay; only the local store. `had_explicit_petname` is computed by the caller
/// (before `apply_follow_petname` consumes the `petname` arg) — see `follow` for why the flag exists.
///
/// `pub(crate)` (M20 W6.4): the WAN-U suite drives this fn directly to assert the W2 contract over a
/// live relay — one `resolve_peer` (the lookup) then this save-tail (0 resolves). The structural
/// proof the relay is never dialed on the commit path lives in the unit test; the WAN-U row exercises
/// the same path end-to-end. Promoted from private to `pub(crate)` (no full `pub`) for the in-crate
/// harness only.
pub(crate) fn save_followed_peer(
    store: &DataStore,
    mut peer: CachedPeer,
    petname: Option<String>,
    group_name: Option<String>,
    had_explicit_petname: bool,
) -> Result<(), String> {
    reject_profileless(&peer)?;
    apply_follow_petname(&mut peer, petname);
    let npub = peer.npub.clone();
    if let Ok(Some(existing)) = store.load_contact(&CachedPeer::pubkey_hash(&npub)) {
        merge_local_state(&mut peer, &existing, had_explicit_petname);
    }
    store.save_contact(&CachedPeer::pubkey_hash(&npub), &peer).map_err(cmd_err)?;

    if let Some(gname) = group_name {
        let mut groups = store.load_groups().map_err(cmd_err)?;
        if let Some(group) = groups.iter_mut().find(|g| g.name == gname) {
            if !group.pubkeys.contains(&npub) {
                group.pubkeys.push(npub);
                group.modified_at = Utc::now();
            }
            store.save_groups(&groups).map_err(cmd_err)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn follow(
    code: String,
    group_name: Option<String>,
    // M13 W5 item 4: an optional user-supplied petname, set at follow-time. Trailing `Option` keeps
    // existing callers (which pass fewer invoke args) working — a missing/`null` arg is simply "no
    // petname edit", falling back to the auto-derived one `resolve_peer` already set.
    petname: Option<String>,
    // M20 W2: an optional pre-resolved peer. The AddContact funnel already resolved the peer via
    // `paste_key` (the lookup); passing that result here lets `follow` skip a SECOND `resolve_peer`
    // round-trip after the user commits (the perceived-latency fix). `None` (legacy callers — chat
    // Unlock, topic invite) falls back to resolving from `code` as before. The caller MUST pass a peer
    // whose npub matches the code's pubkey — enforced below.
    resolved_peer: Option<CachedPeer>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let had_explicit_petname = petname.as_ref().is_some_and(|p| !p.is_empty());
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;

    // The "don't resolve twice" fix: when the caller hands us the peer the lookup already resolved,
    // skip `resolve_peer` entirely. The npub is bound to the code's pubkey so a mismatched/crafted
    // peer (a stale lookup result for a DIFFERENT peer) is refused before it is trusted — the whole
    // point of `paste_key` resolving from the share code, not from the UI's say-so.
    let peer = if let Some(pre) = resolved_peer {
        let code_npub = share_code.pubkey().to_bech32().map_err(cmd_err)?;
        if pre.npub != code_npub {
            return Err(
                "The resolved peer does not match the share code. Re-look the peer up and try again."
                    .into(),
            );
        }
        pre
    } else {
        let me = identity_clone(&identity).await?;
        resolve_peer(&share_code, &me, &store, &relay).await?
    };
    save_followed_peer(&store, peer, petname, group_name, had_explicit_petname)
}

#[tauri::command]
pub async fn get_contacts(store: State<'_, DataStore>) -> CmdResult<Vec<CachedPeer>> {
    store.list_contacts().map_err(cmd_err)
}

#[tauri::command]
pub async fn unfollow_contact(npub: String, store: State<'_, DataStore>) -> CmdResult<()> {
    store.delete_contact(&CachedPeer::pubkey_hash(&npub)).map_err(cmd_err)
}

/// Rebuild a share code from a saved contact (npub + cached browse-key) so a refresh can re-read.
fn contact_share_code(contact: &CachedPeer) -> Result<ShareCode, String> {
    let pubkey = hb_core::identity::parse_npub(&contact.npub).map_err(cmd_err)?;
    match &contact.browse_key_hex {
        Some(hexk) => {
            let bytes: [u8; 32] = hex::decode(hexk)
                .map_err(cmd_err)?
                .try_into()
                .map_err(|_| "stored browse-key is not 32 bytes".to_string())?;
            Ok(ShareCode::Full { pubkey, browse_key: bytes })
        }
        None => Ok(ShareCode::FollowOnly { pubkey }),
    }
}

#[tauri::command]
pub async fn refresh_contact(
    npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<CachedPeer> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let existing = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    let share_code = contact_share_code(&existing)?;
    let me = identity_clone(&identity).await?;
    let updated = resolve_peer(&share_code, &me, &store, &relay).await?;
    save_refreshed_contact(&store, &hash, &existing, updated)
}

/// The pure save-tail of [`refresh_contact`]: merge local-only state onto the freshly resolved peer
/// then persist. Extracted (QURATOR-119 #12) so the refresh path's preservation is unit-testable
/// without a relay, mirroring [`follow`]'s [`save_followed_peer`] extraction. Never calls the relay;
/// only the local store. `source` is local-only (a relay teaser can only ever rebuild it as
/// `Manual`), so a pre-existing `Topic` contact must keep its badge — otherwise the Contacts
/// auto-refresh would silently promote a topic-sourced stranger into the private-listing trust gate
/// (`contact_author_allowlist` admits exactly `Manual`).
fn save_refreshed_contact(
    store: &DataStore,
    hash: &str,
    existing: &CachedPeer,
    mut updated: CachedPeer,
) -> Result<CachedPeer, String> {
    // Preserve local-only state across refresh.
    updated.local_tags = existing.local_tags.clone();
    updated.petname = existing.petname.clone().or(updated.petname);
    updated.source = existing.source;
    store.save_contact(hash, &updated).map_err(cmd_err)?;
    Ok(updated)
}

/// The result of importing a `.hbmanifest` file (M16 W4): the slug it upgrades, the full-tree
/// `PeerCollection` (its `truncated`/`total_items` cleared — the fade lifts), and whether the
/// manifest is older than the teaser the browser is showing (`stale` ⇒ "ask again", still imported).
#[derive(Debug, Clone, Serialize)]
pub struct ImportedManifest {
    pub slug: String,
    pub collection: PeerCollection,
    pub created_at: u64,
    pub stale: bool,
}

/// Upper bound on a manifest file / paste we will read before parsing. A single-ciphertext envelope
/// is NIP-44-bounded (~64 KB plaintext → ~90 KB base64 + JSON framing); 1 MB is a generous ceiling
/// that still refuses a multi-GB file a user was tricked into importing (a self-inflicted OOM guard).
const MANIFEST_FILE_MAX_BYTES: u64 = 1_000_000;

/// Parse a `.hbmanifest` from either its raw JSON text (the file the export writes) or a base64
/// encoding of that JSON (the paste fallback — safe against copy/paste mangling of the JSON). Tries
/// JSON first, then base64 → utf-8 → JSON. Nothing here trusts the contents; the caller verifies.
fn parse_manifest_source(raw: &str) -> Result<hb_core::manifest::ManifestEnvelope, String> {
    let trimmed = raw.trim();
    if let Ok(env) = hb_core::manifest::ManifestEnvelope::from_json(trimmed) {
        return Ok(env);
    }
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(trimmed.as_bytes())
        .map_err(|_| "Not a valid manifest — expected .hbmanifest JSON or its base64.".to_string())?;
    let text = String::from_utf8(decoded)
        .map_err(|_| "The pasted manifest did not decode to text.".to_string())?;
    hb_core::manifest::ManifestEnvelope::from_json(text.trim()).map_err(cmd_err)
}

/// Import a full-listing **manifest** the user received out of band (M16 W4), upgrading a truncated
/// paywall teaser to the whole tree. The manifest author is pinned to the **browsed peer** and the
/// signature verified *before* any decrypt or merge (headline failure mode #3: a manifest for peer A
/// must not import while browsing peer B, and a tampered body is refused before it is trusted). The
/// browse-key that opens the body is the one captured from that peer's share code at add-time.
/// Read-only w.r.t. the relay and the store — the result is returned for the session; a durable
/// local cache is M16 W4 slice 4.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn import_manifest(
    npub: String,
    expected_slug: Option<String>,
    path: Option<String>,
    pasted: Option<String>,
    newest_fingerprint: Option<String>,
    store: State<'_, DataStore>,
) -> CmdResult<ImportedManifest> {
    // The manifest bytes come from the picked file or the pasted text (never both required). Both are
    // size-capped before parsing so a huge file/paste can't OOM the app (MANIFEST_FILE_MAX_BYTES).
    let raw = match (path, pasted) {
        (Some(p), _) => {
            let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if len > MANIFEST_FILE_MAX_BYTES {
                return Err("That file is too large to be a manifest.".into());
            }
            std::fs::read_to_string(&p).map_err(|e| format!("Could not read manifest file: {e}"))?
        }
        (None, Some(t)) => {
            if t.len() as u64 > MANIFEST_FILE_MAX_BYTES {
                return Err("That pasted text is too large to be a manifest.".into());
            }
            t
        }
        (None, None) => return Err("No manifest file or text provided.".into()),
    };
    // Best-effort cache: this path returns the tree to the UI, so a cache miss costs only an offline
    // re-browse (unchanged behaviour).
    accept_manifest_bytes(&npub, expected_slug.as_deref(), &raw, newest_fingerprint.as_deref(), &store, false)
}

/// The verify→gate→render→cache tail shared by every way a manifest can arrive: the file/paste import
/// above, and the transport redemption (M18 W4).
///
/// **Extracted rather than reimplemented, deliberately.** W4's acceptance is that the transport
/// consumes a manifest with the M16 W4 gates *unchanged* — author pinned to the browsed peer,
/// `expected_slug` bound, signature verified before decrypt, completeness required. A second call
/// site that merely looked equivalent would satisfy that on the day it was written and drift by the
/// first change to either. There is one path, so there is nothing to drift.
///
/// `raw` is envelope JSON (or base64 of it — [`parse_manifest_source`] accepts both). The size cap is
/// applied by the *caller*, which is where the source's own ceiling lives: `MANIFEST_FILE_MAX_BYTES`
/// for a picked file, `MANIFEST_MAX_TRANSPORT_BYTES` for the wire.
pub(crate) fn accept_manifest_bytes(
    npub: &str,
    expected_slug: Option<&str>,
    raw: &str,
    newest_fingerprint: Option<&str>,
    store: &DataStore,
    cache_required: bool,
) -> CmdResult<ImportedManifest> {
    let contact = store
        .load_contact(&CachedPeer::pubkey_hash(npub))
        .map_err(cmd_err)?
        .ok_or("Add this peer as a contact with their share code before importing a manifest.")?;
    let share_code = contact_share_code(&contact)?;
    let peer = share_code.pubkey();
    let browse_key = share_code
        .browse_key()
        .ok_or("This contact has no browse key — re-add them with a full share code.")?;

    let envelope = parse_manifest_source(raw)?;
    let result = open_manifest(&envelope, &peer, expected_slug, &browse_key, newest_fingerprint)?;

    // Cache the verified envelope for offline re-browse, keyed (npub, slug, fingerprint) with LRU +
    // size-cap.
    //
    // **`cache_required` exists because the two callers differ in what the cache MEANS to them.**
    // For a file/paste import the cache is a convenience: the tree is returned to the UI and rendered
    // immediately, so a write hiccup costs an offline re-browse and nothing else — best-effort is
    // right, and that is the historical behaviour, unchanged.
    //
    // For a TRANSPORT redemption the cache is the delivery. Chat discards the returned tree and
    // Browse picks the manifest up from the cache; and the owner spends the ticket on our
    // acknowledgement. So a silently-dropped write there means the ticket is burned and the user has
    // nothing — the acceptance gate would have said "accepted" about a manifest that never landed.
    // Failing here is what keeps the ACK honest, because this runs *before* it.
    let cached = envelope.to_json().ok().and_then(|json| {
        manifest_cache::put(
            &store.manifest_cache_dir(),
            npub,
            &envelope.slug,
            &envelope.snapshot_fingerprint,
            &json,
            now_secs(),
            manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .ok()
    });
    if cache_required && cached.is_none() {
        return Err(
            "The full list arrived but could not be saved locally, so it was not accepted. \
             Check free disk space and ask again."
                .into(),
        );
    }
    Ok(result)
}

/// M16 W4 — try to upgrade a truncated teaser from the LOCAL manifest cache, before any relay (the
/// browse resolution order is: cache → big relay → keep the teaser). A once-imported manifest for
/// `(peer, slug, fingerprint)` is re-verified + re-decrypted + rendered offline. `None` when the
/// teaser isn't truncated, carries no fingerprint to gate on, the cache misses, or the cached envelope
/// no longer verifies (fails closed like a fresh import). Sync — no relay, no store write.
fn resolve_from_cache(
    dir: &std::path::Path,
    peer: &nostr::PublicKey,
    npub: &str,
    slug: &str,
    browse_key: &[u8; 32],
    teaser: &RenderedListing,
    now: u64,
) -> Option<RenderedListing> {
    if teaser.meta.get("truncated").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let fingerprint = listing_snapshot_fingerprint(teaser)?;
    let json = manifest_cache::get(dir, npub, slug, fingerprint, now)?;
    let envelope = hb_core::manifest::ManifestEnvelope::from_json(&json).ok()?;
    envelope.verify_author(peer).ok()?;
    // Bind the AUTHOR-SIGNED fingerprint to the teaser's (the cache filename/key is unsigned local
    // metadata): only serve a cached manifest whose signed snapshot matches the teaser being shown, so
    // a stale-but-authentic manifest can never shadow a newer teaser.
    if envelope.snapshot_fingerprint != fingerprint {
        return None;
    }
    let parts = envelope.decrypt(browse_key).ok()?;
    let rendered = hb_net::render_listing(&parts).ok()?;
    if !rendered.complete() {
        return None; // never upgrade a teaser to a partial cached tree
    }
    Some(rendered)
}

/// The pure verify→decrypt→render→convert core the [`import_manifest`] command wraps (extracted so the
/// security-relevant ordering is unit-testable without Tauri `State`). Verifies the envelope
/// (version → sha → author-pin → signature) BEFORE decrypting, author pinned to `peer`; renders the
/// full plaintext the same way the browse path renders a fetched listing; converts back to a
/// `PeerCollection` (a full tree carries no truncated/total_items meta, so the fade lifts); and flags
/// staleness (surfaced, never blocking — an older manifest still imports).
fn open_manifest(
    envelope: &hb_core::manifest::ManifestEnvelope,
    peer: &nostr::PublicKey,
    expected_slug: Option<&str>,
    browse_key: &[u8; 32],
    newest_fingerprint: Option<&str>,
) -> Result<ImportedManifest, String> {
    envelope.verify_author(peer).map_err(cmd_err)?;
    // Bind the import to the collection whose paywall was clicked (when a target is given): the slug is
    // authenticated by the signature, so a same-author manifest for a DIFFERENT collection cannot
    // silently swap the viewed tree.
    if let Some(want) = expected_slug {
        if envelope.slug != want {
            return Err(format!(
                "This manifest is for “{}”, not the collection you're viewing (“{}”).",
                envelope.slug, want
            ));
        }
    }
    let parts = envelope.decrypt(browse_key).map_err(cmd_err)?;
    let rendered = hb_net::render_listing(&parts).map_err(cmd_err)?;
    // A crafted (but validly-signed) manifest could decrypt to a bare split index with no content
    // parts, which renders an EMPTY tree; require completeness so a partial family can't masquerade as
    // the full list (mirrors the big-relay gate `fetch_full_listing_if_current`).
    if !rendered.complete() {
        return Err("The manifest is incomplete — it does not contain the full listing.".into());
    }
    let mut collection = rendered_to_peer_collection(&rendered)
        .ok_or("The manifest did not decode as a collection listing.")?;
    collection.manifest_imported_at = Some(envelope.created_at);
    let stale = newest_fingerprint.map(|fp| !envelope.matches_fingerprint(fp)).unwrap_or(false);
    Ok(ImportedManifest {
        slug: envelope.slug.clone(),
        collection,
        created_at: envelope.created_at,
        stale,
    })
}

/// Set user-defined local tags on a contact. Tags are stored locally and never shared.
#[tauri::command]
pub async fn set_contact_tags(
    npub: String,
    tags: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let mut peer = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    peer.local_tags = tags;
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// Set a contact's local, user-editable petname (M13 W5 item 4). Mirrors `set_contact_tags` — an
/// impersonation-resistant label bound to the `npub`, stored locally and never shared.
#[tauri::command]
pub async fn set_contact_petname(
    npub: String,
    petname: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let hash = CachedPeer::pubkey_hash(&npub);
    let mut peer = store
        .load_contact(&hash)
        .map_err(cmd_err)?
        .ok_or_else(|| format!("Contact {npub} not found"))?;
    peer.petname = Some(petname);
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// §6 Discovery (M12 W3): search public teasers by tag (AND) / content-type (OR) across the relays
/// and return teaser cards. **≥1 filter is required** (no unfiltered global peer list — §6). A hit
/// carries only the opt-in public teaser + the §7 fingerprint, **never** a listing or browse-key
/// (DISC3) — the stash stays 🔒 locked. The result carries a `capped` flag (M20 W3) set when the
/// `SEARCH_CAP` truncated the ranked set, so the UI can surface a "showing first N" affordance.
#[tauri::command]
pub async fn search_peers(
    tags: Vec<String>,
    content_types: Vec<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<PeerSearchResult> {
    let (tags, content_types) = normalize_search_filters(tags, content_types)?;
    let me = identity_clone(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let (hits, capped) =
        search_teasers_capped(&client, &tags, &content_types, SEARCH_CAP, net::RELAY_TIMEOUT)
            .await
            .map_err(cmd_err)?;
    let contact_npubs: Vec<String> =
        store.list_contacts().map_err(cmd_err)?.into_iter().map(|c| c.npub).collect();
    let hits = filter_hits(hits, &me.npub(), &contact_npubs);
    Ok(PeerSearchResult { hits: hits.into_iter().map(hit_to_card).collect(), capped })
}

/// QURATOR-70 — tag autocomplete: the set of tags this node has actually observed, for the Discover
/// search box's chip/pill affordance. "Observed" = tags carried on ingested teasers — the cached
/// contacts' `Profile.tags` (teasers we have already resolved) plus the identity's own profile tags.
/// The owner ruled multi-term search stays strict AND-on-tags; the chip affordance is
/// load-bearing for that ruling because it makes the second term a real tag picked from a list,
/// rather than hopeful free text that silently switches the search's kind. Pure over the local
/// cache — no relay read. Tags are lowercased, deduped, and sorted (alphabetical for determinism).
#[tauri::command]
pub async fn discover_observed_tags(
    store: State<'_, DataStore>,
) -> CmdResult<Vec<String>> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // The identity's own profile tags (the draft IS the published teaser source — see profile.rs).
    if let Some(profile) = store.load_profile_draft().map_err(cmd_err)? {
        for tag in &profile.tags {
            let t = tag.trim().to_lowercase();
            if !t.is_empty() {
                seen.insert(t);
            }
        }
    }
    // Tags from every cached contact's resolved teaser (Profile.tags).
    for peer in store.list_contacts().map_err(cmd_err)? {
        if let Some(profile) = &peer.profile {
            for tag in &profile.tags {
                let t = tag.trim().to_lowercase();
                if !t.is_empty() {
                    seen.insert(t);
                }
            }
        }
    }
    Ok(seen.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_requires_at_least_one_filter() {
        // Decision I / DISC4 at the command's trust boundary: empty ∧ empty is refused (defense in
        // depth — also enforced inside teaser_search_filter), and whitespace-only filters count as empty.
        assert!(normalize_search_filters(vec![], vec![]).is_err());
        assert!(normalize_search_filters(vec!["  ".into()], vec!["".into()]).is_err());
        let (tags, cts) = normalize_search_filters(vec![" Anime ".into()], vec![]).unwrap();
        assert_eq!(tags, vec!["anime".to_string()], "tags are trimmed + lowercased");
        assert!(cts.is_empty());
    }

    #[test]
    fn hit_card_derives_fingerprint_and_carries_no_listing_or_key_disc3() {
        // DISC3: a discovery card is the teaser + a derived fingerprint — never a listing or
        // browse-key. The card type structurally cannot hold one; assert the serialized shape too.
        let id = Identity::generate();
        let hit = SearchHit {
            npub: id.npub(),
            teaser: Teaser {
                display_name: "archivebox".into(),
                bio: "90s anime".into(),
                tags: vec!["anime".into()],
                content_types: vec!["video".into()],
                picture: Some("data:image/webp;base64,AA==".into()),
            },
            created_at: nostr::Timestamp::from(0),
        };
        let card = hit_to_card(hit);
        assert!(card.fingerprint.is_some(), "the §7 fingerprint is derived from the npub");
        assert_eq!(card.bio.as_deref(), Some("90s anime"));
        assert_eq!(card.picture.as_deref(), Some("data:image/webp;base64,AA=="), "teaser avatar rides the hit card");
        let json = serde_json::to_string(&card).unwrap();
        assert!(!json.contains("browse_key") && !json.contains("browseKey"), "no browse-key on a hit");
        assert!(!json.contains("listing"), "no listing on a hit (DISC3)");
    }

    #[test]
    fn hit_card_blank_bio_is_none() {
        let id = Identity::generate();
        let hit = SearchHit {
            npub: id.npub(),
            teaser: Teaser { display_name: "x".into(), bio: String::new(), tags: vec![], content_types: vec![], picture: None },
            created_at: nostr::Timestamp::from(0),
        };
        assert_eq!(hit_to_card(hit).bio, None, "a blank bio renders as None, not an empty string");
    }

    fn hit_for(npub: String) -> SearchHit {
        SearchHit {
            npub,
            teaser: Teaser { display_name: "x".into(), bio: String::new(), tags: vec![], content_types: vec![], picture: None },
            created_at: nostr::Timestamp::from(0),
        }
    }

    #[test]
    fn filter_hits_drops_own_npub_devtest_4() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let hits = vec![hit_for(me.npub()), hit_for(stranger.npub())];
        let kept = filter_hits(hits, &me.npub(), &[]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].npub, stranger.npub());
    }

    #[test]
    fn filter_hits_drops_existing_contacts_devtest_6() {
        let me = Identity::generate();
        let contact = Identity::generate();
        let stranger = Identity::generate();
        let hits = vec![hit_for(contact.npub()), hit_for(stranger.npub())];
        let kept = filter_hits(hits, &me.npub(), &[contact.npub()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].npub, stranger.npub());
    }

    #[test]
    fn filter_hits_keeps_strangers() {
        let me = Identity::generate();
        let stranger = Identity::generate();
        let hits = vec![hit_for(stranger.npub())];
        let kept = filter_hits(hits, &me.npub(), &[]);
        assert_eq!(kept.len(), 1);
    }

    fn valid_meta(slug: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut meta = serde_json::Map::new();
        meta.insert("slug".into(), serde_json::json!(slug));
        meta.insert("path_alias".into(), serde_json::json!(slug));
        meta.insert("item_count".into(), serde_json::json!(0));
        meta.insert("content_types".into(), serde_json::json!(["video"]));
        meta.insert("last_updated".into(), serde_json::json!(Utc::now().to_rfc3339()));
        meta
    }

    #[test]
    fn rendered_listing_maps_to_peer_collection_with_parts() {
        // A partial family (K of N): the counts carry straight through onto the PeerCollection.
        let rendered = RenderedListing {
            meta: valid_meta("films"),
            entries: vec![serde_json::json!({"name": "a.mkv", "item_type": "File", "tags": [], "children": []})],
            parts_total: 5,
            parts_present: 3,
            missing: vec![1, 4],
        };
        let peer_col = rendered_to_peer_collection(&rendered).expect("valid meta must convert");
        assert_eq!(peer_col.collection.slug, "films");
        assert_eq!(peer_col.collection.listing.len(), 1, "the rendered entries become the listing");
        assert_eq!(peer_col.parts_total, Some(5));
        assert_eq!(peer_col.parts_present, Some(3));

        // Malformed meta (missing the Collection's required fields) → None, never a panic/hard error.
        let malformed = RenderedListing {
            meta: serde_json::Map::new(),
            entries: vec![],
            parts_total: 1,
            parts_present: 1,
            missing: vec![],
        };
        assert!(rendered_to_peer_collection(&malformed).is_none(), "unparseable meta must convert to None");
    }

    #[test]
    fn rendered_listing_carries_the_paywall_truncation_markers() {
        // devtest #7: a browsed truncated teaser's `truncated`/`total_items` markers ride in the meta
        // and surface on the PeerCollection (they are NOT Collection fields, so they must be pulled
        // out before the meta is decoded as a Collection — otherwise a stricter Collection would reject
        // the unknown keys).
        let mut meta = valid_meta("bigvault");
        meta.insert("truncated".into(), serde_json::json!(true));
        meta.insert("total_items".into(), serde_json::json!(9000));
        let rendered = RenderedListing {
            meta,
            entries: vec![serde_json::json!({"name": "a.mkv", "item_type": "File", "tags": [], "children": []})],
            parts_total: 1,
            parts_present: 1,
            missing: vec![],
        };
        let peer_col = rendered_to_peer_collection(&rendered).expect("markers must not break the decode");
        assert_eq!(peer_col.truncated, Some(true));
        assert_eq!(peer_col.total_items, Some(9000));
        assert_eq!(peer_col.collection.slug, "bigvault");
    }

    // ── M16 W4: manifest import (verify → decrypt → merge) ─────────────────────────
    // `open_manifest` is the pure core the `import_manifest` command wraps around a contact lookup;
    // the wire (relay) is untouched — an imported manifest is a local file consume.

    const IMPORT_FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn full_listing_plaintext(slug: &str, fp: &str) -> String {
        // The canonical full listing JSON the export produces: metadata + the fingerprint + `entries`
        // (what `render_listing` consumes — a plain, unsplit listing).
        let mut meta = valid_meta(slug);
        meta.insert("snapshot_fingerprint".into(), serde_json::json!(fp));
        meta.insert(
            "entries".into(),
            serde_json::json!([{"name": "Ran.mkv", "item_type": "File", "tags": [], "children": []}]),
        );
        serde_json::to_string(&serde_json::Value::Object(meta)).unwrap()
    }

    fn a_manifest(slug: &str, fp: &str) -> (Identity, [u8; 32], hb_core::manifest::ManifestEnvelope) {
        let id = Identity::generate();
        let bk: [u8; 32] = [9u8; 32];
        let plaintext = full_listing_plaintext(slug, fp);
        let env =
            hb_core::manifest::build_manifest_envelope(&id, slug, &bk, fp, 1_700_000_000, &[plaintext])
                .unwrap();
        (id, bk, env)
    }

    #[test]
    fn open_manifest_upgrades_the_teaser_to_the_full_tree() {
        let (id, bk, env) = a_manifest("criterion", IMPORT_FP);
        let imported = open_manifest(&env, &id.public_key(), Some("criterion"), &bk, Some(IMPORT_FP)).unwrap();
        assert_eq!(imported.slug, "criterion");
        assert_eq!(imported.collection.collection.listing.len(), 1, "the full entries become the tree");
        // A full tree carries no truncation markers, so the paywall fade lifts.
        assert_eq!(imported.collection.truncated, None);
        assert_eq!(imported.collection.total_items, None);
        assert_eq!(imported.collection.manifest_imported_at, Some(1_700_000_000));
        assert!(!imported.stale, "matching fingerprint is not stale");
    }

    #[test]
    fn open_manifest_rejects_a_manifest_authored_by_another_peer() {
        // Headline failure mode #3: a manifest for peer A must not import while browsing peer B — the
        // author-pin rejects it before any decrypt or merge.
        let (_id, bk, env) = a_manifest("criterion", IMPORT_FP);
        let other = Identity::generate();
        assert!(open_manifest(&env, &other.public_key(), None, &bk, None).is_err());
    }

    #[test]
    fn open_manifest_rejects_a_tampered_body() {
        let (id, bk, env) = a_manifest("criterion", IMPORT_FP);
        let mut tampered = env.clone();
        tampered.ciphertexts[0].push_str("AA"); // flips the sha, refused before decrypt
        assert!(open_manifest(&tampered, &id.public_key(), None, &bk, None).is_err());
    }

    #[test]
    fn open_manifest_needs_the_right_browse_key() {
        // The signature verifies (author is right) but the wrong browse-key can't open the body.
        let (id, _bk, env) = a_manifest("criterion", IMPORT_FP);
        let wrong: [u8; 32] = [1u8; 32];
        assert!(open_manifest(&env, &id.public_key(), None, &wrong, None).is_err());
    }

    #[test]
    fn open_manifest_flags_a_stale_manifest_but_still_imports() {
        // Staleness is surfaced, never blocking (M16 UX rule): an older manifest still merges its tree.
        let (id, bk, env) = a_manifest("criterion", IMPORT_FP);
        let imported = open_manifest(&env, &id.public_key(), None, &bk, Some("00ff00ff")).unwrap();
        assert!(imported.stale, "a fingerprint mismatch is flagged");
        assert_eq!(imported.collection.collection.listing.len(), 1, "…yet the full tree still imports");
    }

    #[test]
    fn open_manifest_rejects_a_manifest_for_a_different_collection() {
        // A validly-signed manifest for another collection (same author) must not swap the viewed
        // collection when an expected slug is given — the slug is authenticated, so this is caught.
        let (id, bk, env) = a_manifest("criterion", IMPORT_FP);
        let err = open_manifest(&env, &id.public_key(), Some("something-else"), &bk, None).unwrap_err();
        assert!(err.contains("something-else"), "got: {err}");
    }

    #[test]
    fn open_manifest_rejects_an_incomplete_manifest() {
        // A validly-signed envelope whose plaintext is a bare split INDEX (no content parts) renders an
        // empty tree; the completeness gate refuses it so a partial family can't pose as the full list.
        let id = Identity::generate();
        let bk: [u8; 32] = [9u8; 32];
        // A well-formed v1 split INDEX (parts=3) with no content parts alongside → render_listing
        // returns Ok but with K=0 of 3 present, so `complete()` is false and the gate refuses it.
        let index_only = r#"{"slug":"criterion","split":true,"parts":3}"#.to_string();
        let env = hb_core::manifest::build_manifest_envelope(&id, "criterion", &bk, IMPORT_FP, 1, &[index_only])
            .unwrap();
        let err = open_manifest(&env, &id.public_key(), None, &bk, None).unwrap_err();
        assert!(err.to_lowercase().contains("incomplete"), "got: {err}");
    }

    #[test]
    fn resolve_from_cache_upgrades_a_truncated_teaser_and_gates_on_the_signed_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let (id, bk, env) = a_manifest("criterion", IMPORT_FP);
        let npub = id.npub();
        manifest_cache::put(
            dir.path(), &npub, "criterion", IMPORT_FP, &env.to_json().unwrap(), 1,
            manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
        )
        .unwrap();

        // A truncated teaser carrying the SAME fingerprint → the cache upgrades it to the full tree.
        let mut meta = valid_meta("criterion");
        meta.insert("truncated".into(), serde_json::json!(true));
        meta.insert("snapshot_fingerprint".into(), serde_json::json!(IMPORT_FP));
        let teaser = RenderedListing {
            meta: meta.clone(),
            entries: vec![],
            parts_total: 1,
            parts_present: 1,
            missing: vec![],
        };
        let full = resolve_from_cache(dir.path(), &id.public_key(), &npub, "criterion", &bk, &teaser, 2);
        assert!(full.is_some(), "matching fingerprint upgrades from cache");

        // A teaser advertising a DIFFERENT fingerprint must NOT be served the stale cached manifest.
        let mut stale_meta = valid_meta("criterion");
        stale_meta.insert("truncated".into(), serde_json::json!(true));
        stale_meta.insert("snapshot_fingerprint".into(), serde_json::json!("00ff00ff"));
        let newer_teaser = RenderedListing { meta: stale_meta, ..teaser.clone() };
        assert!(
            resolve_from_cache(dir.path(), &id.public_key(), &npub, "criterion", &bk, &newer_teaser, 3).is_none(),
            "a newer teaser (different fingerprint) never hits the old cache entry",
        );
    }

    #[test]
    fn parse_manifest_source_accepts_json_and_base64_and_rejects_garbage() {
        let (_id, _bk, env) = a_manifest("criterion", IMPORT_FP);
        let json = env.to_json().unwrap();
        assert_eq!(parse_manifest_source(&json).unwrap(), env, "the file the export writes");
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        assert_eq!(parse_manifest_source(&b64).unwrap(), env, "the base64 paste fallback");
        assert!(parse_manifest_source("not a manifest at all").is_err());
    }

    #[test]
    fn open_manifest_restitches_a_large_multi_part_family() {
        // The W4 residual: a listing too large for one NIP-44 event splits into a family (index +
        // content parts), the envelope carries the encrypted parts inline, and open_manifest decrypts +
        // restitches every part into the complete tree — the file carrier now serves large collections.
        let id = Identity::generate();
        let bk: [u8; 32] = [9u8; 32];
        let entries: Vec<serde_json::Value> = (0..2000)
            .map(|i| serde_json::json!({"name": format!("file-{i:05}.mkv"), "item_type": "File", "tags": [], "children": []}))
            .collect();
        let listing_json = serde_json::json!({
            "slug": "vault", "path_alias": "vault", "item_count": entries.len(),
            "content_types": ["video"], "last_updated": Utc::now().to_rfc3339(),
            "snapshot_fingerprint": IMPORT_FP, "entries": entries,
        })
        .to_string();
        let parts: Vec<String> = hb_net::split_listing("vault", &listing_json, 40_000)
            .unwrap()
            .into_iter()
            .map(|p| p.json)
            .collect();
        assert!(parts.len() > 1, "the listing must actually split for this test to mean anything");
        let env = hb_core::manifest::build_manifest_envelope(&id, "vault", &bk, IMPORT_FP, 1, &parts)
            .unwrap();
        assert_eq!(env.ciphertexts.len(), parts.len(), "every split part is sealed into the envelope");

        let imported = open_manifest(&env, &id.public_key(), Some("vault"), &bk, Some(IMPORT_FP)).unwrap();
        assert_eq!(imported.collection.collection.listing.len(), 2000, "the full tree restitches");
        assert_eq!(imported.collection.truncated, None, "a full tree lifts the paywall fade");
    }

    // ── M16 W3: browse-side big-relay resolution order (a → b) ────────────────────
    // The candidate ordering is pure; the actual fetch/merge round-trip (truncated teaser → full
    // tree) is proven by hb-it Suite BIG1/BIG2, same split as the publish-path tests.

    #[test]
    fn big_relay_fetch_order_tries_own_then_peer_deduped() {
        // (a) own first, then (b) the peer's advertised relay.
        assert_eq!(
            big_relay_fetch_order("ws://own:7777", "ws://peer:7777"),
            vec!["ws://own:7777", "ws://peer:7777"],
        );
        // No own setting ⇒ just the peer's advertised relay (option b alone).
        assert_eq!(big_relay_fetch_order("", "ws://peer:7777"), vec!["ws://peer:7777"]);
        // Own set, peer advertises none ⇒ just our own (option a alone).
        assert_eq!(big_relay_fetch_order("ws://own:7777", ""), vec!["ws://own:7777"]);
        // Shared-community case: the peer's advertised relay equals ours ⇒ tried once, not twice.
        assert_eq!(big_relay_fetch_order("ws://one:7777", "ws://one:7777"), vec!["ws://one:7777"]);
        // Trimmed; a blank peer entry is dropped.
        assert_eq!(big_relay_fetch_order("  ws://own:7777  ", "   "), vec!["ws://own:7777"]);
        // Both blank ⇒ nothing to try (keep the teaser).
        assert!(big_relay_fetch_order("", "").is_empty());
    }

    #[test]
    fn big_relay_fetch_order_drops_ssrf_peer_candidates() {
        // M19 W7: the peer's advertised `big_relay_url` is attacker-controlled; the SSRF guard must
        // reject loopback/private/link-local hosts before anything dials them. The own big relay is
        // validated on save so it survives; only the peer candidate gets filtered.
        // Own valid + peer malicious ⇒ only the own relay remains.
        for bad in [
            "ws://127.0.0.1:7777",
            "ws://169.254.169.254",
            "ws://10.0.0.5",
            "ws://172.16.0.1",
            "ws://192.168.1.1",
        ] {
            assert_eq!(
                big_relay_fetch_order("wss://big.example.com", bad),
                vec!["wss://big.example.com"],
                "peer SSRF candidate {bad} should be dropped",
            );
            // Own empty + peer malicious ⇒ nothing left to dial (keep the teaser).
            assert!(big_relay_fetch_order("", bad).is_empty(), "peer SSRF candidate {bad} with no own relay should yield nothing");
        }
        // A valid public peer relay is still kept (regression guard against over-filtering).
        assert_eq!(
            big_relay_fetch_order("wss://big.example.com", "wss://peer.example.com"),
            vec!["wss://big.example.com", "wss://peer.example.com"],
        );
    }

    #[test]
    fn peer_collection_serializes_with_flattened_collection_fields() {
        // REGRESSION #90 pattern: the parts info must sit ALONGSIDE the flattened Collection fields
        // in the wire JSON, not nested — so a pre-M13 consumer expecting a plain Collection object
        // still finds every Collection field at the top level.
        let rendered = RenderedListing {
            meta: valid_meta("films"),
            entries: vec![],
            parts_total: 2,
            parts_present: 2,
            missing: vec![],
        };
        let peer_col = rendered_to_peer_collection(&rendered).unwrap();
        let json = serde_json::to_value(&peer_col).unwrap();
        assert_eq!(json.get("slug").unwrap(), "films", "Collection fields are flattened to the top level");
        assert_eq!(json.get("parts_total").unwrap(), 2);
        assert_eq!(json.get("parts_present").unwrap(), 2);
    }

    // ── M13 W5 item 4: petname ─────────────────────────────────────────────────────

    fn test_store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn stub_peer(npub: &str, petname: Option<&str>) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source: crate::store::ContactSource::Manual,
            browse_key_hex: None,
            petname: petname.map(|s| s.to_string()),
            profile: None,
            collections: vec![],
            listings_state: Default::default(), // QURATOR-134: fixtures predate the tri-state; Fetched is the least-wrong default
            online: false,
            last_fetched: Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        }
    }

    // ── R2: profileless peers cannot be added ─────────────────────────────────────

    #[test]
    fn reject_profileless_errs_when_peer_has_no_profile() {
        let peer = stub_peer("hb1_test", None);
        assert!(reject_profileless(&peer).is_err());
    }

    #[test]
    fn reject_profileless_ok_when_peer_has_profile() {
        let mut peer = stub_peer("hb1_test", None);
        peer.profile = Some(teaser_to_profile(Teaser {
            display_name: "archivebox".into(),
            bio: String::new(),
            tags: vec![],
            content_types: vec![],
            picture: None,
        }));
        assert!(reject_profileless(&peer).is_ok());
    }

    #[test]
    fn follow_sets_edited_petname() {
        // An explicit non-empty petname overrides whatever resolve_peer auto-derived.
        let mut peer = stub_peer("hb1_test", Some("AutoName"));
        apply_follow_petname(&mut peer, Some("MyNickname".into()));
        assert_eq!(peer.petname.as_deref(), Some("MyNickname"));

        // No petname arg (the trailing-Option default for existing callers) leaves the
        // auto-derived one alone.
        let mut peer2 = stub_peer("hb1_test2", Some("AutoName2"));
        apply_follow_petname(&mut peer2, None);
        assert_eq!(peer2.petname.as_deref(), Some("AutoName2"), "no petname arg keeps the auto-derived one");

        // An empty-string petname is treated the same as "no edit", not "clear it".
        apply_follow_petname(&mut peer2, Some(String::new()));
        assert_eq!(peer2.petname.as_deref(), Some("AutoName2"), "an empty-string petname is a no-op");
    }

    // ── devtest #4: a pasted full share code's browse-key survives the stale-teaser fallback ────

    #[test]
    fn merge_browse_key_sets_key_from_full_code() {
        // Add-by-npub leaves the contact keyless; pasting the full code later must attach the key
        // even when the teaser fetch flakes and we fall back to the stale cache.
        let pubkey = Identity::generate().public_key();
        let mut stale = stub_peer("hb1_test", None);
        assert!(stale.browse_key_hex.is_none(), "starts keyless (npub-added)");

        merge_browse_key(&mut stale, &ShareCode::Full { pubkey, browse_key: [7u8; 32] });
        assert_eq!(
            stale.browse_key_hex.as_deref(),
            Some(hex::encode([7u8; 32]).as_str()),
            "the full code's browse-key is merged onto the stale contact"
        );
    }

    #[test]
    fn merge_browse_key_followonly_leaves_key_untouched() {
        // A bare/FollowOnly code carries no key — it must not clobber an already-keyed contact.
        let pubkey = Identity::generate().public_key();
        let mut keyed = stub_peer("hb1_test", None);
        keyed.browse_key_hex = Some(hex::encode([9u8; 32]));

        merge_browse_key(&mut keyed, &ShareCode::FollowOnly { pubkey });
        assert_eq!(
            keyed.browse_key_hex.as_deref(),
            Some(hex::encode([9u8; 32]).as_str()),
            "a keyless code is a no-op, never a downgrade"
        );
    }

    /// Mirrors `set_contact_petname`'s core logic at the store level (load → set → reload) — the
    /// same pattern this file's other `State`-taking commands are exercised with (no live relay
    /// needed; `set_contact_tags` has no direct-call test either, for the same reason).
    #[test]
    fn set_contact_petname_updates_contact() {
        let (_dir, store) = test_store();
        let npub = "hb1_testpeer".to_string();
        let hash = CachedPeer::pubkey_hash(&npub);
        store.save_contact(&hash, &stub_peer(&npub, None)).unwrap();

        let mut peer = store.load_contact(&hash).unwrap().unwrap();
        peer.petname = Some("Nickname".into());
        store.save_contact(&hash, &peer).unwrap();

        let loaded = store.load_contact(&hash).unwrap().unwrap();
        assert_eq!(loaded.petname.as_deref(), Some("Nickname"), "the new petname must persist");
    }

    // ── M17 W3: share_code_info — zero-network render-time parse + fingerprint ──────────
    // The card render path MUST cost zero relay round-trips: `share_code_info_inner` is pure, touching
    // only the local codec + the pure fingerprint derivation. The same fingerprint algorithm is pinned
    // by `hb_core::fingerprint::tests::fingerprint_matches_golden_vectors` + the cross-language
    // `fingerprint_vectors.json` fixture; the assertions below confirm THIS command agrees with them.

    #[test]
    fn share_code_info_full_code_has_browse_key_and_golden_fingerprint() {
        // The npub whose fingerprint is the golden vector "thorn jetty luster trellis nacre
        // #7ea007ce" (secret …01). `share_code_info_inner` must reproduce it exactly — the card's
        // fingerprint is single-sourced in hb-core, never re-derived in JS (M3 decision #7).
        let id = Identity::from_secret(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let npub = id.npub();
        // Build a full `hbk…` code (npub + a browse-key) and confirm the parse round-trips.
        let code = ShareCode::Full { pubkey: id.public_key(), browse_key: [7u8; 32] }
            .encode()
            .unwrap();
        let info = share_code_info_inner(&code).unwrap();
        assert_eq!(info.npub, npub, "the embedded npub is recovered");
        assert_eq!(
            info.fingerprint.words,
            vec!["thorn", "jetty", "luster", "trellis", "nacre"],
            "golden words"
        );
        assert_eq!(info.fingerprint.color_hex, "#7ea007ce", "golden color");
        assert!(info.has_browse_key, "a full hbk code carries the browse-key");
    }

    #[test]
    fn share_code_info_bare_npub_has_no_browse_key() {
        let id = Identity::generate();
        let info = share_code_info_inner(&id.npub()).unwrap();
        assert_eq!(info.npub, id.npub());
        assert!(!info.has_browse_key, "a bare npub carries no browse-key");
        // The fingerprint is still derived — it is a function of the npub ALONE, so a keyless code
        // still shows the impersonation distinguisher on the card.
        assert_eq!(info.fingerprint.words.len(), 5);
        assert!(info.fingerprint.color_hex.starts_with('#'));
    }

    #[test]
    fn share_code_info_rejects_invalid_checksum() {
        // A checksum-invalid lookalike must Err — the frontend treats Err as "no card, plain text".
        assert!(share_code_info_inner("hbk1zzzzzzzz").is_err());
        assert!(share_code_info_inner("not a code at all").is_err());
        assert!(share_code_info_inner("").is_err());
    }

    // ── M17 W3: follow preserves local state on re-add (the M15 keyless-add bug) ──────────

    #[test]
    fn merge_local_state_discards_auto_derived_petname_when_no_explicit_edit() {
        // The W3 regression: `resolve_peer` sets `petname = profile.display_name` (Some, not None)
        // whenever a teaser exists. Unlock passes no petname, so `had_explicit_petname = false` and
        // the auto-derived teaser name ("Alice") MUST be discarded in favour of the existing local
        // petname ("Rae"). The pre-fix code inspected `peer.petname.is_none()` (false here) and kept
        // "Alice", clobbering the user's chosen name.
        let npub = "npub1_testpeer";
        let mut fresh = stub_peer(npub, None);
        fresh.petname = Some("Alice".into()); // resolve_peer's auto-derived teaser display_name
        let existing = stub_peer(npub, Some("Rae"));

        merge_local_state(&mut fresh, &existing, false);
        assert_eq!(fresh.petname.as_deref(), Some("Rae"), "the local petname survives");
    }

    #[test]
    fn merge_local_state_user_follow_time_petname_wins_over_existing() {
        // The user's follow-time edit (already applied via apply_follow_petname) takes precedence;
        // merge_local_state must NOT clobber it with the older existing petname.
        let npub = "npub1_testpeer";
        let mut fresh = stub_peer(npub, None);
        fresh.petname = Some("NewEdit".into()); // user just typed this
        let existing = stub_peer(npub, Some("OldName"));

        merge_local_state(&mut fresh, &existing, true);
        assert_eq!(fresh.petname.as_deref(), Some("NewEdit"), "the follow-time edit wins");
    }

    #[test]
    fn merge_local_state_preserves_local_tags_and_topic_source() {
        // Local tags + Topic source are local-only — a relay teaser cannot re-derive them.
        let npub = "npub1_testpeer";
        let mut fresh = stub_peer(npub, None);
        let mut existing = stub_peer(npub, Some("Name"));
        existing.local_tags = vec!["trader".into(), "eu".into()];
        existing.source = crate::store::ContactSource::Topic;

        merge_local_state(&mut fresh, &existing, false);
        assert_eq!(fresh.local_tags, vec!["trader".to_string(), "eu".to_string()], "tags carry over");
        assert_eq!(fresh.source, crate::store::ContactSource::Topic, "a Topic source survives a re-follow");
    }

    // ── M20 W2: a single add issues ONE resolve_peer ──────────────────────────────────
    // The fix: `follow` accepts an optional pre-resolved peer (the lookup's result) and hands it
    // straight to `save_followed_peer`, skipping the second `resolve_peer`. The tail extracted out
    // behind a pure helper is what makes that provable WITHOUT a relay: `save_followed_peer` touches
    // only the store, so a unit test exercising it is, by construction, asserting the relay was never
    // dialed in the pre-resolved path.

    fn stub_peer_with_profile(npub: &str, display_name: &str) -> CachedPeer {
        let mut peer = stub_peer(npub, Some(display_name));
        peer.profile = Some(teaser_to_profile(Teaser {
            display_name: display_name.into(),
            bio: String::new(),
            tags: vec![],
            content_types: vec![],
            picture: None,
        }));
        peer
    }

    #[test]
    fn save_followed_peer_persists_a_pre_resolved_peer_without_a_relay() {
        // The "don't resolve twice" fix in miniature: the lookup already produced a CachedPeer, and
        // the follow leg persists it via `save_followed_peer` alone — no `resolve_peer` call, no relay.
        // A store round-trip is the proof the peer landed; that the function signature takes no
        // `SharedRelay`/`Identity` (only `&DataStore`) is the structural proof the relay was not dialed.
        let (_dir, store) = test_store();
        let npub = "npub1_w2_resolve";
        let peer = stub_peer_with_profile(npub, "W2Peer");

        save_followed_peer(&store, peer.clone(), None, None, false).unwrap();

        let loaded = store
            .load_contact(&CachedPeer::pubkey_hash(npub))
            .unwrap()
            .expect("the pre-resolved peer is persisted");
        assert_eq!(loaded.npub, npub);
        assert_eq!(loaded.petname.as_deref(), Some("W2Peer"), "the auto-derived petname survives");
    }

    #[test]
    fn save_followed_peer_applies_an_explicit_petname() {
        // The user's follow-time petname edit rides the pre-resolved path too (the dialog sits between
        // lookup and follow). Guards the W3 regression on this new code path.
        let (_dir, store) = test_store();
        let npub = "npub1_w2_petname";
        let peer = stub_peer_with_profile(npub, "AutoName");

        save_followed_peer(&store, peer, Some("MyNick".into()), None, true).unwrap();

        let loaded = store.load_contact(&CachedPeer::pubkey_hash(npub)).unwrap().unwrap();
        assert_eq!(loaded.petname.as_deref(), Some("MyNick"), "the explicit petname wins");
    }

    #[test]
    fn save_followed_peer_adds_to_a_group() {
        // The group add also rides the pre-resolved path — nothing about the resolve is needed for a
        // local group membership write.
        let (_dir, store) = test_store();
        let npub = "npub1_w2_group";
        // Seed an empty group the follow will add into.
        let groups = vec![crate::store::Group {
            name: "Pals".into(),
            pubkeys: vec![],
            modified_at: Utc::now(),
            color: None,
        }];
        store.save_groups(&groups).unwrap();

        save_followed_peer(&store, stub_peer_with_profile(npub, "G"), None, Some("Pals".into()), false)
            .unwrap();

        let reloaded = store.load_groups().unwrap();
        let group = reloaded.iter().find(|g| g.name == "Pals").unwrap();
        assert!(
            group.pubkeys.iter().any(|p| p == npub),
            "the pre-resolved peer is added to the group: {:?}",
            group.pubkeys
        );
    }

    #[test]
    fn save_followed_peer_preserves_local_state_on_re_add() {
        // The W3 merge also rides the pre-resolved path — a pre-existing contact's local petname/tags
        // survive a re-follow that skips the resolve.
        let (_dir, store) = test_store();
        let npub = "npub1_w2_readd";
        let hash = CachedPeer::pubkey_hash(npub);
        let mut existing = stub_peer_with_profile(npub, "Cached");
        existing.local_tags = vec!["vip".into()];
        store.save_contact(&hash, &existing).unwrap();

        // A second add with no explicit petname: the auto-derived name ("Fresh") must be discarded in
        // favour of the existing cached petname ("Cached").
        let fresh = stub_peer_with_profile(npub, "Fresh");
        save_followed_peer(&store, fresh, None, None, false).unwrap();

        let loaded = store.load_contact(&hash).unwrap().unwrap();
        assert_eq!(loaded.petname.as_deref(), Some("Cached"), "the local petname survives re-add");
        assert_eq!(loaded.local_tags, vec!["vip".to_string()], "local tags survive re-add");
    }

    // ── QURATOR-119 #12: refresh must not promote a Topic-sourced contact to Manual ──────────
    // Contacts auto-refreshes every contact on mount; `refresh_contact` rebuilds each via
    // `resolve_peer`, which hardcodes `source: Manual`. Without this guard a topic-sourced stranger
    // (who joined a public Topic, zero interaction with the victim) would be durably flipped to
    // `Manual` on the victim's next Contacts visit, silently admitting them to the private-listing
    // trust gate (`contact_author_allowlist` admits exactly `Manual`). This drives
    // `save_refreshed_contact` — the exact save-tail `refresh_contact` runs — through a store
    // round-trip, NOT `merge_local_state` (follow's helper, which already carried the guard and so
    // stayed green while this path broke).

    #[test]
    fn refresh_preserves_topic_source_through_refresh_contact_save_tail() {
        let (_dir, store) = test_store();
        let npub = "npub1_q119_refresh";
        let hash = CachedPeer::pubkey_hash(npub);

        // A topic-sourced contact, exactly as `upsert_topic_contact` leaves it.
        let mut existing = stub_peer(npub, None);
        existing.source = crate::store::ContactSource::Topic;
        store.save_contact(&hash, &existing).unwrap();

        // What `resolve_peer` hands `refresh_contact`: a fresh rebuild, hardcoded `Manual`, carrying a
        // freshly-derived teaser petname and no local state.
        let updated = stub_peer(npub, Some("FreshTeaserName"));
        assert_eq!(
            updated.source,
            crate::store::ContactSource::Manual,
            "resolve_peer rebuilds as Manual — this is the bug's premise"
        );

        let saved = save_refreshed_contact(&store, &hash, &existing, updated).unwrap();
        assert_eq!(
            saved.source,
            crate::store::ContactSource::Topic,
            "refresh must not promote a Topic contact to Manual"
        );
        let reloaded = store.load_contact(&hash).unwrap().unwrap();
        assert_eq!(
            reloaded.source,
            crate::store::ContactSource::Topic,
            "the durable record keeps the Topic badge"
        );
    }

    // ── QURATOR-119 #35: re-follow must not drop a stored browse-key / collections ──────────
    // Re-following an already-keyed contact via a bare-npub (keyless) share code rebuilds the peer
    // keyless with empty collections. `merge_local_state` must fall back to the stored key/cache so
    // the peer stays browseable, while a full code carrying a *different* key (a rotation) still wins.

    fn cached_collection(slug: &str) -> PeerCollection {
        PeerCollection {
            collection: Collection {
                slug: slug.into(),
                path_alias: slug.into(),
                description: None,
                item_count: 1,
                est_size: None,
                content_types: vec![],
                tags: vec![],
                languages: vec![],
                visibility: hb_core::types::Visibility::Public,
                sorted: false,
                last_updated: Utc::now(),
                listing: vec![],
            },
            parts_total: None,
            parts_present: None,
            truncated: None,
            total_items: None,
            snapshot_fingerprint: None,
            manifest_imported_at: None,
            teaser_event_id: None,
        }
    }

    #[test]
    fn merge_local_state_carries_over_stored_browse_key_and_collections_when_fresh_are_empty() {
        let npub = "npub1_q119_keyless";
        let mut fresh = stub_peer(npub, None);
        assert!(fresh.browse_key_hex.is_none(), "a bare-npub re-follow rebuilds keyless");
        assert!(fresh.collections.is_empty(), "and without cached collections");

        let mut existing = stub_peer(npub, Some("Cached"));
        existing.browse_key_hex = Some(hex::encode([11u8; 32]));
        existing.collections = vec![cached_collection("vault")];

        merge_local_state(&mut fresh, &existing, false);
        assert_eq!(
            fresh.browse_key_hex.as_deref(),
            Some(hex::encode([11u8; 32]).as_str()),
            "the stored browse-key survives a keyless re-follow"
        );
        assert_eq!(fresh.collections.len(), 1, "cached collections survive a keyless re-follow");
        assert_eq!(fresh.collections[0].collection.slug, "vault");
    }

    #[test]
    fn merge_local_state_lets_a_rotated_browse_key_win_over_the_stored_key() {
        // A full code carrying a DIFFERENT, non-empty key is a rotation and must win — only an
        // absent key falls back to storage. This is what distinguishes #35's no-downgrade from a
        // "never update the key" bug.
        let npub = "npub1_q119_rotate";
        let mut fresh = stub_peer(npub, None);
        fresh.browse_key_hex = Some(hex::encode([22u8; 32])); // the rotated key from the new code

        let mut existing = stub_peer(npub, Some("Cached"));
        existing.browse_key_hex = Some(hex::encode([11u8; 32])); // the old key on disk

        merge_local_state(&mut fresh, &existing, false);
        assert_eq!(
            fresh.browse_key_hex.as_deref(),
            Some(hex::encode([22u8; 32]).as_str()),
            "a non-empty incoming key (rotation) wins over the stored key"
        );
    }
}
