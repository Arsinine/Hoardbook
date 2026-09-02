//! The fulfil verb (M18 W4): the two commands that turn an inert "please send me the full list" DM
//! into a manifest that actually arrives.
//!
//! Two halves, one per side of the plane:
//!
//! - [`send_full_list`] — the **owner** side. Behind an explicit human click, always: build the
//!   manifest, bind the plane, mint a ticket for this one request, record it, DM it. Nothing here is
//!   reachable from a timer or a relay event, which is M17 ruling #4 (the app never auto-sends) kept
//!   structural rather than remembered.
//! - [`redeem_manifest_ticket`] — the **asker** side. Dial, fetch, and hand the bytes to the
//!   *existing* M16 W4 gate. Redemption is immediate and has no defer affordance (owner ruling
//!   2026-07-30): there is no "redeem later" entry point for a button to bind to.
//!
//! **Export stays reachable and stays the fallback.** `export_manifest` is untouched by this module;
//! when the plane cannot connect, [`send_full_list`] fails with an error that names export rather than
//! leaving the hoarder with a dead button and no second route.

use std::sync::Arc;

use hb_core::{ManifestPayload, TransportTicket};
use tauri::{Emitter, State};

use crate::commands::browse::accept_manifest_bytes;
use crate::commands::collection::build_slug_manifest;
use crate::error::CmdResult;
use crate::identity_state::SharedIdentity;
use crate::manifest_source::StoreManifestSource;
use crate::net::SharedRelay;
use crate::store::{DataStore, IssuedTicketRecord};
use crate::transport::{fetch_manifest_with_progress, issue_ticket, sanitize_node_addr};
use crate::transport_state::{ensure_endpoint, Role, SharedEndpoint};

fn cmd_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mint a request id: 128 bits of randomness, hex.
///
/// **Not derived from the slug, the asker, or the clock.** A request id is the ticket's primary
/// binding, so a guessable one would let a peer present a ticket for an approval it was never sent —
/// and a derived one collides across two approvals for the same collection, which would make the
/// second silently reuse the first's spent-bit.
fn new_request_id() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

/// **The fulfil verb.** Approve one request for one collection: build the manifest, bind the plane,
/// mint a ticket bound to this approval, persist it, and DM it to the asker.
///
/// Ordering is load-bearing at three points:
///
/// 1. **The manifest is built first**, before the endpoint binds or a ticket exists. Sealing applies
///    the 16 MiB ceiling, so an over-cap collection is refused here — before a promise is made, rather
///    than after the asker has a ticket that can only ever fail.
/// 2. **The record is persisted before the DM.** The reverse order hands a peer a ticket this node
///    cannot authorize, which is indistinguishable from a forgery. An orphaned record — the DM then
///    failed — is inert, because nobody holds the matching ticket.
/// `ask_nonce` is the asker's own value, taken from the request being answered (owner ruling ①,
/// 2026-07-31). It is echoed into the ticket **verbatim and never interpreted** — only the asker can
/// check it, and that check is what stops a peer minting tickets we would auto-dial. `None` when the
/// request predates the field; the asker then refuses to auto-dial, which is the intended outcome.
///
/// 3. **The payload built here is discarded — approval authorizes THE COLLECTION, not a snapshot.**
///    **Owner ruling ② (2026-07-31), ratified explicitly** after a review flagged the consequence:
///    tickets never expire, so an approval given today can deliver a materially newer list months
///    later, including files added since. That is intended. The envelope built here exists only to
///    prove the manifest is producible and within the ceiling; the plane rebuilds it at redeem time
///    from the same pure core, so the asker gets the tree as it is *then*.
///
///    **Do not "fix" this into a snapshot.** Freezing the payload at approval would make an unredeemed
///    ticket serve a stale list forever, which is the failure the staleness gates exist to prevent.
///    If the consent concern is ever revisited, the answer is re-approval on change, not a frozen
///    artifact — and that is an owner ruling, not a refactor.
///
/// **This command is a marshalling shim and must stay one.** Every line of behaviour lives in
/// [`send_full_list_inner`], which takes plain references instead of Tauri `State` so the WAN harness
/// can drive the REAL body rather than a hand-copy of it. That split exists because of a shipped
/// defect: the harness used to re-implement the asker's half step by step, the one step it did not
/// copy was `sanitize_node_addr`, and the feature was broken end to end while 18 QUIC tests stayed
/// green (owner devtest 2026-08-27). `fulfil_commands_stay_thin` pins the shim shape.
#[tauri::command]
pub async fn send_full_list(
    npub: String,
    slug: String,
    ask_nonce: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<()> {
    send_full_list_inner(npub, slug, ask_nonce, &identity, &store, &relay, &endpoint).await
}

/// The whole of `send_full_list`'s behaviour, callable without a Tauri runtime.
pub(crate) async fn send_full_list_inner(
    npub: String,
    slug: String,
    ask_nonce: Option<String>,
    identity: &SharedIdentity,
    store: &DataStore,
    relay: &SharedRelay,
    endpoint: &SharedEndpoint,
) -> CmdResult<()> {
    let recipient = crate::commands::chat::parse_recipient(&npub)?;
    let (id_clone, browse_key, transport_key, own_npub) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone(), id.transport_key.clone(), id.npub())
    };
    if crate::commands::chat::is_self_send(&recipient, &id_clone.public_key()) {
        return Err("You can't send a full list to yourself.".into());
    }

    // QURATOR-45: the fulfil click was silent and could hang — `send_full_list` had ZERO tracing
    // calls and nothing bounded the endpoint bind, so a stuck bind left no log trace and swallowed
    // every subsequent click. These milestones trace every step so a real run produces evidence, and
    // the bind itself is bounded in `ensure_endpoint` (see `ENDPOINT_BIND_TIMEOUT`). npubs are
    // truncated through `trunc_npub` (INV-2); the browse-key, nsec, DM plaintext, and peer/node
    // addresses are NEVER logged. The slug is owner-public (it identifies a collection the peer
    // already browsed and asked about).
    let recipient_npub = crate::commands::chat::npub_of(&recipient);
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        ask_nonce_present = ask_nonce.is_some(),
        "fulfil: send_full_list invoked — building + sealing the manifest"
    );

    // (1) Prove the manifest exists and fits, before anything is promised. `seal` is the ceiling.
    let envelope = build_slug_manifest(&slug, store, &id_clone, browse_key.bytes())?;
    let sealed = ManifestPayload::seal(&envelope).map_err(|e| {
        format!(
            "This collection's full list is too large to send over the connection ({e}). \
             Export it instead: Home → ⋯ → Export, then hand the file over."
        )
    })?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        sealed_bytes = sealed.as_bytes().len(),
        "fulfil: manifest sealed within the transport ceiling"
    );

    let source: Arc<dyn crate::transport::ManifestSource> = StoreManifestSource::new(
        (*store).clone(),
        id_clone.clone(),
        browse_key.clone(),
    );
    // Fulfilling must LISTEN — the asker dials us. The bind is bounded inside `ensure_endpoint`
    // (`ENDPOINT_BIND_TIMEOUT`); a stuck relay handshake now fails loudly instead of hanging.
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        role = "listen",
        "fulfil: binding the transport endpoint"
    );
    let ep = ensure_endpoint(endpoint, &own_npub, identity, &transport_key, source, Role::Listen)
        .await
        .map_err(|e| {
            format!("Could not start the transport ({e}). Export the list instead: Home → ⋯ → Export.")
        })?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        role = "listen",
        "fulfil: transport endpoint bound — minting the ticket"
    );

    let request_id = new_request_id();
    let ticket =
        issue_ticket(&ep, &request_id, &slug, now_secs(), ask_nonce.as_deref()).map_err(cmd_err)?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        request_id = %request_id,
        "fulfil: ticket minted — persisting the issued-ticket record"
    );

    // (2) Record before the DM, so a redeemer always presents a ticket we can recognise.
    // **Canonicalize the recipient before storing it.** `parse_recipient` also accepts a full `hbk`
    // share code, but contacts are keyed by canonical npub and `contact_standing` hashes whatever is
    // stored here. Persisting the raw input meant an `hbk` caller stored a share code, the later
    // lookup missed, standing came back `Unknown`, and a ticket this node had legitimately issued and
    // delivered was refused at redemption.
    let redeemer_npub = crate::commands::chat::npub_of(&recipient);
    store
        .record_issued_ticket(&IssuedTicketRecord {
            ticket: ticket.clone(),
            redeemer_npub: redeemer_npub.clone(),
            consumed_at: None,
            delivered_bytes: None,
            served_fingerprint: None,
        })
        .map_err(cmd_err)?;
    // (2b) The standing-grant record (QURATOR-137 slice 2): this click IS the owner's approval of
    // serving `slug` to this peer, so it lands in the grant map too. Record-only for now — slice 3
    // is what consults it at redeem time. Fails the fulfilment rather than minting a ticket the
    // grant ledger cannot back. `None` is the author: this is the OWNER's own collection, the same
    // meaning `Ticket::author_npub` already gives `None` — one convention, not two.
    store
        .record_standing_grant(&redeemer_npub, None, &slug, now_secs())
        .map_err(cmd_err)?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        request_id = %request_id,
        "fulfil: issued-ticket record persisted — publishing the ticket DM"
    );

    let body = serde_json::to_string(&ticket).map_err(cmd_err)?;
    let own = crate::net::relay_urls(store);
    let client = crate::net::client(&id_clone, store, relay).await.map_err(cmd_err)?;
    crate::commands::chat::send_dm_inner(
        &client,
        &id_clone,
        &recipient,
        &body,
        &own,
        crate::net::RELAY_TIMEOUT,
    )
    .await
    .map_err(cmd_err)?;
    tracing::info!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        request_id = %request_id,
        "fulfil: send_full_list complete — ticket DM delivered"
    );
    Ok(())
}

