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
use tauri::State;

use crate::commands::browse::accept_manifest_bytes;
use crate::commands::collection::build_slug_manifest;
use crate::error::CmdResult;
use crate::identity_state::SharedIdentity;
use crate::manifest_source::StoreManifestSource;
use crate::net::SharedRelay;
use crate::store::{DataStore, IssuedTicketRecord};
use crate::transport::{fetch_manifest, issue_ticket, sanitize_node_addr};
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
///    the 8 MiB ceiling, so an over-cap collection is refused here — before a promise is made, rather
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
            redeemer_npub,
            consumed_at: None,
            delivered_bytes: None,
        })
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
/// [`redeem_manifest_ticket_inner`], which the WAN harness calls directly, so the sanitize/claim/
/// fetch/spend sequence the harness used to hand-copy is now the one the app runs.
#[tauri::command]
pub async fn redeem_manifest_ticket(
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    endpoint: State<'_, SharedEndpoint>,
) -> CmdResult<crate::commands::browse::ImportedManifest> {
    redeem_manifest_ticket_inner(npub, ticket_json, newest_fingerprint, &identity, &store, &endpoint)
        .await
}

/// The whole of `redeem_manifest_ticket`'s behaviour, callable without a Tauri runtime.
pub(crate) async fn redeem_manifest_ticket_inner(
    npub: String,
    ticket_json: String,
    newest_fingerprint: Option<String>,
    identity: &SharedIdentity,
    store: &DataStore,
    endpoint: &SharedEndpoint,
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
    let claim = store
        .claim_manifest_ask(
            &npub,
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
            return Err("You've already received this list. Ask again if you want a fresh copy.".into())
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
    fetch_manifest(&ep, &ticket, |payload| {
        let raw = std::str::from_utf8(payload.as_bytes())
            .map_err(|_| anyhow::anyhow!("the manifest that arrived was not text"))?;
        imported = Some(
            accept_manifest_bytes(
                &npub,
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
    })
    .await
    .map_err(cmd_err)?;
    let imported =
        imported.ok_or_else(|| "The manifest was acknowledged but never accepted.".to_string())?;

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
            [("send_full_list", "send_full_list_inner"), ("redeem_manifest_ticket", "redeem_manifest_ticket_inner")]
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
                .record_manifest_ask(npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
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
            let ask = store.load_manifest_asks().unwrap().get(&format!("{npub}|{slug}")).unwrap().clone();
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
            let ask = store.load_manifest_asks().unwrap().get(&format!("{npub}|{slug}")).unwrap().clone();
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
                .record_manifest_ask(npub, slug, "fp", "2026-01-01T00:00:00Z", nonce)
                .unwrap();
            // Claim, then spend — the exact sequence a successful redemption leaves behind.
            store.claim_manifest_ask(npub, slug, nonce, "req-A").unwrap();
            store.spend_manifest_ask(npub, slug, nonce).unwrap();

            let err = redeem(
                npub,
                undialable_ticket("req-A", slug, Some(nonce)),
                &identity,
                &store,
                &endpoint,
            )
            .await
            .expect_err("a spent ask must refuse even the ticket that spent it");
            assert_eq!(err, "You've already received this list. Ask again if you want a fresh copy.");
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
