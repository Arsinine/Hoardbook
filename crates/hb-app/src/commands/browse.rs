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
    browse_peer_listings, browse_share_code, fetch_full_listing_if_current,
    listing_snapshot_fingerprint, search_teasers, RelayClient, RenderedListing, SearchHit,
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
        if !candidate.is_empty() && !out.contains(&candidate) {
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
async fn resolve_peer(
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
        None => vec![],
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
        online,
        last_fetched: Utc::now(),
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

#[tauri::command]
pub async fn follow(
    code: String,
    group_name: Option<String>,
    // M13 W5 item 4: an optional user-supplied petname, set at follow-time. Trailing `Option` keeps
    // existing callers (which pass fewer invoke args) working — a missing/`null` arg is simply "no
    // petname edit", falling back to the auto-derived one `resolve_peer` already set.
    petname: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let share_code = ShareCode::parse(&code).map_err(|e| format!("Invalid share code: {e}"))?;
    let me = identity_clone(&identity).await?;
    let mut peer = resolve_peer(&share_code, &me, &store, &relay).await?;
    // Defense-in-depth (R2): closes the AddContactDialog Skip path for a profileless peer even if
    // the caller bypassed the paste_key/lookup gate.
    reject_profileless(&peer)?;
    apply_follow_petname(&mut peer, petname);
    let npub = peer.npub.clone();
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
    let mut updated = resolve_peer(&share_code, &me, &store, &relay).await?;
    // Preserve local-only state across refresh.
    updated.local_tags = existing.local_tags;
    updated.petname = existing.petname.or(updated.petname);
    store.save_contact(&hash, &updated).map_err(cmd_err)?;
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
    // Recover the browsed peer's pubkey (author to pin) + browse-key (to decrypt) from the saved
    // contact — you can only see a truncated teaser worth upgrading if you hold their share code.
    let contact = store
        .load_contact(&CachedPeer::pubkey_hash(&npub))
        .map_err(cmd_err)?
        .ok_or("Add this peer as a contact with their share code before importing a manifest.")?;
    let share_code = contact_share_code(&contact)?;
    let peer = share_code.pubkey();
    let browse_key = share_code
        .browse_key()
        .ok_or("This contact has no browse key — re-add them with a full share code.")?;

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
    let envelope = parse_manifest_source(&raw)?;
    let result =
        open_manifest(&envelope, &peer, expected_slug.as_deref(), &browse_key, newest_fingerprint.as_deref())?;

    // Cache the verified envelope for offline re-browse, keyed (npub, slug, fingerprint) with LRU +
    // size-cap. Best-effort — a cache-write hiccup never fails the import the user just performed.
    if let Ok(json) = envelope.to_json() {
        let _ = manifest_cache::put(
            &store.manifest_cache_dir(),
            &npub,
            &envelope.slug,
            &envelope.snapshot_fingerprint,
            &json,
            now_secs(),
            manifest_cache::DEFAULT_MANIFEST_CACHE_BYTES,
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
/// (DISC3) — the stash stays 🔒 locked.
#[tauri::command]
pub async fn search_peers(
    tags: Vec<String>,
    content_types: Vec<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<PeerSearchHit>> {
    let (tags, content_types) = normalize_search_filters(tags, content_types)?;
    let me = identity_clone(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let hits = search_teasers(&client, &tags, &content_types, SEARCH_CAP, net::RELAY_TIMEOUT)
        .await
        .map_err(cmd_err)?;
    let contact_npubs: Vec<String> =
        store.list_contacts().map_err(cmd_err)?.into_iter().map(|c| c.npub).collect();
    let hits = filter_hits(hits, &me.npub(), &contact_npubs);
    Ok(hits.into_iter().map(hit_to_card).collect())
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
        };
        assert_eq!(hit_to_card(hit).bio, None, "a blank bio renders as None, not an empty string");
    }

    fn hit_for(npub: String) -> SearchHit {
        SearchHit {
            npub,
            teaser: Teaser { display_name: "x".into(), bio: String::new(), tags: vec![], content_types: vec![], picture: None },
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
            online: false,
            last_fetched: Utc::now(),
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
}