/// **Carrier 4 (QURATOR-79) — the re-serving half.** Mint a ticket for a CACHED copy of a peer's
/// manifest this node holds, answering a peer's "please send me the full list" ask out of the
/// manifest cache instead of out of this node's own collections.
///
/// The ordering is [`send_full_list`]'s, with one substitution at step (1) and one extra fence:
///
/// 1. **The envelope is read from the cache and its author verified BEFORE anything is promised.**
///    `verify_author` runs here, at MINT time, against the *requested* author — the §2 C-side
///    provenance fence. Without it a D asking for `(author = A, slug = s)` could be served B's
///    same-slug envelope: D's own gate would refuse it (author pin), so this is not a disclosure
///    hole, but C would have spent a ticket on a delivery that can never land. Resolving the cache
///    entry here — once, under the human's "Send cached copy" click — is also what
///    `IssuedTicketRecord.served_fingerprint` records: serve time (`StoreManifestSource::payload`)
///    replays this decision instead of re-guessing "newest for slug".
/// 2. **The record is persisted before the DM**, as ever — a redeemer always presents a ticket this
///    node can recognise, and the reverse order is indistinguishable from a forgery. An orphaned
///    record (the DM then failed) is inert: nobody holds the matching ticket.
///
/// The served envelope is the one already in the cache, so no manifest is built here and the
/// ceiling cannot be exceeded — `ManifestPayload::seal` re-checks it at serve time regardless.
///
/// **A marshalling shim only** — the behaviour is [`send_cached_manifest_inner`], same split as the
/// other two commands, so the harness drives the real body.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn send_cached_manifest(
    npub: String,
    author_npub: String,
    slug: String,
    ask_nonce: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<()> {
    send_cached_manifest_inner(npub, author_npub, slug, ask_nonce, &identity, &store, &relay, &endpoint)
        .await
}

/// The whole of `send_cached_manifest`'s behaviour, callable without a Tauri runtime.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_cached_manifest_inner(
    npub: String,
    author_npub: String,
    slug: String,
    ask_nonce: Option<String>,
    identity: &SharedIdentity,
    store: &DataStore,
    relay: &SharedRelay,
    endpoint: &SharedEndpoint,
) -> CmdResult<()> {
    let recipient = crate::commands::chat::parse_recipient(&npub)?;
    // The requested author: whose collection the asker asked about. This is the npub the envelope's
    // author is pinned against below — never `None`, never "whoever's envelope is handy".
    let expected_author = crate::commands::chat::parse_recipient(&author_npub)
        .map_err(|e| format!("Invalid author: {e}"))?;
    let author_npub = crate::commands::chat::npub_of(&expected_author);
    let (id_clone, browse_key, transport_key, own_npub) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone(), id.transport_key.clone(), id.npub())
    };
    if crate::commands::chat::is_self_send(&recipient, &id_clone.public_key()) {
        return Err("You can't send a cached list to yourself.".into());
    }
    let recipient_npub = crate::commands::chat::npub_of(&recipient);

    // (1) Resolve the cache entry and verify its author, before the endpoint binds or a ticket
    // exists. This scan is the read-only half — the ONLY production writer of the cache is
    // `accept_manifest_bytes` in `commands/browse.rs` (CI sweep).
    let cache_dir = store.manifest_cache_dir();
    let Some((fingerprint, envelope)) = newest_cached_for(&cache_dir, &author_npub, &slug) else {
        return Err(format!(
            "You don't have a cached copy of '{slug}' from that peer, so it can't be re-sent."
        ));
    };
    envelope
        .verify_author(&expected_author)
        .map_err(|e| format!("That cached copy could not be verified as this peer's ({e})."))?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        author = %crate::logging::trunc_npub(&author_npub),
        slug = %slug,
        fingerprint = %fingerprint,
        "fulfil: send_cached_manifest — author verified against the requested peer, minting"
    );

    let source: Arc<dyn crate::transport::ManifestSource> = StoreManifestSource::new(
        (*store).clone(),
        id_clone.clone(),
        browse_key.clone(),
    );
    let ep = ensure_endpoint(endpoint, &own_npub, identity, &transport_key, source, Role::Listen)
        .await
        .map_err(|e| {
            format!("Could not start the transport ({e}). Export the list instead: Home → ⋯ → Export.")
        })?;

    let request_id = new_request_id();
    let mut ticket =
        issue_ticket(&ep, &request_id, &slug, now_secs(), ask_nonce.as_deref()).map_err(cmd_err)?;
    // The carrier-4 mark: `author_npub` names whose collection this re-serves (None = the issuer's
    // own, the `send_full_list` case), and `served_fingerprint` is the exact cache entry resolved
    // above — recorded at MINT time so serve time replays this decision rather than re-resolving.
    ticket.author_npub = Some(author_npub.clone());

    // (2) Record before the DM — same canonicalization note as `send_full_list_inner`.
    let redeemer_npub = crate::commands::chat::npub_of(&recipient);
    store
        .record_issued_ticket(&IssuedTicketRecord {
            ticket: ticket.clone(),
            redeemer_npub: redeemer_npub.clone(),
            consumed_at: None,
            delivered_bytes: None,
            served_fingerprint: Some(fingerprint),
        })
        .map_err(cmd_err)?;
    // (2b) The standing-grant record (QURATOR-137 slice 2), cached-serve side: this click is an
    // owner approval of serving this (author, slug) to this peer, so it lands in the grant map.
    // The author IS part of the key — it names WHICH collection was approved.
    //
    // ⚠ An earlier revision excluded it deliberately, reasoning that "gating on the author would
    // let a re-serve of the same collection under a different author bypass a refusal". That does
    // not hold: the author is half of a collection's identity (the NIP-01 coordinate is
    // `kind:author_pubkey:d-tag`, and the d-tag IS the slug), so "the same collection under a
    // different author" is not a thing. What the omission DID create is real — re-serving A's
    // `films` to D wrote `D|films`, which slice 3 would then have matched when D asked after THIS
    // node's own `films`. Pinned by
    // `a_carrier4_grant_does_not_authorize_this_nodes_own_same_named_collection`.
    store
        .record_standing_grant(&redeemer_npub, Some(&author_npub), &slug, now_secs())
        .map_err(cmd_err)?;
    tracing::debug!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        request_id = %request_id,
        "fulfil: cached re-serve record persisted — publishing the ticket DM"
    );

    let body = serde_json::to_string(&ticket).map_err(cmd_err)?;
    let own = crate::net::relay_urls(store);
    let client = crate::net::client(&id_clone, store, relay).await.map_err(cmd_err)?;
    crate::commands::chat::send_dm_inner(
        &client,
        &id_clone,
        &recipient,
        &body,
        &own,
        crate::net::RELAY_TIMEOUT,
    )
    .await
    .map_err(cmd_err)?;
    tracing::info!(
        recipient = %crate::logging::trunc_npub(&recipient_npub),
        slug = %slug,
        request_id = %request_id,
        "fulfil: send_cached_manifest complete — cached-copy ticket DM delivered"
    );
    Ok(())
}

/// Resolve the newest cache entry for `(author, slug)` by scanning the cache — a `CacheEntry`
/// carries `npub`/`slug`/`fingerprint` in plaintext, so "newest" is a `last_access` max. Returns
/// `(fingerprint, envelope)`. Read-only: this NEVER writes the cache.
fn newest_cached_for(
    dir: &std::path::Path,
    author: &str,
    slug: &str,
) -> Option<(String, hb_core::manifest::ManifestEnvelope)> {
    let mut best: Option<(u64, String, hb_core::manifest::ManifestEnvelope)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(parsed) = serde_json::from_slice::<CacheIndexEntry>(&bytes) else { continue };
        if parsed.npub != author || parsed.slug != slug {
            continue;
        }
        let Ok(env) = hb_core::manifest::ManifestEnvelope::from_json(&parsed.envelope) else {
            continue;
        };
        if best.as_ref().is_none_or(|(t, _, _)| parsed.last_access > *t) {
            best = Some((parsed.last_access, parsed.fingerprint.clone(), env));
        }
    }
    best.map(|(_, fp, env)| (fp, env))
}

/// The plaintext half of `manifest_cache::CacheEntry`, read here for scanning. A private struct
/// twin, not a schema change: the fields are the cache's stable on-disk contract.
#[derive(serde::Deserialize)]
struct CacheIndexEntry {
    npub: String,
    slug: String,
    fingerprint: String,
    envelope: String,
    last_access: u64,
}

/// **The asker's half.** Redeem a ticket that arrived by DM: dial the address it carries, fetch the
/// manifest, and hand it to the M16 W4 gate.
///
/// The gate is [`accept_manifest_bytes`] — the same function the file/paste import calls, not a
/// transport-flavoured copy of it. So the author is still pinned to the browsed peer, the signature
/// still verified before the decrypt, the slug still bound, and completeness still required. The one
/// thing this path adds is `expected_slug` coming from the **ticket** rather than from the UI: the
/// ticket is what the owner signed off on, so it is the stronger statement of what was approved.
///
/// A failure here does **not** spend the ticket — the owner only records a receipt on the asker's
/// acknowledgement, so a dial that never connects can simply be retried.
///
/// **A marshalling shim only** — see [`send_full_list`]'s note. The body is
/// [`redeem_manifest_ticket_emitting`], which owns the Tauri-only progress forwarder and then
/// calls the same body fn [`redeem_manifest_ticket_inner`] does, so the sanitize/claim/fetch/spend
/// sequence the harness used to hand-copy is still the one the app runs.
#[tauri::command]
pub async fn redeem_manifest_ticket(
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    endpoint: State<'_, SharedEndpoint>,
    app: tauri::AppHandle,
) -> CmdResult<crate::commands::browse::ImportedManifest> {
    redeem_manifest_ticket_emitting(app, npub, ticket_json, newest_fingerprint, &identity, &store, &endpoint).await
}

/// The Tauri-only half of [`redeem_manifest_ticket`]: stand up the progress forwarder, then run the
/// same body [`redeem_manifest_ticket_inner`] runs.
///
/// This exists so the `#[tauri::command]` body stays a single delegating call. The forwarder is the
/// `"snapshot-progress"` shape from `lib.rs` — an unbounded channel into a spawned task that emits —
/// and the CHANNEL, never `AppHandle`, is what crosses into the body fn, so the WAN harness's direct
/// call to `_inner` stays Tauri-free. Nothing here decides anything: no guard, transform or ordering
/// rule may move into this function, because the harness cannot run it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn redeem_manifest_ticket_emitting(
    app: tauri::AppHandle,
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: &SharedIdentity,
    store: &DataStore,
    endpoint: &SharedEndpoint,
) -> CmdResult<crate::commands::browse::ImportedManifest> {
    let (prog_tx, mut prog_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::transport::ManifestProgress>();
    tauri::async_runtime::spawn(async move {
        while let Some(p) = prog_rx.recv().await {
            let _ = app.emit(
                "manifest-progress",
                serde_json::json!({
                    "request_id": p.request_id,
                    "slug": p.slug,
                    "received": p.received,
                    "total": p.total,
                }),
            );
        }
    });
    redeem_manifest_ticket_with_progress(
        npub,
        ticket_json,
        newest_fingerprint,
        identity,
        store,
        endpoint,
        Some(&prog_tx),
    )
    .await
}

/// Carrier 4 provenance — the redeem side's answer to "who served this copy?".
///
/// A re-serve is exactly the delivery whose ticket names a third-party author (`author_npub` is
/// `Some`): the DM arrived from peer C while the collection is A's, so the serving peer is the DM
/// sender. A direct serve (`None`, the `send_full_list` case) must stay `None` — the author served
/// it themselves, and the UI's `reServed` branch must not fire. This is the SAME discriminator the
/// claim key in `redeem_manifest_ticket_inner` already resolves from (`expected_author`), not a new
/// signal.
///
/// There is deliberately no companion cache-time value on either path: when C took its copy is a
/// property of C's cache that never crosses the wire, and the envelope's own clock is A's authoring
/// time, not a cache time — surfacing that would render a false date. A `cached_at` field was
/// declared for months on the strength of this reasoning and never had a producer; it was removed
/// in QURATOR-172 #2.
pub(crate) fn carrier4_served_by(ticket: &TransportTicket, dm_sender: &str) -> Option<String> {
    ticket.author_npub.is_some().then(|| dm_sender.to_string())
}

/// The whole of `redeem_manifest_ticket`'s behaviour, callable without a Tauri runtime — the
/// signature the WAN harness drives, unchanged by QURATOR-159.
pub(crate) async fn redeem_manifest_ticket_inner(
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: &SharedIdentity,
    store: &DataStore,
    endpoint: &SharedEndpoint,
) -> CmdResult<crate::commands::browse::ImportedManifest> {
    redeem_manifest_ticket_with_progress(
        npub,
        ticket_json,
        newest_fingerprint,
        identity,
        store,
        endpoint,
        None,
    )
    .await
}

/// [`redeem_manifest_ticket_inner`] plus an optional progress channel: a plain `mpsc` sender, so no
/// `AppHandle` ever reaches this seam. `None` means no progress — the WAN harness's path.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn redeem_manifest_ticket_with_progress(
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: &SharedIdentity,
    store: &DataStore,
    endpoint: &SharedEndpoint,
    progress: Option<&tokio::sync::mpsc::UnboundedSender<crate::transport::ManifestProgress>>,
) -> CmdResult<crate::commands::browse::ImportedManifest> {
    let mut ticket: TransportTicket = serde_json::from_str(&ticket_json)
        .map_err(|_| "That message is not a readable transport ticket.".to_string())?;
    ticket.verify_shape().map_err(cmd_err)?;
    // QURATOR-113 #20 — SSRF guard: the ticket's node_addr is peer-authored (a stranger who answers
    // a manifest ask controls this dial target). Drop any transport address pointing at
    // loopback/private/link-local before the dial; the transport layer's loopback QUIC harness stays
    // unguarded so it keeps exercising the real dial.
    ticket.node_addr = sanitize_node_addr(&ticket.node_addr).map_err(cmd_err)?;

    let (id_clone, browse_key, transport_key, own_npub) = {
        let guard = identity.read().await;
        let id = guard.as_ref().ok_or("No identity loaded. Generate a keypair first.")?;
        (id.identity.clone(), id.browse_key.clone(), id.transport_key.clone(), id.npub())
    };

    // **Dial-only** (owner ruling ③): the asker needs to connect, not to be connectable. Binding a
    // listening endpoint here used to leave this node answering anyone holding its permanently-stable
    // node id — a probeable liveness oracle for every owner it had ever redeemed from, which is
    // exactly what presence carrying no address exists to prevent.
    //
    // The source is still constructed because `ensure_endpoint` may find (or need) a listening
    // binding for this identity; in the `DialOnly` path it is simply never served from.
    let source: Arc<dyn crate::transport::ManifestSource> =
        StoreManifestSource::new((*store).clone(), id_clone, browse_key);
    // **The gate is a durable ATOMIC CLAIM, taken before any dial — not a validation.**
    //
    // Validating and then dialing was a TOCTOU: two concurrent invokes carrying different
    // peer-crafted tickets with the same valid nonce both passed and both connected. And releasing
    // the ask on failure let a peer send ticket after ticket — each with a fresh `request_id` and a
    // fresh node address — getting an automatic dial per attempt with no user action.
    //
    // `claim_manifest_ask` does the whole check-and-claim under one lock and records the winning
    // `request_id` **durably**, so: one ask admits one ticket, a retry must be that same ticket, and
    // a restart cannot resurrect the authorization. This is the security boundary; the ledger in the
    // Chat page is only render-idempotence.
    //
    // **Carrier 4 — the ask is keyed on the AUTHOR, not the sender.** On a re-serve the DM arrives
    // from peer C but answers an ask we recorded about peer A's collection, so the key must be the
    // one the asker recorded: `ticket.author_npub` when present, else the DM sender (`None` means
    // "the issuer's own collection" — exactly today's behaviour). Keying on the sender would make
    // every re-serve claim fall through as `Unsolicited` and fail closed.
    let expected_author = ticket.author_npub.clone().unwrap_or_else(|| npub.clone());
    let claim = store
        .claim_manifest_ask(
            &npub,
            &expected_author,
            &ticket.slug,
            ticket.ask_nonce.as_deref().unwrap_or_default(),
            &ticket.request_id,
        )
        .map_err(cmd_err)?;
    match claim {
        crate::store::AskClaim::Granted => {}
        crate::store::AskClaim::Unsolicited => {
            return Err("That link doesn't answer a request you sent, so nothing was fetched.".into())
        }
        crate::store::AskClaim::Spent => {
            return Err("You've already received this list.".into())
        }
        crate::store::AskClaim::ClaimedByAnother => {
            return Err("Another link is already answering that request, so nothing was fetched.".into())
        }
    }

    let ep = ensure_endpoint(endpoint, &own_npub, identity, &transport_key, source, Role::DialOnly)
        .await
        .map_err(cmd_err)?;

    // The gate runs INSIDE `fetch_manifest`, before the acknowledgement — see its doc comment. If it
    // ran out here the ACK would already have been sent, and a manifest we reject (wrong author,
    // undecryptable, incomplete) would still have burned the ticket, forcing the human to ask again
    // and the owner to approve again.
    let mut imported: Option<crate::commands::browse::ImportedManifest> = None;
    // QURATOR-159: byte progress for the manifest body. `(received, total)` straight off the
    // framing layer; the wrapper's forwarder is what turns these into Tauri events. Every sample
    // names the ticket this redeem is bound to, so the UI can never attribute one bar's bytes to
    // another in-flight redeem.
    let progress_sink = progress.map(|tx| {
        crate::transport::ManifestProgress {
            request_id: ticket.request_id.clone(),
            slug: ticket.slug.clone(),
            received: 0,
            total: 0,
        }
        .sink(tx)
    });
    fetch_manifest_with_progress(&ep, &ticket, |payload| {
        let raw = std::str::from_utf8(payload.as_bytes())
            .map_err(|_| anyhow::anyhow!("the manifest that arrived was not text"))?;
        imported = Some(
            accept_manifest_bytes(
                // Carrier 4: pin the author to the one the ticket names, not the DM sender. On a
                // re-serve D receives A's envelope from C; pinning to C made `open_manifest`'s
                // author check refuse every carrier-4 delivery. This also keys the CACHE by the
                // resolved author, so a re-serve files A's manifest under A (not under C) — the
                // cache write inside `accept_manifest_bytes` uses this same npub.
                &expected_author,
                Some(&ticket.slug),
                raw,
                newest_fingerprint.as_deref(),
                store,
                // The cache IS the delivery on this path — Chat discards the tree and Browse reads it
                // back. A dropped write must fail the gate, so no ACK is sent and the ticket survives.
                true,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
        Ok(())
    }, progress_sink.as_ref().map(|f| f as crate::transport::ProgressSink))
    .await
    .map_err(cmd_err)?;
    let mut imported =
        imported.ok_or_else(|| "The manifest was acknowledged but never accepted.".to_string())?;
    // Carrier 4 provenance: name the peer that actually served the copy when this was a re-serve
    // (the ticket naming a third-party author is the discriminator — see `carrier4_served_by`).
    // `ImportedManifest`'s fields are `pub` and `open_manifest` hardcodes `None` (it has no notion
    // of who carried the bytes; the file/paste import path is by definition a direct serve), so
    // this sets it AT the redeem call site rather than widening `accept_manifest_bytes` for all
    // three of its callers.
    imported.served_by = carrier4_served_by(&ticket, &npub);

    // **Consume the ask now that it has been answered** (owner ruling ①). One ask, one auto-dial:
    // leaving it would restore the standing authorization the nonce exists to remove, since a peer
    // could then send further tickets echoing the same still-stored nonce.
    //
    // AFTER success, never on an attempt — a dial that failed has cost nothing and must stay
    // retryable, exactly like the ticket itself. Best-effort: the manifest is already in hand, and
    // failing the command here would tell the user a delivery they received had failed.
    // Conditional on the nonce this ticket carried, so a re-ask made while this was in flight is not
    // thrown away by our completion.
    //
    // **A failure here does NOT restore the authorization** — the UI holds an in-session spent-marker
    // for the ask, keyed independently of this write, precisely because a disk-full here would
    // otherwise re-open the reusable authorization this whole ruling removes. Logged loudly; the
    // manifest is already in hand, so failing the command would report a delivery the user received
    // as failed.
    // Mark it answered. The durable CLAIM already bounds the damage if this write fails — the ask
    // stays bound to this one `request_id`, so no *new* ticket can take it even across a restart. The
    // residual is a replay of this same ticket dialing the same address once more, which is why the
    // failure is loud rather than silent.
    if let Err(e) = store.spend_manifest_ask(
        &npub,
        &expected_author,
        &ticket.slug,
        ticket.ask_nonce.as_deref().unwrap_or_default(),
    ) {
        tracing::error!(
            "could not mark the manifest ask spent after redemption; it stays bound to this ticket, \
             but a replay of this exact ticket could dial once more: {e}"
        );
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request id must be unguessable and unique per approval — it is the ticket's primary binding,
    /// so a collision would make one approval's spent-bit govern another's.
    #[test]
    fn request_ids_are_unique_and_128_bit() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32, "16 bytes, hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A ticket DM that is not a ticket must be refused as unreadable, not mis-parsed into a ticket
    /// with empty bindings — `verify_shape` is the second line, but serde is the first.
    #[test]
    fn a_non_ticket_body_is_not_a_ticket() {
        assert!(serde_json::from_str::<TransportTicket>(r#"{"hb":"manifest_request"}"#).is_err());
        // A structurally complete body with the WRONG discriminator parses, then fails the shape
        // check — which is how a `manifest_request` can never be redeemed as a ticket.
        let wrong = serde_json::json!({
            "hb": "manifest_request",
            "ticket_v": 1,
            "request_id": "r",
            "slug": "s",
            "node_addr": "a",
            "issued_at": 1
        });
        let t: TransportTicket = serde_json::from_value(wrong).unwrap();
        assert!(t.verify_shape().is_err(), "the discriminator is checked, not assumed");
    }
    /// **The structural repair for the 2026-08-27 devtest failure, pinned.**
    ///
    /// The bug itself was a byte comparison against a value production transforms. The reason it
    /// SHIPPED was different and worse: the WAN harness re-implemented `redeem_manifest_ticket`'s
    /// body step by step instead of calling it, and the one step it did not copy was the transform.
    /// Both commands are now marshalling shims over `*_inner` functions the harness calls directly,
    /// so no step can exist in production without also being on the tested path.
    ///
    /// This guard keeps them shims. If someone adds real logic to a `#[tauri::command]` body here,
    /// the harness stops covering it and the blind spot returns — so the body must stay a single
    /// delegating call. Scanned with comments stripped, so documenting the rule cannot satisfy it.
    #[test]
    fn fulfil_commands_stay_thin_so_the_harness_keeps_covering_them() {
        let src = include_str!("fulfil.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        for (cmd, inner) in
            [("send_full_list", "send_full_list_inner"), ("redeem_manifest_ticket", "redeem_manifest_ticket_emitting"), ("send_cached_manifest", "send_cached_manifest_inner")]
        {
            let sig = format!("pub async fn {cmd}(");
            let at = code.find(&sig).unwrap_or_else(|| panic!("{cmd} not found"));
            let inner_sig = format!("async fn {inner}(");
            let inner_at = code[at..]
                .find(&inner_sig)
                .map(|i| at + i)
                .unwrap_or_else(|| panic!("{cmd}'s inner fn {inner} must follow it"));
            // Everything between the command's signature and its inner fn: the parameter list plus
            // the body. Only the body can contain statements, so counting `;` here is safe.
            let region = &code[at..inner_at];
            let brace = region
                .find('{')
                .unwrap_or_else(|| panic!("{cmd} has no body"));
            let body = &region[brace..];

            assert!(
                body.contains(&format!("{inner}(")),
                "{cmd} must delegate to {inner}"
            );
            // A shim marshals and calls. Anything else — a guard, a transform, an ordering rule —
            // belongs in the inner fn where the harness will actually run it.
            for banned in ["if ", "match ", "for ", "while ", "?;", ".await?"] {
                assert!(
                    !body.contains(banned),
                    "{cmd} grew logic ({banned:?}) in its #[tauri::command] body. Move it into \
                     {inner}: the WAN harness calls the inner fn, so anything here is untested — \
                     which is exactly how the sanitize_node_addr defect shipped."
                );
            }
            let stmts = body.matches(';').count();
            assert!(
                stmts <= 1,
                "{cmd}'s command body should be one delegating call, found {stmts} statements"
            );
        }

        // QURATOR-159: `redeem_manifest_ticket` delegates to `_emitting` rather than straight to
        // `_inner`, because the progress forwarder needs an `AppHandle` the harness cannot supply.
        // That indirection is only safe while BOTH paths rejoin the SAME body — the production one
        // through `_emitting`, the harness one through `_inner`. Assert the convergence, or the two
        // can drift apart and the harness silently stops covering what ships, which is precisely the
        // failure this guard exists to prevent.
        for f in ["redeem_manifest_ticket_emitting", "redeem_manifest_ticket_inner"] {
            let sig = format!("async fn {f}(");
            let at = code.find(&sig).unwrap_or_else(|| panic!("{f} not found"));
            let end = code[at..].find("\n}").expect("fn must terminate") + at;
            assert!(
                code[at..end].contains("redeem_manifest_ticket_with_progress("),
                "{f} must converge on redeem_manifest_ticket_with_progress, so the harness path and \
                 the production path run one body and cannot drift apart"
            );
        }
    }

    // ── QURATOR-161 slice 4 — the commands' guards, driven through the real bodies ────────────────
    //
    // Both `#[tauri::command]` fns are marshalling shims over the `*_inner` fns in this file, so the
    // guards are pinned by calling the INNER fns — the same bodies production runs and the same seam
    // the WAN harness uses. There is no second copy of any guard here to drift.
    //
    // Hermetic by construction, on both sides. The error sides refuse at the guard itself. The
    // pass sides use either a ticket whose address set carries NOTHING dialable (empty after
    // sanitizing — the connect has nothing to try), or an endpoint pre-fenced with a generation
    // bump so the bind fails instantly, or a claim that refuses (`Unsolicited`) before any network
    // step. Every call resolves in milliseconds inside CALL_BUDGET; a hang is reported as a
    // failure, not waited out. "Past the guard, fails at the next statement" without a second copy
    // of any guard's own error.
    mod command_guards {
        use super::*;
        use crate::identity_state::AppIdentity;
        use crate::store::DataStore;
        use crate::transport_state::new_shared_endpoint;

        /// Upper bound on any single command call in this module. Every call either refuses at a
        /// guard (microseconds) or fails at a dial against an address set that carries nothing
        /// dialable — neither path is allowed to reach this ceiling.
        const CALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

        /// A ticket whose node address carries an endpoint id but NO transport addresses. It parses,
        /// passes `verify_shape`, and sanitizes to itself (nothing to drop), so a redeem carrying it
        /// clears every guard, takes the claim, and fails at the connect — which has nothing to dial
        /// and no relay/discovery to ask. This is how the tests get "past the guard, fails at the
        /// next statement" WITHOUT emitting a packet: the address set is empty, not merely private.
        fn undialable_ticket(request_id: &str, slug: &str, nonce: Option<&str>) -> String {
            let ticket = TransportTicket::issue(
                request_id,
                slug,
                &crate::transport::tests::undialable_addr_json(),
                1_700_000_000,
                nonce,
            );
            serde_json::to_string(&ticket).unwrap()
        }

        fn fixture() -> (tempfile::TempDir, DataStore, SharedIdentity, SharedEndpoint) {
            // Mirror the production startup step: `lib.rs` installs the process-level rustls
            // CryptoProvider before any network task, and the WAN harness (`wan_it/mod.rs`) repeats
            // it for the same reason. The lock resolves rustls with BOTH providers compiled in, so
            // it cannot auto-pick one and any bind panics without this. `install_default` returns
            // `Err` once a provider is set, so this is safe to call from every test.
            let _ = rustls::crypto::ring::default_provider().install_default();
            let dir = tempfile::tempdir().unwrap();
            let store = DataStore::new(dir.path().to_path_buf());
            let identity: SharedIdentity =
                std::sync::Arc::new(tokio::sync::RwLock::new(Some(AppIdentity::generate())));
            (dir, store, identity, new_shared_endpoint())
        }

        async fn redeem(
            npub: &str,
            ticket_json: String,
            identity: &SharedIdentity,
            store: &DataStore,
            endpoint: &SharedEndpoint,
        ) -> CmdResult<crate::commands::browse::ImportedManifest> {
            tokio::time::timeout(
                CALL_BUDGET,
                redeem_manifest_ticket_inner(
                    npub.to_string(),
                    ticket_json,
                    None,
                    identity,
                    store,
                    endpoint,
                ),
            )
            .await
            .expect("the call must resolve well inside the budget — a hang is a finding")
            // The outer timeout error is infallible here (we expect the inner call to fail).
        }

        /// (a): an unparseable DM body is refused as unreadable — the first line of defence, before
        /// the shape check, the SSRF rewrite, the identity read, and the claim. Positioning is
        /// proven by the empty identity slot: if the parse guard moved below the identity read this
        /// would flip to "No identity loaded" and red.
        #[tokio::test]
        async fn redeem_refuses_an_unparseable_ticket_body() {
            let (_dir, store, _, endpoint) = fixture();
            let identity: SharedIdentity =
                std::sync::Arc::new(tokio::sync::RwLock::new(None));
            let err = redeem(
                "npub1peer",
                "{\"hb\":\"manifest_request\"}".into(),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("a non-ticket body must be refused");
            assert_eq!(err, "That message is not a readable transport ticket.");
        }

        /// (d): with the identity slot EMPTY, a ticket that parses, passes the shape check, and
        /// sanitizes to an empty address set is refused for the identity alone.
        #[tokio::test]
        async fn redeem_refuses_when_no_identity_is_loaded() {
            let (_dir, store, _, endpoint) = fixture();
            let identity: SharedIdentity =
                std::sync::Arc::new(tokio::sync::RwLock::new(None));
            let err = redeem(
                "npub1peer",
                undialable_ticket("req-1", "vault", Some("nonce-1")),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("no identity must be refused before any manifest work");
            assert_eq!(err, "No identity loaded. Generate a keypair first.");
        }

        // (c) QURATOR-113 #20 — the SSRF rewrite: OWED, not pinnable from this command.
        //
        // A difference-based test was written here and removed 2026-08-30: it cannot discriminate.
        // `sanitize_node_addr` is TOTAL — it drops non-global addresses, keeps the node id, and
        // returns `Ok` (see its own two unit tests in `transport.rs`,
        // `sanitize_node_addr_drops_non_global_transport_addrs` and
        // `..._keeps_the_id_when_every_address_is_dropped`). It never returns `Err`, so it never
        // short-circuits `redeem_manifest_ticket_inner`. An internal-only ticket and a public one
        // therefore take the SAME path through every remaining guard and produce the SAME error
        // string: with an empty identity slot both stop at "No identity loaded"; with a loaded one
        // both stop at the claim gate's `Unsolicited`. The rewrite's effect first becomes
        // observable at the DIAL, which is downstream of the claim — so reaching it needs a
        // recorded ask AND a real connect attempt, i.e. a packet, which this file's no-network rule
        // forbids.
        //
        // Reported as OWED per the slice's rule ("if a guard cannot be reached from a test without
        // restructuring the command, it is reported as OWED, not refactored into reach"). The
        // function itself IS pinned at unit level; what stays unpinned is its CALL POSITION in this
        // command body — which is exactly the step the WAN harness once failed to copy. Pinning
        // that needs `sanitize_node_addr` to become observable (e.g. returning the dropped count),
        // which is a production change.

        /// (b) `verify_shape`: a structurally complete body with the WRONG discriminator parses and
        /// is then refused by the shape check, before the SSRF rewrite and the identity read (empty
        /// identity slot, same positioning proof).
        #[tokio::test]
        async fn redeem_refuses_a_wrong_discriminator_ticket() {
            let (_dir, store, _, endpoint) = fixture();
            let identity: SharedIdentity =
                std::sync::Arc::new(tokio::sync::RwLock::new(None));
            let wrong = serde_json::json!({
                "hb": "manifest_request",
                "ticket_v": 1,
                "request_id": "req-1",
                "slug": "vault",
                "node_addr": "a",
                "issued_at": 1,
                "ask_nonce": "nonce-1",
            });
            let err = redeem(
                "npub1peer",
                serde_json::to_string(&wrong).unwrap(),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("a wrong-discriminator body must be refused before the identity read");
            assert_eq!(err, "invalid transport ticket: not a transport ticket");
        }

        /// The pass side of (a)-(d): a parseable, well-shaped, sanitized ticket with an identity
        /// loaded clears every guard and fails at the claim — `Unsolicited`, because no ask was
        /// recorded. That is the statement AFTER all four guards, so the guards are discriminated:
        /// this call is what the error-side tests would look like if any guard silently stopped
        /// firing. No network is touched — the claim refusal precedes `ensure_endpoint`.
        #[tokio::test]
        async fn redeem_clears_the_early_guards_and_fails_at_the_claim() {
            let (_dir, store, identity, endpoint) = fixture();
            let err = redeem(
                "npub1peer",
                undialable_ticket("req-1", "vault", Some("nonce-1")),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("no recorded ask means Unsolicited, not a guard refusal");
            assert_eq!(err, "That link doesn't answer a request you sent, so nothing was fetched.");
        }

        /// (e) — the durable atomic claim, the file's highest-value guard. One ask admits exactly
        /// one ticket: a second redemption carrying a DIFFERENT `request_id` but the same otherwise-
        /// valid nonce is refused with `ClaimedByAnother`, and the ask's `claimed_by` on disk stays
        /// the first ticket's id. A retry of the SAME ticket is re-granted (the documented retry
        /// rule), which is the discriminating pass side: the gate is keyed on `request_id`
        /// disagreement, not on a blanket "already claimed". Every call uses an address set with
        /// nothing dialable, so a granted claim fails at the connect inside the budget — no network.
        #[tokio::test]
        async fn one_ask_admits_one_ticket() {
            let (_dir, store, identity, endpoint) = fixture();
            let npub = "npub1peer";
            let slug = "vault";
            let nonce = "nonce-1";
            store
                .record_manifest_ask(npub, npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
                .unwrap();

            // First ticket: GRANTED the claim, then fails at the connect (nothing dialable) — the
            // ask is claimed but NOT spent, per the documented failed-dial rule.
            let first = redeem(
                npub,
                undialable_ticket("req-A", slug, Some(nonce)),
                &identity,
                &store,
                &endpoint,
            )
            .await;
            assert!(first.is_err(), "an undialable address can never deliver a manifest");
            assert!(
                !first.as_ref().unwrap_err().contains("already answering"),
                "the first claim must be Granted, not refused: {first:?}"
            );
            let ask = store.load_manifest_asks().unwrap().get(&format!("{npub}|{npub}|{slug}")).unwrap().clone();
            assert_eq!(ask.claimed_by.as_deref(), Some("req-A"), "the claim is durable on disk");
            assert!(!ask.spent, "a failed dial must not spend the ask");

            // A different ticket for the same ask: refused, and the claim on disk does not move.
            let err = redeem(
                npub,
                undialable_ticket("req-B", slug, Some(nonce)),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("a second, different ticket must not take the same ask");
            assert_eq!(err, "Another link is already answering that request, so nothing was fetched.");
            let ask = store.load_manifest_asks().unwrap().get(&format!("{npub}|{npub}|{slug}")).unwrap().clone();
            assert_eq!(ask.claimed_by.as_deref(), Some("req-A"), "a refused claim does not steal the ask");

            // The SAME ticket retries: granted again, proceeds to the connect. This is the pass
            // side that discriminates "one ask one ticket" from "one ask one attempt".
            let retry_err = redeem(
                npub,
                undialable_ticket("req-A", slug, Some(nonce)),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("the retry still cannot dial");
            assert_ne!(
                retry_err, "Another link is already answering that request, so nothing was fetched.",
                "a same-ticket retry is re-granted, not refused as ClaimedByAnother"
            );
        }

        /// (e), the spent half: after `spend_manifest_ask` (which production calls on SUCCESS), a
        /// replay of the ticket is refused with the spent message — the ask is consumed, not merely
        /// claimed.
        #[tokio::test]
        async fn a_spent_ask_refuses_even_the_same_ticket() {
            let (_dir, store, identity, endpoint) = fixture();
            let npub = "npub1peer";
            let slug = "vault";
            let nonce = "nonce-1";
            store
                .record_manifest_ask(npub, npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
                .unwrap();
            // Claim, then spend — the exact sequence a successful redemption leaves behind.
            store.claim_manifest_ask(npub, npub, slug, nonce, "req-A").unwrap();
            store.spend_manifest_ask(npub, npub, slug, nonce).unwrap();

            let err = redeem(
                npub,
                undialable_ticket("req-A", slug, Some(nonce)),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("a spent ask must refuse even the ticket that spent it");
            assert_eq!(err, "You've already received this list.");
        }

        // ── Carrier 4 (QURATOR-79) — the redeem side's AUTHOR resolution ─────────────────────────
        //
        // Both tests are hermetic the same way `one_ask_admits_one_ticket` is: the ticket's address
        // set carries nothing dialable, so a claim that is Granted proceeds to a connect that fails
        // locally — no packet. What discriminates is WHICH key the claim resolved: an ask recorded
        // for author A answers a ticket naming A (Granted → a dial error, not the claim refusal),
        // and only that.

        /// A re-serve redeem resolves the expected author from `ticket.author_npub`, not the DM
        /// sender. The ask was recorded for `(sender C, author A, slug)` — the key the asker's side
        /// writes via `build_manifest_request_for_author`. Keying the claim on the sender alone (the
        /// pre-Carrier-4 shape) resolves `(C, C, slug)`, misses the ask, and every carrier-4
        /// delivery fails closed as `Unsolicited`.
        ///
        /// MUTATION (P-10) — resolved by containing function: inside
        /// `redeem_manifest_ticket_inner`, change
        /// `let expected_author = ticket.author_npub.clone().unwrap_or_else(|| npub.clone());`
        /// to `let expected_author = npub.clone();` → the claim resolves `(C, C, slug)`, misses the
        /// recorded ask, and the `Unsolicited` assert below reds.
        #[tokio::test]
        async fn a_re_serve_redeem_resolves_the_author_from_the_ticket() {
            let (_dir, store, identity, endpoint) = fixture();
            let sender = "npub1server"; // peer C, who re-serves from its cache
            let author = "npub1author"; // peer A, whose collection it is
            let slug = "vault";
            let nonce = "nonce-1";
            // The asker's record: asked C for A's collection.
            store
                .record_manifest_ask(sender, author, slug, "fp", "2026-01-01T00:00:00Z", nonce)
                .unwrap();

            // A carrier-4 ticket: same shape the mint side stamps (`ticket.author_npub = Some(A)`).
            let mut ticket: TransportTicket =
                serde_json::from_str(&undialable_ticket("req-A", slug, Some(nonce))).unwrap();
            ticket.author_npub = Some(author.to_string());
            let err = redeem(
                sender,
                serde_json::to_string(&ticket).unwrap(),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("the undialable address can never deliver — but the CLAIM must be Granted");
            assert_ne!(
                err,
                "That link doesn't answer a request you sent, so nothing was fetched.",
                "the claim must resolve the author from the ticket, not the sender — got: {err}"
            );
            assert_ne!(
                err,
                "Another link is already answering that request, so nothing was fetched.",
                "the claim must be Granted on the first ticket — got: {err}"
            );
            // And the claim was taken under the AUTHOR-scoped key, durably.
            let asks = store.load_manifest_asks().unwrap();
            let ask = asks.get(&format!("{sender}|{author}|{slug}")).unwrap();
            assert_eq!(ask.claimed_by.as_deref(), Some("req-A"), "the claim landed on the author-scoped key");
        }

        /// The negative half of the boundary: an ask recorded for author A must NOT claim under a
        /// key scoped to a different author. A ticket from C naming a DIFFERENT author B — the
        /// cross-tenant collision shape — is `Unsolicited`, and nothing on disk moves.
        ///
        /// MUTATION (P-10) — resolved by containing function: inside `claim_manifest_ask`
        /// (`store.rs`), replace the exact `manifest_ask_key(npub, author, slug)` lookup with an
        /// author-BLIND fallback that matches any stored key sharing `(npub, slug)` (ignoring the
        /// middle segment) → the ticket's claim resolves Granted on the asked-author's entry and the
        /// `Unsolicited` assert below reds. (Verified: dropping the author from `manifest_ask_key`
        /// alone does NOT redden this test — the lenient legacy widening then rewrites the collapsed
        /// key back to the self spelling, so the ask still misses; that mutation reds the sibling
        /// `a_re_serve_redeem_resolves_the_author_from_the_ticket` instead.)
        #[tokio::test]
        async fn a_re_serve_ask_does_not_claim_under_a_different_author() {
            let (_dir, store, identity, endpoint) = fixture();
            let sender = "npub1server";
            let asked_author = "npub1author";
            let other_author = "npub1other";
            let slug = "vault";
            let nonce = "nonce-1";
            store
                .record_manifest_ask(sender, asked_author, slug, "fp", "2026-01-01T00:00:00Z", nonce)
                .unwrap();

            // A ticket naming a DIFFERENT author than the one we asked about.
            let mut ticket: TransportTicket =
                serde_json::from_str(&undialable_ticket("req-X", slug, Some(nonce))).unwrap();
            ticket.author_npub = Some(other_author.to_string());
            let err = redeem(
                sender,
                serde_json::to_string(&ticket).unwrap(),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("a ticket for a different author's collection answers no ask of ours");
            assert_eq!(
                err,
                "That link doesn't answer a request you sent, so nothing was fetched.",
                "the author is part of the ask's identity — got: {err}"
            );
            // Nothing was claimed on any key for this sender/slug.
            let asks = store.load_manifest_asks().unwrap();
            assert!(
                asks.values().all(|a| a.claimed_by.is_none()),
                "a refused claim must not touch any ask: {asks:?}"
            );
        }

        // ── Carrier 4 (QURATOR-79) — the redeem side's SERVED-BY provenance ──────────────────────
        //
        // The two halves of the discriminator. `carrier4_served_by` is the function the production
        // redeem path calls on the returned `ImportedManifest` — there is no second copy of the
        // discriminator here to drift (the §9 P-6 shape: a guard that re-emits its own copy of the
        // thing it checks). The call-site wiring itself cannot be reached from a unit test: the
        // success stage sits behind `fetch_manifest`'s real dial, and `sanitize_node_addr` strips the
        // loopback address a hermetic endpoint could offer — the same documented OWED shape as the
        // SSRF rewrite note above. What pins the wiring against drifting off the call site is the
        // `redeem_sets_served_by_through_the_shared_discriminator` test below.

        /// The re-serve half: a ticket naming a third-party author yields `served_by == Some(C)`,
        /// where C is the DM sender — the peer whose cached copy arrived.
        ///
        /// MUTATION (P-10) — resolved by containing function: inside `carrier4_served_by`, change
        /// `ticket.author_npub.is_some().then(|| dm_sender.to_string())` to
        /// `Some(dm_sender.to_string())` (stop consulting the ticket) → this test STAYS green
        /// (the value is the same); the SIBLING test `a_direct_serve_stays_served_by_none` is the
        /// one that reds. The mutation that reds THIS test is the inverse, below.
        #[test]
        fn a_re_serve_names_the_serving_peer() {
            let mut ticket: TransportTicket =
                serde_json::from_str(&undialable_ticket("req-A", "vault", Some("nonce-1"))).unwrap();
            ticket.author_npub = Some("npub1author".to_string());
            assert_eq!(
                carrier4_served_by(&ticket, "npub1server"),
                Some("npub1server".to_string()),
                "a re-serve names the DM sender (peer C), whose cached copy arrived"
            );
        }

        /// The direct-serve half — LOAD-BEARING: the shape that must not regress, and the one that
        /// distinguishes this fix from unconditionally setting the field. A ticket with NO
        /// `author_npub` (the author served it themselves) yields `served_by == None`, so the UI's
        /// `reServed` branch stays false and the plain "Full manifest imported" copy keeps firing.
        ///
        /// MUTATION (P-10) — resolved by containing function: inside `carrier4_served_by`, change
        /// `ticket.author_npub.is_some().then(|| dm_sender.to_string())` to
        /// `Some(dm_sender.to_string())` (stop consulting the ticket) → THIS test reds; the sibling
        /// `a_re_serve_names_the_serving_peer` stays green. The pair together discriminates
        /// "consults the ticket" from "always Some".
        #[test]
        fn a_direct_serve_stays_served_by_none() {
            let ticket: TransportTicket =
                serde_json::from_str(&undialable_ticket("req-A", "vault", Some("nonce-1"))).unwrap();
            assert_eq!(
                ticket.author_npub, None,
                "fixture premise: a direct-serve ticket carries no author_npub"
            );
            assert_eq!(
                carrier4_served_by(&ticket, "npub1author"),
                None,
                "a direct serve must not name a serving peer — the author served it themselves"
            );
        }

        /// The wiring did not drift off the call site: the redeem path sets `served_by` by CALLING
        /// the shared discriminator, not by re-deriving (or dropping) it. This is the drift guard
        /// that keeps the two tests above attached to production — without it they pin a function
        /// nothing calls, which is exactly how the field was decorative before this fix.
        ///
        /// MUTATION (P-10) — resolved by containing function: inside
        /// `redeem_manifest_ticket_inner`, delete the statement
        /// `imported.served_by = carrier4_served_by(&ticket, &npub);` → this test reds while both
        /// sibling tests stay green (they test the function directly).
        #[test]
        fn redeem_sets_served_by_through_the_shared_discriminator() {
            let src = include_str!("fulfil.rs");
            // Comments stripped, so documenting the rule cannot satisfy it.
            let code: String = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            // Resolve by containing function, never by matching text anywhere. And bound the
            // region at `#[cfg(test)]`: this test's own assertion literal echoes the assignment it
            // checks, so an unbounded to-EOF scan would be satisfied by its own copy — the exact
            // P-6 shape the guard exists to prevent.
            let sig = "async fn redeem_manifest_ticket_inner(";
            let at = code.find(sig).expect("the inner fn must exist");
            let end = code[at..]
                .find("#[cfg(test)]")
                .expect("the test module must follow the inner fn")
                + at;
            let body = &code[at..end];
            assert!(
                body.contains("imported.served_by = carrier4_served_by(&ticket, &npub);"),
                "the redeem path must set served_by through the shared `carrier4_served_by` \
                 discriminator — an inline re-derivation or a dropped assignment is the P-6 \
                 lookalike shape this field was decorative under"
            );
        }

        // ── send_full_list — the owner side's early guards ──────────────────────────────────────
        //
        // Every guard before the endpoint bind is reachable without touching the network:
        //   - a bad recipient code,
        //   - no identity loaded,
        //   - a self-send,
        //   - a missing draft / invalid slug (via `build_slug_manifest`).
        // Each error-side call is paired with a pass-side call that clears that guard and fails at
        // the NEXT statement, so every guard is discriminated on both sides.

        async fn send(
            npub: &str,
            slug: &str,
            identity: &SharedIdentity,
            store: &DataStore,
            endpoint: &SharedEndpoint,
        ) -> CmdResult<()> {
            let relay: SharedRelay = std::sync::Arc::new(tokio::sync::RwLock::new(None));
            tokio::time::timeout(
                CALL_BUDGET,
                send_full_list_inner(
                    npub.to_string(),
                    slug.to_string(),
                    Some("nonce-1".into()),
                    identity,
                    store,
                    &relay,
                    endpoint,
                ),
            )
            .await
            .expect("the call must resolve well inside the budget — a hang is a finding")
        }

        #[tokio::test]
        async fn send_refuses_an_invalid_recipient_before_anything_else() {
            let (_dir, store, identity, endpoint) = fixture();
            let err = send("not-a-code", "vault", &identity, &store, &endpoint)
                .await
                .expect_err("an unparseable recipient must be refused");
            assert!(err.starts_with("Invalid recipient:"), "{err}");
        }

        #[tokio::test]
        async fn send_refuses_when_no_identity_is_loaded() {
            let (_dir, store, _, endpoint) = fixture();
            let identity: SharedIdentity =
                std::sync::Arc::new(tokio::sync::RwLock::new(None));
            let peer = crate::identity_state::AppIdentity::generate();
            let err = send(&peer.npub(), "vault", &identity, &store, &endpoint)
                .await
                .expect_err("no identity must be refused before any manifest work");
            assert_eq!(err, "No identity loaded. Generate a keypair first.");
        }

        #[tokio::test]
        async fn send_refuses_a_self_send() {
            let (_dir, store, identity, endpoint) = fixture();
            let own_npub = identity.read().await.as_ref().unwrap().npub();
            let err = send(&own_npub, "vault", &identity, &store, &endpoint)
                .await
                .expect_err("a self-send must be refused (devtest #14)");
            assert_eq!(err, "You can't send a full list to yourself.");
        }

        #[tokio::test]
        async fn send_refuses_a_slug_with_no_draft() {
            let (_dir, store, identity, endpoint) = fixture();
            let peer = crate::identity_state::AppIdentity::generate();
            let err = send(&peer.npub(), "vault", &identity, &store, &endpoint)
                .await
                .expect_err("a slug with no draft must be refused");
            assert_eq!(err, "No draft found for collection 'vault'");
        }

        // The pass side for the whole early chain: OWED, not hermetic in this environment.
        //
        // A `send_clears_the_early_guards_and_fails_at_the_endpoint_bind` test was written here and
        // removed 2026-08-30. Its premise was that pre-binding a CLOSED dial-only endpoint and then
        // bumping the generation (the `close_plane` fence) would make `ensure_endpoint`'s rebind
        // fail in milliseconds, letting the call prove it had cleared every guard by dying at the
        // bind with "Could not start the transport". It does not: with the rustls CryptoProvider
        // installed (as production installs it), the rebind does NOT short-circuit on the fence —
        // it proceeds into real transport setup and the call exceeded CALL_BUDGET, tripping the
        // helper's own "a hang is a finding" guard. Before the provider was installed the same test
        // masked this by panicking inside rustls instead, which is the documented shape: a harness
        // that skips a subsystem also skips that subsystem's side effects.
        //
        // Reported as OWED per the slice's rule. Reaching the statement after the last guard needs
        // either a real bind (a packet, which this file's no-network rule forbids) or a production
        // seam that makes the rebind refuse synchronously — a production change a tests-only slice
        // must not make. The four REFUSAL guards above (recipient, identity, self-send, draft) are
        // each pinned on the real command; what stays unpinned is only the pass-through.

    }
}
