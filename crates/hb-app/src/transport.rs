//! **The manifest plane** (M18 W1) — the transport that carries manifests and nothing else.
//!
//! > **INV-4′ — Hoardbook moves no collection files.** A transport plane exists and is structurally
//! > limited to manifest payloads.
//!
//! This module is the plane INV-4′ describes. It is **not** the retired `transfer.rs` recovered:
//! that module was a file mover (a download registry, a throttled copy, an allow-listed path, a
//! `tokio::fs::File::open`), and recovering it would trip INV-4′ mechanism 3 by construction. What
//! was worth recovering is the *connection discipline* — the endpoint idioms and, above all,
//! [`crate::conn::drain_connection`] — not the payload half. The plane below moves exactly one kind
//! of thing, because [`ManifestPayload`] is the only type its signatures can hold.
//!
//! **What this deliberately does not contain, and must never gain:** a `Path`, a `File`, a
//! `tokio::fs` call, a download queue, a `request_download`/`cancel_download` verb, or a resume
//! offset. Those are the constructs the rewritten CI sweep greps for *inside this module*
//! (mechanism 3, `ci.yml`) — the guard replaced the old "no iroh dep" probes, which M18 made red by
//! construction and which would otherwise have been deleted rather than replaced (INVARIANT_AUDIT
//! §6b).
//!
//! ## Protocol — `/hoardbook/manifest/1`
//!
//! ```text
//! asker → owner   [u32-LE frame-len][JSON FetchRequest { request_id, ticket }]
//! owner → asker   [u8 status: 0 = ok, 1 = refused]
//!   ok      → [u32-LE payload-len][ManifestEnvelope JSON bytes]
//!   refused → [u32-LE msg-len][UTF-8 reason]
//! asker → owner   [u8 0]   ← the RECEIPT: sent only after from_wire, the slug check, AND the
//!                              caller's acceptance gate all pass
//! ```
//!
//! **Both length prefixes are bounded before a byte is allocated** ([`read_framed`]). That is the
//! framing-layer check W3 deliberately left to W1: `ManifestPayload::from_wire` is the *second*
//! ceiling check, and by the time bytes reach it they are already in memory. A hostile peer
//! declaring 500 MB is refused on the declared length, not after the allocation.
//!
//! **The trailing receipt frame is not a courtesy.** Writing bytes proves only that the *sender* had
//! them; the ACK is the asker asserting that a manifest it could actually parse, verify, match to its
//! ticket **and accept** is now in hand — the acceptance gate is the caller's, and it runs before the
//! ACK precisely so the asker never asserts possession of a manifest it cannot use. (Until
//! QURATOR-177 Option E the ACK also spent the ticket; the spent bit is deleted with the ledger —
//! see [`serve_manifest_stream`] — but the ACK's meaning is unchanged.) Every step also carries a deadline, so a peer cannot hold a handler
//! open by connecting and going quiet.
//!
//! ## Authorization — the capability is the ticket, not the address
//!
//! There is no binding token here and the retired follower gate is **not** re-imported. The asker
//! presents the [`TransportTicket`] it received; the owner's node runs [`validate_redemption`] on
//! it (shape + the request binding) and serves. A redeem-time contact standing check used to run
//! here too, withdrawn by owner ruling 2026-09-03 (QURATOR-177): blocking gates chat/DM interaction
//! only — and it was never read-access revocation, since a mutual contact may re-serve a cached
//! copy (Carrier 4) and a public browse key is a single forwardable string.
//!
//! **There is NO per-request authorization lookup on the serve path at all** (owner ruling
//! 2026-09-03, QURATOR-177 Option E). Authorization happens at ASK time — the auto-approve loop
//! consults the standing grant against the asker's npub, established by the NIP-17 seal on the
//! request DM — and the ticket is reduced to address delivery. The issued-ticket ledger that used
//! to answer "what did we issue for this request, and is it spent?" is deleted with the same
//! ruling, and deliberately given up with it: durable replay protection and the audit trail. Do
//! not re-introduce a grant lookup, a replay set, a peer-identity check, or a rate limit on the
//! serve path — **the absence is the ruling, not an oversight.** What bounds repeat traffic is the
//! asker-side trigger condition (a refetch fires only when the collection's fingerprint changed)
//! and the ask-trace claim gate in hb-app, never a serve-side refusal.
//!
//! The ticket rode a **sealed NIP-17 DM to one recipient**, so presenting it back is evidence of
//! receipt — but note precisely what it is: a **BEARER capability**. The owner verifies the ticket's
//! shape and request binding; it does not authenticate the connecting peer as the
//! recipient, and it cannot, because this design deliberately has no `npub`→node-key map to check
//! against. A recipient who forwards the ticket JSON lets someone else dial and redeem it. What that
//! costs is the OWNER's address plus one ciphertext delivery — binding a ticket to the asker's
//! transport identity would need the asker's node key on the wire, which is an owner-level protocol
//! decision, not a local fix. Note also what the capability is *not*: read access. The payload stays browse-key
//! encrypted, so a ticket without the share code buys ciphertext — the distinction `SEMANTICS.md`
//! reserves between a share code (grants *read*) and a transport ticket (grants *connect and
//! fetch*).
//!
//! **No receipt, no spent bit.** Until 2026-09-03 the owner side held a `RedemptionGrant`, spent
//! only after the asker's ACK (`into_consumed(&ManifestPayload)` — "consumed on success"), and
//! refused a replay. That machinery is deleted by the ruling above; what survives of it here is the
//! framing and the slug binding. [`drain_connection`](crate::conn::drain_connection) still
//! precedes the owner's close — without it a fast link can close ahead of the last chunk.
//!
//! **One concurrent redemption per `request_id`.** [`ManifestPlane`] holds an in-flight set keyed
//! by `request_id`, so two simultaneous connections presenting the same ticket cannot both drive
//! half-delivered streams against the same payload resolution at once; the guard is per-connection
//! concurrency control, **not** authorization (which would be a new serve-path check the ruling
//! forbids).

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use hb_core::ticket::{validate_redemption, TransportTicket, TICKET_TAG};
use hb_core::{ManifestPayload, MANIFEST_MAX_TRANSPORT_BYTES};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::conn::drain_connection;

/// The plane's ALPN. Distinct from the retired `/hoardbook/xfer/1`: a peer still speaking the file
/// protocol finds nothing listening, which is the correct answer.
pub const MANIFEST_ALPN: &[u8] = b"/hoardbook/manifest/1";

/// Ceiling on the *request* frame. A request is a ticket JSON — an opaque node address plus two
/// short bindings — so 8 KiB is generous by an order of magnitude. Bounded for the same reason the
/// payload is: a declared length is attacker-controlled input.
pub const FETCH_REQUEST_MAX_FRAME: usize = 8 * 1024;

const STATUS_OK: u8 = 0;
const STATUS_REFUSED: u8 = 1;

/// Ceiling on a refusal message, so an error path cannot become an unbounded allocation either.
const REFUSAL_MAX_FRAME: usize = 4 * 1024;

/// The single refusal for "no approved request matches this ticket". A **constant** rather than two
/// identical string literals, because the property that an unknown request id and a forged ticket
/// are indistinguishable is only as good as the two messages staying byte-identical — and two
/// literals drift the first time someone improves one of them. (Under QURATOR-177 Option E the
/// indistinguishable pair is *a malformed ticket and a wrong-request ticket* — the issued-record
/// lookup whose absence used to be the other half of the pair is deleted with the ledger; the
/// constant itself and the indistinguishability property stay.)
///
/// QURATOR-242 widened the class: every refusal a request can reach AFTER the request-binding
/// gate — payload-resolution failure, slug mismatch, unreadable declared slug — is this same
/// constant, so a forged-ticket prober cannot distinguish "no such collection in this node's
/// cache" from "cached, but the binding refused" (the old mismatch text even NAMED the cached
/// slug). The distinct reasons live in `tracing` only. This is an information-disclosure closure,
/// not an authorization check: nothing new is consulted before serving (INV-9 / Option E).
const REFUSAL_NO_MATCH: &str = "no approved manifest request matches this ticket";

/// How long the owner's side waits for a peer to open a stream and finish its request frame.
///
/// Without this a peer could connect and send nothing — or dribble an 8 KiB request one byte at a
/// time — and hold a handler forever. `read_exact` has no deadline of its own, so the only bound was
/// the peer's goodwill.
const HANDSHAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the owner's side waits for the asker's acknowledgement after writing the payload.
/// Generous: the asker has to read up to 16 MiB, parse it, and verify its integrity before it can
/// honestly ACK. A timeout is **not** a success — the ticket stays unspent.
const ACK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// How long the owner's side waits to *write* the manifest (status byte, payload bytes, FIN) before
/// treating the peer as stalled on flow control. Comparable to `ACK_DEADLINE`: a peer that is
/// honestly reading has to receive the whole payload and then ACK, so the send and the
/// acknowledgement are both bounded by the same peer-side work. A timeout is **not** a success — the
/// grant is dropped and the ticket stays unspent.
const SEND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// How long the asker's side waits for the dial itself — address resolution, the QUIC handshake,
/// whatever hole-punching iroh needs — before treating the ticket's address as dead. The asker's
/// counterpart to the three owner-side deadlines above, and the tightest of the four: the failure
/// it bounds is not slowness but *silence*. A blackholed or hostile address produces no signal at
/// any layer, and iroh retries hole punches and relay lookups against internal timeouts measured
/// in minutes (what the WAN-M dead-endpoint row observed on an unroutable IP), while an honest
/// dial — loopback, direct, or via relay rendezvous — completes the handshake in single-digit
/// seconds. 20s is several times that, as retransmission headroom for a lossy link, while staying
/// under `HANDSHAKE_DEADLINE` (30s): the one step that has proved nothing about the peer should
/// not be able to out-wait the steps granted to a peer that has already connected.
const DIAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// How long a *refused* connection is held open for its small response to flush. Deliberately far
/// shorter than the 5s `drain_connection` window: a refusal is a status byte plus a short framed
/// reason, and holding a redemption permit (or an in-flight claim) for a peer-controlled 5s on a
/// refused connection is the QURATOR-110 DoS. 500ms is enough for the response to flush on any link
/// that delivered the request.
const REFUSAL_DRAIN: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the owner's side waits to *write* a refusal — status byte, one ≤`REFUSAL_MAX_FRAME`
/// framed reason, FIN — before treating the peer as never going to read it. The write-side sibling
/// of `REFUSAL_DRAIN`: the drain argument (QURATOR-110) is that a refused connection is owed only
/// a trickle, and the same holds for the write that precedes it. `SEND_DEADLINE` (120s) is sized
/// for a 16 MiB manifest on a slow link; a ~4 KiB refusal has no legitimate need for it.
/// What the 120s bound was costing (QURATOR-299): tickets are unsigned address delivery (owner
/// ruling 2026-09-03, QURATOR-177 Option E), so any peer can mint a shape-valid ticket answering
/// its own request — it clears `validate_redemption`, takes the in-flight claim in
/// `ManifestPlane::serve` (and one of the three redemption permits), and only then dies at payload
/// resolution, the first check that consults this node's state. A peer that simply stops reading
/// parked all of that for the refusal write's full deadline, per probe.
///
/// 5s, reasoned against the drain rather than picked: 10× the 500ms that already declares "any
/// link that delivered the request can flush a frame this size", i.e. ~15 RTTs on a lossy 300ms
/// path — generous for an honest peer, whose flow-control credit for the stream was granted at
/// handshake, before the request was even sent. And it stays far under the asker's own read bound
/// (`HANDSHAKE_DEADLINE`, 30s), so an honest high-latency peer still reads the refusal AS a
/// refusal — "no match" stays distinguishable from a network fault — while the refusal WRITE's
/// contribution to the parkable window drops from 120s to 5s per probe.
///
/// ⚠ **5s is this deadline, NOT the worst-case claim hold — do not size a retry off it**
/// (review finding, 2026-09-19). When the write genuinely stalls, `refuse()` errors at 5s and
/// `serve`'s `Err` arm holds `_guard` through `drain_connection`, a SECOND 5s bound
/// (`conn.rs`), so the claim releases at **~10s**, not 5. The win is still ~12× (was
/// 120s + drain); the number to plan against is 10s.
const REFUSAL_SEND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Apply a deadline to one step, turning a stall into a stated error instead of a wedged task.
async fn with_deadline<T>(
    limit: std::time::Duration,
    what: &str,
    fut: impl std::future::Future<Output = T>,
) -> Result<T> {
    tokio::time::timeout(limit, fut)
        .await
        .map_err(|_| anyhow!("{what} (waited {}s)", limit.as_secs()))
}

/// The asker's opening frame: which request this is, and the ticket that authorizes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    pub request_id: String,
    pub ticket: TransportTicket,
}

// `IssuedTicket` and the `issued()`/`record_consumed()` trait methods — DELETED 2026-09-03,
// QURATOR-177 Option E (owner ruling: authorization is at ASK time via the standing grant; the
// ticket is address delivery). `issued()` was the serve path's lookup into the issued-ticket
// ledger ("what did we mint for this request, and is it spent?"); `record_consumed()` wrote the
// durable spent bit that fed it. Both the ledger and the whole one-ticket-one-delivery story are
// gone with the ruling — deliberately, giving up cross-restart replay protection and the audit
// trail. Do not re-introduce either method or any per-request authorization lookup on the serve
// path: the absence is the ruling, not an oversight.

/// The owner side's seam onto app state. A trait so the plane is testable over real QUIC without
/// Tauri, and so the plane itself holds no store handle.
///
/// **Note what `payload` returns and does not take:** a [`ManifestPayload`] for a presented
/// [`TransportTicket`]. There is no path parameter and no byte-slice parameter, so an
/// implementation cannot answer with a collection file even if it wanted to — mechanism 1
/// reaching one layer up from the type into the seam. The ticket names the slug and (for a
/// Carrier-4 re-serve) the author to resolve from: the owner path rebuilds from the collection as
/// it is NOW, the carrier-4 re-serve path serves the newest cached copy for `(author_npub, slug)`
/// (QURATOR-177 slice 1).
pub trait ManifestSource: Send + Sync + 'static {
    /// The manifest for the request the presented ticket answers.
    fn payload(&self, ticket: &TransportTicket) -> Result<ManifestPayload>;
}

// ---------------------------------------------------------------------------
// Framing — the length prefix is bounded BEFORE anything is allocated
// ---------------------------------------------------------------------------

/// Read one length-prefixed frame, refusing an over-cap **declared** length before allocating.
///
/// This is the check `ManifestPayload::from_wire` documents as belonging to the framing layer. The
/// order matters and is the whole point: a peer that declares `u32::MAX` is refused on the four
/// bytes it sent, not after a 4 GiB allocation. `from_wire` remains the second check — defence in
/// depth for any future caller that assembles bytes some other way.
/// Byte-level progress for an in-flight frame read: `(received, total)` — cumulative body bytes
/// read so far, and the frame's **declared** length. A plain callable rather than a Tauri handle,
/// so the seam the WAN harness calls stays runtime-free; only the `redeem_manifest_ticket`
/// wrapper passes one.
pub(crate) type ProgressSink<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

/// One progress sample for an in-flight manifest fetch, tagged with the ticket it belongs to —
/// this is the payload the wrapper emits as the `manifest-progress` Tauri event. Plain data, no
/// Tauri types, so it can cross the runtime-free seam into `redeem_manifest_ticket_inner`.
#[derive(Clone, Debug)]
pub(crate) struct ManifestProgress {
    pub(crate) request_id: String,
    pub(crate) slug: String,
    pub(crate) received: u64,
    pub(crate) total: u64,
}

impl ManifestProgress {
    /// The `ProgressSink` closure for one fetch: stamps each `(received, total)` sample with this
    /// ticket's `request_id` and `slug`, and forwards it over the channel.
    pub(crate) fn sink(
        self,
        tx: &tokio::sync::mpsc::UnboundedSender<ManifestProgress>,
    ) -> impl Fn(u64, u64) + Send + Sync + '_ {
        move |received, total| {
            let _ = tx.send(Self {
                received,
                total,
                ..self.clone()
            });
        }
    }
}

pub(crate) async fn read_framed(
    recv: impl tokio::io::AsyncRead + Unpin,
    max: usize,
) -> Result<Vec<u8>> {
    read_framed_with_progress(recv, max, None).await
}

/// [`read_framed`] plus an optional progress sink, handed `(received, total)` as the body arrives.
///
/// The body still lands in the ONE `declared`-byte allocation — chunking changes when progress is
/// reported, not how much memory is held — and the ceiling still fires on the declared length
/// before that allocation exists.
pub(crate) async fn read_framed_with_progress(
    mut recv: impl tokio::io::AsyncRead + Unpin,
    max: usize,
    progress: Option<ProgressSink<'_>>,
) -> Result<Vec<u8>> {
    let declared = recv.read_u32_le().await.context("read frame length")? as usize;
    if declared > max {
        return Err(anyhow!(
            "peer declared a {declared}-byte frame, over the {max}-byte ceiling — refused before \
             reading it (a manifest over {MANIFEST_MAX_TRANSPORT_BYTES} bytes is too large for the \
             transport; export it instead)"
        ));
    }
    let mut buf = vec![0u8; declared];
    read_body_chunked(&mut recv, &mut buf, progress)
        .await
        .context("read frame body")?;
    Ok(buf)
}

/// `read_exact` in bounded chunks, reporting cumulative progress. EOF partway through is the same
/// `UnexpectedEof` `read_exact` produces, so a truncated body still reads as a failure.
async fn read_body_chunked(
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
    buf: &mut [u8],
    progress: Option<ProgressSink<'_>>,
) -> std::io::Result<()> {
    const CHUNK: usize = 64 * 1024;
    let total = buf.len() as u64;
    let mut done = 0usize;
    while done < buf.len() {
        let end = (done + CHUNK).min(buf.len());
        let n = recv.read(&mut buf[done..end]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "failed to fill whole buffer",
            ));
        }
        done += n;
        if let Some(sink) = progress {
            sink(done as u64, total);
        }
    }
    Ok(())
}

async fn write_framed(mut send: impl tokio::io::AsyncWrite + Unpin, bytes: &[u8]) -> Result<()> {
    let len: u32 = bytes
        .len()
        .try_into()
        .map_err(|_| anyhow!("frame does not fit a u32 length prefix"))?;
    send.write_u32_le(len).await.context("write frame length")?;
    send.write_all(bytes).await.context("write frame body")?;
    Ok(())
}

async fn refuse(mut send: impl tokio::io::AsyncWrite + Unpin, reason: &str) -> Result<()> {
    with_deadline(
        REFUSAL_SEND_DEADLINE,
        "the peer stopped reading the refusal response",
        async {
            send.write_u8(STATUS_REFUSED).await.context("write refused status")?;
            let mut msg = reason.as_bytes();
            if msg.len() > REFUSAL_MAX_FRAME {
                msg = &msg[..REFUSAL_MAX_FRAME];
            }
            write_framed(&mut send, msg).await?;
            send.shutdown().await.context("shutdown after refusal")?;
            Ok(())
        },
    )
    .await?
}

/// Hold a *refused* connection open for a fixed, short interval so its small response flushes, then
/// give up regardless of whether the peer closed. The full 5-second drain in `crate::conn` exists to
/// protect a *delivered* manifest's last chunk from a CONNECTION_CLOSE race; a refusal is a status
/// byte plus a short framed reason, and holding a redemption permit (or an in-flight claim) for a
/// peer-controlled 5s on a refused connection is the QURATOR-110 DoS. See `REFUSAL_DRAIN`.
async fn drain_refusal(conn: &iroh::endpoint::Connection) {
    let _ = tokio::time::timeout(REFUSAL_DRAIN, conn.closed()).await;
}

// ---------------------------------------------------------------------------
// Owner side — serve one manifest for one ticket
// ---------------------------------------------------------------------------

/// Serve one request over an already-accepted bi-stream. Returns the delivered payload when the
/// asker **acknowledged** a valid manifest, `Ok(None)` when the request was refused for a stated
/// reason.
///
/// Generic over the streams so the framing and the gate are testable without QUIC; the real caller
/// is [`ManifestPlane::serve`].
///
/// **No authorization lookup happens here** (owner ruling 2026-09-03, QURATOR-177 Option E): the
/// ask was authorized when it was made — the standing grant, checked under the NIP-17 seal — and
/// the ticket is address delivery. Until that ruling this function looked up the issued-ticket
/// record (`source.issued`), compared the presented ticket against the stored one
/// (`matches_issued`) and refused a spent one; all three steps are deleted with the ledger, and
/// deliberately so, giving up durable replay protection and the audit trail. What remains is
/// parsing, the request-binding check (`validate_redemption`: shape + the ticket answers THIS
/// request) — a correctness check, not authorization — the payload resolution, and the byte/slug
/// binding. **Every refusal past the parse is the same wire string, `REFUSAL_NO_MATCH`**
/// (QURATOR-242): pre-resolution and post-resolution arms alike, so a refusal's wording leaks
/// nothing about what this node holds — the distinct reason goes to `tracing` only.
pub(crate) async fn serve_manifest_stream(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    source: &dyn ManifestSource,
) -> Result<Option<ManifestPayload>> {
    let frame = with_deadline(
        HANDSHAKE_DEADLINE,
        "the asker never finished sending its request",
        read_framed(&mut recv, FETCH_REQUEST_MAX_FRAME),
    )
    .await??;
    let req: FetchRequest = match serde_json::from_slice(&frame) {
        Ok(r) => r,
        Err(e) => {
            refuse(&mut send, &format!("malformed fetch request: {e}")).await?;
            return Ok(None);
        }
    };

    // The request-binding gate (correctness, not authorization): the ticket must be structurally
    // valid and answer the request it was presented under. A malformed or wrong-request ticket is
    // refused identically — the same constant — so a prober learns nothing about which requests
    // exist. (Until QURATOR-177 Option E this also refused a spent ticket; that arm is deleted
    // with the ledger.)
    if validate_redemption(&req.ticket, &req.request_id).is_err() {
        refuse(&mut send, REFUSAL_NO_MATCH).await?;
        return Ok(None);
    }

    let payload = match source.payload(&req.ticket) {
        Ok(p) => p,
        Err(e) => {
            // **QURATOR-242 — `REFUSAL_NO_MATCH`, same as the gate above.** The text used to embed
            // `{e}`, which made "no such collection in this node's cache" distinguishable from
            // "cached, but the slug binding refused" — a slug-existence oracle for a peer holding
            // a shape-valid forged ticket and no authorization to ask (the payload stays
            // ciphertext either way; this closes a metadata channel, it adds no check). The
            // reason stays in the local log; the wire gets the one string. A payload-resolution
            // failure still costs nothing: the asker can retry once the owner's side is healthy.
            tracing::debug!(
                slug = %req.ticket.slug,
                error = %e,
                "serve refusal: payload resolution failed"
            );
            refuse(&mut send, REFUSAL_NO_MATCH).await?;
            return Ok(None);
        }
    };

    // **Bind the bytes to the ticket.** `ManifestSource::payload(ticket)` resolving to the right
    // collection is a naming convention, and a convention is not enforcement: a source that
    // answers with collection B for a ticket naming collection A would otherwise be served, and
    // the asker would accept it as self-consistent (it *is* — it is a perfectly valid envelope
    // for the wrong collection). A ticket names one collection; the bytes must agree. Checked on
    // both sides — see `fetch_over_connection`.
    match payload.declared_slug() {
        Ok(declared) if declared == req.ticket.slug => {}
        Ok(declared) => {
            // **QURATOR-242 — `REFUSAL_NO_MATCH`, same as every other refusal past the parse.**
            // The old text NAMED the cached collection's slug ("the manifest describes
            // '{declared}' ..."), which is a strictly stronger oracle than existence: it told a
            // forged-ticket prober exactly what this node holds. Both slugs stay in the local
            // log; the wire gets the one string.
            tracing::warn!(
                ticket_slug = %req.ticket.slug,
                declared_slug = %declared,
                "serve refusal: the resolved manifest's slug does not match the ticket"
            );
            refuse(&mut send, REFUSAL_NO_MATCH).await?;
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!(
                ticket_slug = %req.ticket.slug,
                error = %e,
                "serve refusal: the resolved manifest's own slug was unreadable"
            );
            refuse(&mut send, REFUSAL_NO_MATCH).await?;
            return Ok(None);
        }
    }

    with_deadline(
        SEND_DEADLINE,
        "the peer stopped reading the manifest — treating it as undelivered",
        async {
            send.write_u8(STATUS_OK).await.context("write ok status")?;
            write_framed(&mut send, payload.as_bytes()).await?;
            send.shutdown().await.context("shutdown send")?;
            Ok::<(), anyhow::Error>(())
        },
    )
    .await??;

    // ── The acknowledgement frame ──
    //
    // Writing the bytes proves only that WE had them. The asker acknowledges only after
    // `ManifestPayload::from_wire` and the slug check have both passed on its side, so an ACK means
    // a validated manifest is in the asker's hands. Until QURATOR-177 Option E this is also where
    // the ticket was spent (`into_consumed`); the receipt is gone with the ledger, but the ACK
    // still matters — it is what tells this side the transfer actually landed rather than dying
    // mid-write, so a failure is reported (and logged) as undelivered.
    match with_deadline(
        ACK_DEADLINE,
        "the asker never acknowledged the manifest — treating it as undelivered",
        recv.read_u8(),
    )
    .await
    {
        Ok(Ok(STATUS_OK)) => Ok(Some(payload)),
        Ok(Ok(other)) => Err(anyhow!("asker sent {other} instead of an acknowledgement")),
        Ok(Err(e)) => Err(anyhow!("asker closed before acknowledging: {e}")),
        Err(e) => Err(e),
    }
}

/// The manifest plane's accept side: a [`ManifestSource`] plus an **in-flight set** keyed by
/// `request_id`.
///
/// **Concurrency control, NOT authorization** (owner ruling 2026-09-03, QURATOR-177 Option E).
/// Until that ruling the set existed to make one ticket one delivery: `issued()` read
/// `already_consumed` and `record_consumed()` wrote it much later, so two simultaneous connections
/// could both pass the gate and both deliver. The ledger that fed that story is deleted, so the set
/// no longer guards a spent bit — what it guards is the connection lifecycle itself: two
/// simultaneous streams answering the same `request_id` would race their payload resolution and
/// writes against one another inside one handler pair, and the claim makes the second wait for the
/// first to finish rather than interleave. It is per-process, which is the right scope: a ticket
/// names one issuer, and the issuer is one node.
///
/// Do not grow this back into an authorization check — that is the withdrawn behaviour.
pub struct ManifestPlane {
    source: Arc<dyn ManifestSource>,
    in_flight: std::sync::Mutex<std::collections::HashSet<String>>,
}

// The `poisoned` set (requests whose receipt could NOT be persisted, refused for the rest of the
// process so a durable-write failure could not become a second delivery) — DELETED 2026-09-03,
// QURATOR-177 Option E, with the receipts it guarded. There is no receipt write left to fail.

/// Removes its `request_id` from the in-flight set on drop, so a panic or an early return cannot
/// wedge a ticket permanently un-redeemable.
struct InFlightGuard<'a> {
    plane: &'a ManifestPlane,
    request_id: String,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.plane.in_flight.lock() {
            set.remove(&self.request_id);
        }
    }
}

impl ManifestPlane {
    pub fn new(source: Arc<dyn ManifestSource>) -> Arc<Self> {
        Arc::new(Self {
            source,
            in_flight: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// Claim `request_id` for this redemption, or `None` if one is already running.
    fn claim(&self, request_id: &str) -> Option<InFlightGuard<'_>> {
        let mut set = self.in_flight.lock().ok()?;
        if !set.insert(request_id.to_string()) {
            return None;
        }
        Some(InFlightGuard { plane: self, request_id: request_id.to_string() })
    }

    /// Accept one connection on the manifest plane and serve it.
    ///
    /// The [`drain_connection`] call is load-bearing and its position is deliberate: it holds the
    /// connection open until the asker closes it, so the last chunk is not raced by a
    /// `CONNECTION_CLOSE` on a fast link. Until QURATOR-177 Option E the receipt was recorded
    /// after this drain, only when the asker acknowledged; the receipt is deleted with the ledger,
    /// and what survives is the delivered/undelivered distinction the ACK decides inside
    /// [`serve_manifest_stream`].
    pub async fn serve(&self, conn: iroh::endpoint::Connection) -> Result<()> {
        let (send, mut recv) = with_deadline(
            HANDSHAKE_DEADLINE,
            "peer connected but never opened a stream",
            conn.accept_bi(),
        )
        .await?
        .context("accept_bi")?;

        // Peek the request before claiming, because the claim is keyed by request_id. Re-framing the
        // already-read bytes back into `serve_manifest_stream` keeps that function the single
        // implementation of the protocol rather than duplicating the gate here.
        //
        // **QURATOR-243 — the claim is decided from the WHOLE request, not just its id.** Until
        // this, the claim was taken from the peeked `request_id` alone (a frame that did not parse
        // as a `FetchRequest` claimed the EMPTY STRING as its key), and it was taken BEFORE the
        // gate ran inside `serve_manifest_stream` — so any request, however invalid, held its
        // claim slot for the full refusal window: the refusal write carried `SEND_DEADLINE`
        // (120s, at the time — since QURATOR-299, `REFUSAL_SEND_DEADLINE`), and a stream of
        // garbage borrowing a legitimate `request_id` could park that
        // slot for 120s per probe while being refused, denying the honest holder's concurrent
        // redemption as "already being redeemed". Now a request only claims if it would pass the
        // gate: parsed as a `FetchRequest`, and through `validate_redemption` — the SAME
        // shape + request-binding correctness check `serve_manifest_stream` runs below (and runs
        // again; it is pure and cheap). A request that cannot pass never enters the set;
        // `serve_manifest_stream` still refuses it exactly as before, so we skip the claim, not
        // the refusal, and no gate or refusal wording is duplicated here.
        //
        // This is a *reordering of an existing shape check*, not a new permission (owner ruling
        // 2026-09-03, QURATOR-177 Option E): nothing here consults state about WHO is asking, and
        // the in-flight set stays what it was built to be — concurrency control for connections
        // that will actually serve. A gate-passing request is claimed and served exactly as
        // before. (A ticket that passes `validate_redemption` but is refused later, at payload
        // resolution, still claims for its refusal window — tickets are unsigned address delivery,
        // so that shape is forgeable. That window is bounded since QURATOR-299 by
        // `REFUSAL_SEND_DEADLINE`, the refusal-specific WRITE deadline this note flagged as the
        // lever, so the claim parks for seconds, not 120s, per probe.)
        let frame = with_deadline(
            HANDSHAKE_DEADLINE,
            "the asker never finished sending its request",
            read_framed(&mut recv, FETCH_REQUEST_MAX_FRAME),
        )
        .await??;

        let claimable = serde_json::from_slice::<FetchRequest>(&frame)
            .ok()
            .filter(|req| validate_redemption(&req.ticket, &req.request_id).is_ok());
        let _guard = match &claimable {
            Some(req) => match self.claim(&req.request_id) {
                Some(g) => Some(g),
                None => {
                    let mut send = send;
                    // Concurrent redemption of the same ticket: refuse the second, do not serve it.
                    refuse(&mut send, "this ticket is already being redeemed on another connection")
                        .await?;
                    drain_refusal(&conn).await;
                    return Ok(());
                }
            },
            // Not claimable: no slot is taken at all. `serve_manifest_stream` below produces the
            // refusal (it remains the single implementation of the gate and its wording), while
            // this connection holds nothing a concurrent honest redemption could collide with.
            None => None,
        };

        let replayed = Framed::new(frame, recv);
        let served = serve_manifest_stream(send, replayed, self.source.as_ref()).await;

        // **Which side closes, and why the drain is only on one path.**
        //
        // The ACK is strictly stronger evidence than "the peer closed": it says a validated manifest
        // arrived. So on the delivered path we do not drain — we record and close, and the asker
        // waits for *our* close. That ordering matters, and its absence was a live bug: with the
        // asker closing immediately after writing the ACK, `CONNECTION_CLOSE` outran the ACK on
        // loopback and the owner never saw it. **That is the `drain_connection` truncation race
        // exactly, in the opposite direction** — the HANDOVER note said it would recur, and adding a
        // trailing frame made it recur the same day. Whoever writes last must not be the one to close.
        //
        // On the refusal path there is no ACK, so "the peer closed" is the only evidence the tiny
        // response landed — the drain stays, and `conn::a_refusal_response_survives_connection_close`
        // is what holds it there.
        match served {
            Ok(Some(_delivered)) => {
                // Delivered and acknowledged. Until QURATOR-177 Option E this arm also recorded the
                // receipt and poisoned the request on a write failure; both are deleted with the
                // ledger (owner ruling 2026-09-03).
                conn.close(0u32.into(), b"delivered");
                Ok(())
            }
            Ok(None) => {
                drain_refusal(&conn).await;
                Ok(())
            }
            Err(e) => {
                drain_connection(&conn).await;
                Err(e)
            }
        }
    }
}

/// A reader that yields one already-read frame (length prefix re-synthesized) and then continues
/// from the live stream. Lets `ManifestPlane::serve` inspect the request id for its in-flight claim
/// without `serve_manifest_stream` needing a second, subtly-different code path.
struct Framed<R> {
    prefix: std::io::Cursor<Vec<u8>>,
    rest: R,
}

impl<R> Framed<R> {
    fn new(frame: Vec<u8>, rest: R) -> Self {
        let mut buf = Vec::with_capacity(frame.len() + 4);
        buf.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        buf.extend_from_slice(&frame);
        Self { prefix: std::io::Cursor::new(buf), rest }
    }
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Framed<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if (self.prefix.position() as usize) < self.prefix.get_ref().len() {
            return std::pin::Pin::new(&mut self.prefix).poll_read(cx, buf);
        }
        std::pin::Pin::new(&mut self.rest).poll_read(cx, buf)
    }
}

// ---------------------------------------------------------------------------
// Asker side — redeem a ticket, get a manifest
// ---------------------------------------------------------------------------

/// Redeem a ticket: dial the address it carries, present it, and return the manifest.
///
/// **Redemption is immediate and has no defer affordance** (owner ruling 2026-07-30) — this function
/// is the whole redemption path, it is called as soon as a ticket arrives, and there is deliberately
/// no "redeem later" entry point for a UI to bind a button to.
///
/// ## `accept` — why the caller's gate runs *inside* this function
///
/// The ACK is what tells the owner the transfer landed, so it must mean **"a manifest we can
/// actually use arrived"**. `from_wire` does not prove that: it proves the bytes are a structurally
/// self-consistent envelope, under the ceilings, declaring the ticket's slug. It cannot prove the
/// envelope is signed by *the peer we are browsing*, that the body decrypts under *our* browse-key,
/// or that the tree is complete — those need the contact store, which this layer deliberately does
/// not have.
///
/// Running that gate *after* `fetch_manifest` returned would ACK first and reject second: the asker
/// ends up with nothing usable while the owner has counted a delivery that never landed, and the
/// human has to ask again. That is the `6691377` defect — "consumed on success" quietly
/// meaning "consumed on written" — recurring one layer up, with the boundary moved from *we wrote
/// it* to *they parsed it* when what matters is *they can use it*.
///
/// So the caller passes its gate in, it runs before the ACK, and a rejection returns without
/// acknowledging. The layering is intact — the closure comes from the caller that owns the store;
/// this module still holds no store handle.
pub async fn fetch_manifest(
    endpoint: &iroh::Endpoint,
    ticket: &TransportTicket,
    accept: impl FnOnce(&ManifestPayload) -> Result<()>,
) -> Result<ManifestPayload> {
    fetch_manifest_with_progress(endpoint, ticket, accept, None).await
}

/// [`fetch_manifest`] with byte-level progress for the manifest body, reported through the sink as
/// `(received, total)`. `None` on every existing caller, so the seam the WAN harness drives is
/// unchanged.
pub async fn fetch_manifest_with_progress(
    endpoint: &iroh::Endpoint,
    ticket: &TransportTicket,
    accept: impl FnOnce(&ManifestPayload) -> Result<()>,
    progress: Option<ProgressSink<'_>>,
) -> Result<ManifestPayload> {
    fetch_manifest_with_progress_inner(endpoint, ticket, accept, progress, DIAL_DEADLINE).await
}

/// [`fetch_manifest_with_progress`] with the dial deadline injectable, so the loopback harness can
/// prove the bound fires without sitting through the full `DIAL_DEADLINE`. The public function is
/// the shim that pins the production constant — the same `*_inner` shape the command layer uses;
/// no caller outside this module supplies the duration.
async fn fetch_manifest_with_progress_inner(
    endpoint: &iroh::Endpoint,
    ticket: &TransportTicket,
    accept: impl FnOnce(&ManifestPayload) -> Result<()>,
    progress: Option<ProgressSink<'_>>,
    dial_deadline: std::time::Duration,
) -> Result<ManifestPayload> {
    ticket.verify_shape().map_err(|e| anyhow!("ticket refused before dialling: {e}"))?;
    let addr = parse_node_addr(&ticket.node_addr)?;
    tracing::debug!(slug = %ticket.slug, "iroh: dialing the manifest plane");
    // The context wraps the WHOLE dial, timeout included: `with_deadline` returns the timeout as its
    // own `Err`, so attaching the context to only the inner `Result` would let a timed-out dial
    // escape without naming the slug — the one identifier that makes the log actionable. Flattened
    // so both arms (deadline elapsed, iroh refused) carry it, and only the cause distinguishes them.
    let conn = with_deadline(
        dial_deadline,
        "the ticket's address never answered the dial",
        endpoint.connect(addr, MANIFEST_ALPN),
    )
    .await
    .and_then(|r| r.map_err(Into::into))
    .with_context(|| format!("dial the manifest plane for {}", ticket.slug))?;
    tracing::debug!(slug = %ticket.slug, "iroh: connected — fetching manifest");
    let result = fetch_over_connection(&conn, ticket, accept, progress).await;
    conn.close(0u32.into(), b"");
    result
}

async fn fetch_over_connection(
    conn: &iroh::endpoint::Connection,
    ticket: &TransportTicket,
    accept: impl FnOnce(&ManifestPayload) -> Result<()>,
    progress: Option<ProgressSink<'_>>,
) -> Result<ManifestPayload> {
    let (mut send, mut recv) = with_deadline(
        HANDSHAKE_DEADLINE,
        "the peer never opened a stream",
        conn.open_bi(),
    )
    .await?
    .context("open_bi")?;
    let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: ticket.clone() };
    with_deadline(
        SEND_DEADLINE,
        "the peer stopped reading our request",
        write_framed(&mut send, &serde_json::to_vec(&req)?),
    )
    .await??;
    // **Deliberately not shut down here.** The send half stays open to carry the acknowledgement —
    // that frame is what lets the owner distinguish "delivered" from "written at". Closing early
    // would put us back to the owner burning tickets on transfers that never landed.

    let status = with_deadline(
        HANDSHAKE_DEADLINE,
        "the peer never sent a status byte",
        recv.read_u8(),
    )
    .await?
    .context("read status byte")?;
    match status {
        STATUS_OK => {
            // Bounded on the declared length first (the framing check), then again by `from_wire`.
            let bytes = with_deadline(
                ACK_DEADLINE,
                "the peer never finished sending the manifest",
                read_framed_with_progress(&mut recv, MANIFEST_MAX_TRANSPORT_BYTES, progress),
            )
            .await??;
            let payload = ManifestPayload::from_wire(bytes)
                .map_err(|e| anyhow!("the peer's manifest was refused: {e}"))?;

            // The asker's half of the slug binding. The owner checks it too, but a peer is not a
            // trusted validator of what it sends us: a manifest for a collection we did not ask
            // about must not be accepted just because it is internally consistent.
            let declared = payload
                .declared_slug()
                .map_err(|e| anyhow!("the peer's manifest has no readable slug: {e}"))?;
            if declared != ticket.slug {
                return Err(anyhow!(
                    "refusing the peer's manifest: it describes '{declared}' but we asked for '{}'",
                    ticket.slug
                ));
            }

            // The caller's gate — the author pin, the browse-key decrypt, the completeness check.
            // **Before the ACK, deliberately** (see `fetch_manifest`): the owner spends the ticket on
            // that ACK, so anything that can make this manifest unusable has to have run already.
            // Returning here leaves the ticket unspent and the retry re-authorizes.
            accept(&payload).map_err(|e| anyhow!("the manifest arrived but was not accepted: {e}"))?;

            // Acknowledge only now — after the bytes parsed, verified, matched the ticket, AND passed
            // the caller's gate. This is the asker's assertion that a *usable* manifest arrived, and
            // the owner consumes the ticket on the strength of it.
            with_deadline(
                SEND_DEADLINE,
                "the peer stopped reading our acknowledgement",
                async {
                    send.write_u8(STATUS_OK).await.context("write acknowledgement")?;
                    send.shutdown().await.context("shutdown after acknowledgement")?;
                    Ok::<(), anyhow::Error>(())
                },
            )
            .await??;
            // Wait (bounded) for the owner to close, rather than closing ourselves. `shutdown` only
            // queues the FIN; closing straight after lets `CONNECTION_CLOSE` overtake the ACK, and
            // the owner then treats a delivered manifest as undelivered. Same failure as the original
            // teardown truncation race, mirrored — so the same remedy, on this side.
            drain_connection(conn).await;
            Ok(payload)
        }
        STATUS_REFUSED => {
            let msg = with_deadline(
                HANDSHAKE_DEADLINE,
                "the peer never finished sending the refusal",
                read_framed(&mut recv, REFUSAL_MAX_FRAME),
            )
            .await??;
            Err(anyhow!("{}", String::from_utf8_lossy(&msg)))
        }
        other => Err(anyhow!("peer sent an unknown status byte {other}")),
    }
}

// ---------------------------------------------------------------------------
// Endpoint lifecycle + the ticket's opaque address
// ---------------------------------------------------------------------------

/// Bind the plane's endpoint from the session's transport secret (the third identity key, M18 W2).
///
/// Persisting that secret is what makes the node identity **stable across restarts** — minting per
/// launch would hand a peer a new identity every time, and an unredeemed ticket would go stale for
/// a reason no user could see.
pub async fn bind_endpoint(transport_secret: &[u8; 32]) -> Result<iroh::Endpoint> {
    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::from_bytes(transport_secret))
        .alpns(vec![MANIFEST_ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| anyhow!("bind the manifest transport endpoint: {e}"))
}

/// Bind an endpoint that can **dial but not be dialled** — no advertised ALPN, and the caller starts
/// no accept loop (owner ruling ③, 2026-07-31).
///
/// **Why a redeemer should not listen.** Redeeming reveals our node id and IP to the owner; that much
/// is unavoidable, because we dial them. What a *listening* endpoint adds is durable: the transport
/// secret is persisted, so that node identity is stable forever, and an accept loop answers anyone
/// who has it. They get no plaintext — any payload the plane serves is browse-key sealed, so a
/// ticket without the share code buys ciphertext — but they get **liveness**, on demand, for as
/// long as the app runs.
///
/// That makes every asker a probeable presence oracle for every owner they have ever redeemed from,
/// which contradicts the design elsewhere: presence is a **signed beacon with a TTL that you
/// publish**, never something a third party can poll. `presence_carries_no_address_or_node_key` keeps
/// that true for the beacon; this keeps it true for the transport.
///
/// **This is hardening, not a wall.** An owner mid-redemption still sees us, and someone holding our
/// node id can still learn something from how a dial fails. It removes the standing, always-on answer.
pub async fn bind_client_endpoint(transport_secret: &[u8; 32]) -> Result<iroh::Endpoint> {
    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::from_bytes(transport_secret))
        // No `.alpns(..)`: we advertise nothing, so there is no protocol to accept. Connecting needs
        // no advertised ALPN — every client in this module's tests binds exactly this way.
        .bind()
        .await
        .map_err(|e| anyhow!("bind the manifest transport client endpoint: {e}"))
}

/// The local endpoint's dialable address, in the form a ticket carries it.
///
/// `hb-core` treats `TransportTicket::node_addr` as an **opaque string** and has no iroh dependency,
/// so this is the one place the two representations meet. JSON of iroh's own `EndpointAddr` — its
/// serde impl is the format, not a hand-rolled encoding that could drift from it.
pub fn ticket_node_addr(endpoint: &iroh::Endpoint) -> Result<String> {
    let addr = endpoint.addr();
    serde_json::to_string(&addr).context("serialize the local endpoint address for a ticket")
}

pub(crate) fn parse_node_addr(raw: &str) -> Result<iroh::EndpointAddr> {
    serde_json::from_str(raw).context("the ticket's node address is not a dialable endpoint address")
}

/// SSRF guard (QURATOR-113 #20): the ticket's `node_addr` is written by whoever answers a manifest
/// ask, so every `TransportAddr` it carries is a dial target a stranger controls. Keep only
/// globally-routable targets before the dial — `Ip` addresses are classified by
/// [`crate::net::ip_non_global`] (the same loopback/private/link-local/CGNAT checks the Settings path
/// applies), `Relay` URLs are kept only when their host is not an internal literal, and `Custom`
/// transports (never registered here, opaque peer data) are dropped.
fn retain_global_transport_addrs(addr: &mut iroh::EndpointAddr) {
    addr.addrs.retain(|a| match a {
        iroh::TransportAddr::Ip(sock) => !crate::net::ip_non_global(sock.ip()),
        iroh::TransportAddr::Relay(url) => relay_host_is_global(url.host_str()),
        _ => false,
    });
}

/// Whether a relay URL's host is a safe dial target. Only an IP-literal host is classifiable by
/// address class (reusing [`crate::net::ip_non_global`]); a DNS name is kept — the same no-resolution
/// residual `validate_relay_url` documents (a public name that rebinds to a private IP is accepted),
/// while a literal `localhost`/`*.local` is dropped.
fn relay_host_is_global(host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return !crate::net::ip_non_global(ip);
    }
    let name = bare.trim_end_matches('.').to_ascii_lowercase();
    !(name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local"))
}

/// Parse a peer-authored ticket address and drop the non-global transport addresses, returning the
/// sanitized string for the dial. The transport layer's `fetch_manifest` stays unguarded so its
/// loopback QUIC harness keeps exercising the real dial; the command that owns the peer-trust
/// boundary (`redeem_manifest_ticket`) calls this before handing the ticket down.
pub(crate) fn sanitize_node_addr(raw: &str) -> Result<String> {
    let mut addr = parse_node_addr(raw)?;
    retain_global_transport_addrs(&mut addr);
    serde_json::to_string(&addr).context("re-serialize the sanitized endpoint address")
}

/// How long a serve path waits, after the endpoint binds, for the home relay to connect before
/// minting the ticket (QURATOR-314).
///
/// **Why 10 s:** iroh's own `Endpoint::online` docs recommend wrapping it in a timeout "close to
/// `NET_REPORT_TIMEOUT`" — 10 s in iroh 1.0.3 — so at least one net report has been attempted.
/// The live measurement behind this fix (2026-09-23, two NAT'd machines) had the home relay
/// arriving ~3.06 s after bind, so 10 s is ~3x the observed worst case. It bites only on the FIRST
/// mint after a cold bind: an already-online endpoint's `online()` resolves on the first poll and
/// returns immediately, so later serves on the same plane pay nothing.
pub const HOME_RELAY_MINT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Wait, bounded, for the endpoint to have its home relay connected — the precondition the
/// ticket's address depends on (QURATOR-314).
///
/// iroh's `bind()` returns BEFORE a relay is picked, and [`ticket_node_addr`] snapshots
/// `endpoint.addr()`, so a ticket minted straight after the bind carries no
/// `TransportAddr::Relay`; on a NAT'd box with no global IP the SSRF sanitize
/// ([`sanitize_node_addr`]) then leaves the ticket EMPTY, and the asker's dial degrades to
/// discovery or times out. **This never refuses anything:** on timeout it returns `false` and the
/// caller STILL mints, so offline/relay-less use keeps working exactly as before — the wait can
/// only improve the address, never block the approval.
pub async fn await_home_relay(endpoint: &iroh::Endpoint, wait: std::time::Duration) -> bool {
    match tokio::time::timeout(wait, endpoint.online()).await {
        Ok(()) => true,
        Err(_elapsed) => {
            tracing::warn!(
                ?wait,
                "transport: no home relay connected within the mint wait — minting anyway; the \
                 asker's dial falls back to discovery exactly as before (QURATOR-314)"
            );
            false
        }
    }
}

/// Mint a ticket for one approved request, addressed at this endpoint.
///
/// Thin by design: the lifecycle rules live in `hb_core::ticket`, and this only supplies the address
/// hb-core cannot compute. `TICKET_TAG` is asserted here so a caller cannot accidentally build the
/// body by hand and get the discriminator wrong.
pub fn issue_ticket(
    endpoint: &iroh::Endpoint,
    request_id: &str,
    slug: &str,
    issued_at: u64,
    ask_nonce: Option<&str>,
) -> Result<TransportTicket> {
    let ticket =
        TransportTicket::issue(request_id, slug, &ticket_node_addr(endpoint)?, issued_at, ask_nonce);
    debug_assert_eq!(ticket.hb, TICKET_TAG);
    ticket.verify_shape().map_err(|e| anyhow!("refusing to issue a malformed ticket: {e}"))?;
    Ok(ticket)
}

// ---------------------------------------------------------------------------
// Tests
//
// The plane's tests run over REAL QUIC on loopback (`presets::Minimal` — no relays, no address
// lookup, so nothing leaves 127.0.0.1/::1). A mocked stream cannot exhibit the two failures that
// actually matter here: the teardown truncation race, and a multi-MB payload spanning many QUIC
// frames.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::Mutex;

    use hb_core::{build_manifest_envelope, Identity};
    use hb_net::split_listing;

    const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// The multi-MB case needs its own budget. **Measured on this machine (debug build):** the
    /// transfer is not the cost — 7.3 MB crossed loopback and re-verified in ~355 ms. Building the
    /// fixture is, because it NIP-44-encrypts every 40 KB part: ~2 s per MB, so 10k entries ≈ 20 s
    /// before a byte moves. Sizing the fixture down to fit a 30 s budget would have meant testing a
    /// payload too small to span many QUIC frames, which is the thing being tested.
    const MULTI_MB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

    /// Bind a loopback-only endpoint: relays and address lookup disabled, so a test never touches
    /// the network beyond localhost.
    pub(crate) async fn bind_local_endpoint(
        secret: &[u8; 32],
        alpns: Vec<Vec<u8>>,
    ) -> iroh::Endpoint {
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(secret))
            .alpns(alpns)
            .bind()
            .await
            .expect("bind loopback endpoint")
    }

    /// The server's address rewritten to loopback IPs (a bound socket is a wildcard address).
    pub(crate) fn loopback_addr(server: &iroh::Endpoint) -> iroh::EndpointAddr {
        let mut addrs: BTreeSet<iroh::TransportAddr> = BTreeSet::new();
        for sock in server.bound_sockets() {
            let ip: IpAddr = if sock.is_ipv4() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            };
            addrs.insert(iroh::TransportAddr::Ip(SocketAddr::new(ip, sock.port())));
        }
        iroh::EndpointAddr { id: server.id(), addrs }
    }

    /// A serialized address that parses, sanitizes to itself, and has NOTHING to dial: a real
    /// endpoint id with an empty transport-address set. Callers outside this module use it to reach
    /// the statement after a guard without emitting a packet — the dial has no address, no relay and
    /// no discovery to ask, so it fails locally. It lives HERE, rather than beside its caller,
    /// because naming `iroh::` in a command module trips the INV-4′ CI sweep: that sweep defines the
    /// transport surface as "the files that name iroh", and this file is on the list.
    pub(crate) fn undialable_addr_json() -> String {
        let addr = iroh::EndpointAddr::from_parts(
            iroh::SecretKey::generate().public(),
            std::iter::empty::<iroh::TransportAddr>(),
        );
        serde_json::to_string(&addr).expect("EndpointAddr serializes")
    }

    /// QURATOR-113 #20 — the peer-authored ticket address is sanitized: only globally-routable
    /// transport addresses survive, so a stranger who answers a manifest ask cannot make this node
    /// dial an internal host. Pure (no endpoint bound): `sanitize_node_addr` runs the exact filter
    /// the redeem command applies before the dial.
    #[test]
    fn sanitize_node_addr_drops_non_global_transport_addrs() {
        let id = iroh::SecretKey::generate().public();
        let addrs: BTreeSet<iroh::TransportAddr> = [
            // Public — kept.
            iroh::TransportAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9999)),
            iroh::TransportAddr::Relay("https://relay.example.com".parse().unwrap()),
            // RFC1918 / loopback / link-local / CGNAT — dropped.
            iroh::TransportAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), 9999)),
            iroh::TransportAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9999)),
            iroh::TransportAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)), 9999)),
            iroh::TransportAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), 9999)),
            // Internal relay hosts — dropped.
            iroh::TransportAddr::Relay("https://127.0.0.1:443".parse().unwrap()),
            iroh::TransportAddr::Relay("https://localhost:443".parse().unwrap()),
        ]
        .into_iter()
        .collect();
        let addr = iroh::EndpointAddr::from_parts(id, addrs);

        let cleaned: iroh::EndpointAddr =
            serde_json::from_str(&sanitize_node_addr(&serde_json::to_string(&addr).unwrap()).unwrap())
                .unwrap();

        let expected: BTreeSet<iroh::TransportAddr> = [
            iroh::TransportAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9999)),
            iroh::TransportAddr::Relay("https://relay.example.com".parse().unwrap()),
        ]
        .into_iter()
        .collect();
        assert_eq!(cleaned.id, addr.id, "the endpoint id survives sanitizing");
        assert_eq!(cleaned.addrs, expected, "only the globally-routable addresses survive");
    }

    /// The id is preserved even when every address is dropped — a ticket that names only internal
    /// hosts sanitizes to "no dialable address", which the dial then fails on cleanly (no fallback to
    /// the dropped addresses).
    #[test]
    fn sanitize_node_addr_keeps_the_id_when_every_address_is_dropped() {
        let id = iroh::SecretKey::generate().public();
        let addr = iroh::EndpointAddr::from_parts(
            id,
            [iroh::TransportAddr::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                9999,
            ))],
        );
        let cleaned: iroh::EndpointAddr =
            serde_json::from_str(&sanitize_node_addr(&serde_json::to_string(&addr).unwrap()).unwrap())
                .unwrap();
        assert_eq!(cleaned.id, addr.id, "the id survives even when no address is dialable");
        assert!(cleaned.addrs.is_empty(), "a loopback-only ticket leaves no dialable address");
    }

    /// A manifest built the way production builds one: a real listing JSON through
    /// `split_listing` → `build_manifest_envelope`, for a named collection — so a test can build a
    /// manifest that is entirely valid but describes the WRONG collection (the slug-binding case).
    /// **Not a hand-written fixture** — that is the W7.2 blind spot Suite MAN was added to close, and
    /// a transport test fed hand-built parts would prove nothing about what production puts on the
    /// wire.
    ///
    /// (The `real_payload(entries)` wrapper that defaulted the slug to `"big"` was removed when W4
    /// dropped `mod transport`'s `#[allow(dead_code)]` — the allow was the only thing making an
    /// uncalled helper look used.)
    pub(crate) fn real_payload_for(slug: &str, entries: usize) -> ManifestPayload {
        let id = Identity::generate();
        let items: Vec<String> = (0..entries)
            .map(|i| {
                format!(
                    r#"{{"name":"a-file-with-a-realistically-long-name-{i:07}.mkv","path":"season-{}/a-file-with-a-realistically-long-name-{i:07}.mkv","item_type":"file","size":{},"children":[]}}"#,
                    i % 40,
                    1_000_000 + i
                )
            })
            .collect();
        let listing = format!(
            r#"{{"schema_v":1,"slug":"{slug}","name":"Fixture","entries":[{}]}}"#,
            items.join(",")
        );
        let parts = split_listing(slug, &listing, 40_000).expect("split the listing");
        let plaintexts: Vec<String> = parts.into_iter().map(|p| p.json).collect();
        let env = build_manifest_envelope(
            &id,
            slug,
            &[7u8; 32],
            "fp-real",
            1_700_000_000,
            &plaintexts,
        )
        .expect("build the envelope");
        ManifestPayload::seal(&env).expect("seal the payload")
    }

    /// A `ManifestSource` over a single approved request.
    pub(crate) struct TestSource {
        pub ticket: TransportTicket,
        pub payload: ManifestPayload,
        /// Rig the source to answer with this payload regardless of the slug asked for — the
        /// "lookup fell through to the wrong collection" bug, which no honest implementation would
        /// write on purpose and which the slug binding exists to catch.
        pub serve_instead: Mutex<Option<ManifestPayload>>,
    }

    impl TestSource {
        pub(crate) fn new(ticket: TransportTicket, payload: ManifestPayload) -> Arc<Self> {
            Arc::new(Self {
                ticket,
                payload,
                serve_instead: Mutex::new(None),
            })
        }
    }

    impl ManifestSource for TestSource {
        fn payload(&self, ticket: &TransportTicket) -> Result<ManifestPayload> {
            if let Some(rigged) = self.serve_instead.lock().unwrap().clone() {
                return Ok(rigged);
            }
            if ticket.request_id != self.ticket.request_id {
                return Err(anyhow!("unknown request {}", ticket.request_id));
            }
            Ok(self.payload.clone())
        }
    }

    // TestSource's `consumed`/`fail_receipt` rigs and the `Rendezvous` — DELETED 2026-09-03,
    // QURATOR-177 Option E, with the `issued()`/`record_consumed()` methods they exercised. The
    // replay/poison tests that drove them are deleted below, each with the ruling named.

    /// Spawn the production accept loop for `source` on a fresh loopback endpoint, and return the
    /// endpoint plus a ticket addressed at it.
    async fn spawn_plane(
        entries: usize,
        slug: &str,
    ) -> (iroh::Endpoint, Arc<TestSource>, TransportTicket) {
        // Built FOR this slug: the payload and the ticket must agree, or a test would be exercising
        // the slug-binding refusal instead of the path it means to test. (Every test was — the
        // fixture said "big" while the tickets said "small", and the new binding check found it.)
        spawn_plane_inner(entries, slug).await
    }

    async fn spawn_plane_inner(
        entries: usize,
        slug: &str,
    ) -> (iroh::Endpoint, Arc<TestSource>, TransportTicket) {
        let payload = real_payload_for(slug, entries);
        let server = bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
        // The ticket carries the LOOPBACK address, not `endpoint.addr()` — a wildcard bound socket
        // is not dialable, and `ticket_node_addr` is exercised separately.
        let addr = serde_json::to_string(&loopback_addr(&server)).unwrap();
        let ticket = TransportTicket::issue("req-1", slug, &addr, 1_700_000_000, Some("nonce-1"));
        let source = TestSource::new(ticket.clone(), payload);

        let accept_ep = server.clone();
        let plane = ManifestPlane::new(source.clone());
        tokio::spawn(async move {
            while let Some(incoming) = accept_ep.accept().await {
                let Ok(accepting) = incoming.accept() else { continue };
                let Ok(conn) = accepting.await else { continue };
                // Spawned, not awaited — a serial loop cannot exhibit the concurrent-redeem race the
                // in-flight set exists to stop, and a test that cannot exhibit it proves nothing.
                let plane = plane.clone();
                tokio::spawn(async move { let _ = plane.serve(conn).await; });
            }
        });
        (server, source, ticket)
    }

    /// **The plane's own wire freeze.** `hb-core::wire_freeze` cannot reach these — it does not
    /// depend on `hb-app` — so the framing would otherwise be the one new wire surface M18 added
    /// with nothing pinning it. It is a genuine contract: two peers that disagree about the ALPN,
    /// the status bytes, or the prefix width cannot talk.
    ///
    /// Weaker than the durable-event freeze on purpose, and the difference is worth stating: a
    /// mismatch here refuses a *live connection*, which is a clean failure a user can retry. It
    /// does not orphan anything already published. So this pins against an accidental rename, not
    /// against an unfixable fork — the ALPN carries its own `/1` for the deliberate case.
    #[tokio::test]
    async fn the_planes_framing_is_pinned() {
        assert_eq!(MANIFEST_ALPN, b"/hoardbook/manifest/1", "the plane's ALPN is a wire contract");
        assert_eq!(STATUS_OK, 0, "the ok status byte is a wire contract");
        assert_eq!(STATUS_REFUSED, 1, "the refused status byte is a wire contract");
        // Distinct from the retired file plane, so a peer still speaking it finds nothing listening.
        assert_ne!(MANIFEST_ALPN, b"/hoardbook/xfer/1");

        // The prefix pinned by its ACTUAL BYTES, not by `size_of::<u32>() == 4` — which is a fact
        // about Rust, true no matter what this module does, and so proved nothing about the wire.
        let mut wire = Vec::new();
        write_framed(&mut wire, b"hi").await.unwrap();
        assert_eq!(
            wire,
            vec![2, 0, 0, 0, b'h', b'i'],
            "the length prefix is 4 bytes little-endian, then the body — a width or endianness \
             change is a protocol change"
        );
        // And it reads back as the same frame it wrote (the encode/decode pair, not just encode).
        assert_eq!(read_framed(std::io::Cursor::new(wire), 16).await.unwrap(), b"hi");
    }

    // `await_receipt` — DELETED 2026-09-03, QURATOR-177 Option E, with the `consumed` rig it
    // polled. There is no receipt to wait for; tests that only needed the owner's side to finish
    // settling now just run to the fetch's own completion.

    /// **The acceptance test: a multi-MB manifest crosses a real connection.** 10,000 entries of
    /// realistic long filenames through the real splitter — ~60 parts and ~2.4 MB of sealed
    /// envelope, which is many QUIC frames rather than the single-datagram case a small fixture
    /// would test.
    ///
    /// 10,000 is not arbitrary: it is the owner's stated human browse limit, the number MECH-2's
    /// ceiling was derived from. **Measured here, that shape is ~245 bytes of sealed envelope per
    /// entry, not the ~70 MECH-2 assumed** — realistic nested paths and long names cost more than
    /// bare filenames. So the 16 MiB ceiling carries ~68k such entries, well short of what the
    /// ~70-byte assumption implied. The ceiling still sits
    /// comfortably above the browseable band, which is what it was pinned to; the headroom is just
    /// smaller than the derivation's arithmetic implied.
    #[tokio::test]
    async fn a_multi_megabyte_manifest_crosses_a_real_connection() {
        tokio::time::timeout(MULTI_MB_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(10_000, "big").await;
            let payload = source.payload.clone();
            assert!(
                payload.len() > 2 * 1024 * 1024,
                "the fixture must actually be multi-MB, got {} bytes",
                payload.len()
            );
            let expected = payload.envelope().unwrap();

            let client = bind_local_endpoint(&rand::random(), vec![]).await;
            let got = fetch_manifest(&client, &ticket, |_| Ok(())).await.expect("fetch the manifest");

            assert_eq!(got.len(), payload.len(), "the whole payload arrived");
            assert_eq!(
                got.envelope().unwrap(),
                expected,
                "the envelope survived the plane byte-for-byte"
            );

            // (Until QURATOR-177 Option E this also awaited the owner's receipt and asserted the
            // ticket was consumed exactly once; the receipt rig is deleted with the ledger.)

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    // `a_spent_ticket_is_refused_on_a_second_connection` — DELETED 2026-09-03, QURATOR-177
    // Option E (owner ruling: the issued-ticket ledger is deleted; authorization is the standing
    // grant checked at ASK time, and the ticket is address delivery). A second redemption attempt
    // is no longer a replay to refuse — it either never fires (the collection's fingerprint is
    // unchanged, so no refetch ask originates; that absence IS the anti-nuisance control) or it is
    // a legitimate fetch (the fingerprint changed) and must succeed. Durable replay protection and
    // the audit trail were deliberately given up with the ledger. The trigger condition is pinned
    // by the WAN-M row `a_refetch_fires_only_on_a_fingerprint_change_and_succeeds_when_it_does`.


    // DELETED 2026-09-03, QURATOR-177 (owner ruling: *"Blocks should only block interaction i.e.
    // chats, it should not meaningfully affect other traffic."*):
    // `a_redeemer_blocked_after_approval_is_refused_then_restored` pinned the withdrawn
    // serve-path standing refusal end to end — rig `TestSource.standing` to `Blocked` mid-flight,
    // assert the fetch errors naming the standing check, restore to `Good`, assert it succeeds.
    // Its replacement, pinning the OPPOSITE (a blocked redeemer holding a valid unspent ticket is
    // served), is `a_blocked_redeemer_with_a_valid_ticket_is_still_served` below. The
    // `TestSource.standing` rig itself is gone; the drain test in `conn.rs` now forces the
    // refusal path with a consumed ticket instead.

    /// **Blocking does not gate the serve path** (owner ruling 2026-09-03, QURATOR-177: *"Blocks
    /// should only block interaction i.e. chats, it should not meaningfully affect other
    /// traffic."*). This replaces the deleted `a_redeemer_blocked_after_approval_is_refused_then_
    /// restored`, which pinned the opposite. There is no standing check anywhere on the serve path
    /// and no issued-ticket record left that could carry one, so a redeemer whose contact the owner
    /// has blocked — or removed, or declined — is served a manifest it holds a valid ticket for,
    /// exactly as a good-standing redeemer is. Blocking's remaining enforcement is chat/DM
    /// acceptance, pinned in `commands/chat.rs`
    /// (`proactive_block_refuses_later_dms_and_unblock_restores_acceptance`).
    ///
    /// The assertion is the plane's own observable: the fetch SUCCEEDS — not merely "no
    /// standing-naming refusal", which a compile error would also satisfy. (The string "no longer
    /// an approved contact" is deleted from the codebase, so a re-introduced standing check could
    /// not re-use it verbatim.)
    ///
    /// MUTATION (P-10) — the orchestrator applies this and must see this test red: in
    /// `serve_manifest_stream` (this file), immediately after the `validate_redemption` call,
    /// insert any refusal, e.g.
    ///   if source_blocking_hint(&req.ticket) { refuse(&mut send, "no longer an approved contact").await?; return Ok(None); }
    /// where `source_blocking_hint` is a new `fn(&TransportTicket) -> bool { true }` in this file —
    /// i.e. ANY re-introduced gate on the serve path reds this test, because the fetch that
    /// expects success errors. (Restoring the old arm verbatim needs the issued-ticket record's
    /// standing field, which no longer exists — that is the point of deleting the record rather
    /// than ignoring it.)
    #[tokio::test]
    async fn a_blocked_redeemer_with_a_valid_ticket_is_still_served() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // No standing rig exists to set: blocking a contact changes nothing the plane can see.
            // The blocked redeemer holds a valid ticket, so the fetch must succeed outright.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("a blocked redeemer holding a valid ticket must still be served");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **The devtest 2026-08-27 end-to-end regression, over real QUIC.**
    ///
    /// The owner clicked "Send the full list" with both dev machines online and the asker got the
    /// forged-ticket refusal. The whole reason the existing suite missed it is visible right here:
    /// every other test in this file calls `fetch_manifest` with the ticket EXACTLY as issued, but
    /// the production asker does not — `redeem_manifest_ticket` rewrites `node_addr` through
    /// `sanitize_node_addr` first, and on a LAN that always changes the string. So this test does
    /// what production does: sanitize, then redeem. Against the old `!=` comparison it fails with
    /// REFUSAL_NO_MATCH.
    ///
    /// The dial still uses the endpoint's real address (loopback, which the sanitizer would strip),
    /// so the ticket the SERVER compares is the sanitized one while the connection is the honest
    /// one — exactly the production split, where the asker dials a surviving address and presents a
    /// ticket whose address list it pruned.
    #[tokio::test]
    async fn a_redeemer_that_sanitized_the_node_addr_is_still_served() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // What production does to the ticket before presenting it.
            let sanitized_addr = sanitize_node_addr(&ticket.node_addr)
                .expect("the SSRF guard accepts a well-formed node address");
            assert_ne!(
                sanitized_addr, ticket.node_addr,
                "the fixture must actually be rewritten by the guard, or this test proves nothing \
                 (a loopback endpoint's addresses are exactly what it strips)"
            );
            let mut presented = ticket.clone();
            presented.node_addr = sanitized_addr;

            // Dial with the honest address, then run the REAL request/response body against the
            // sanitized ticket. `fetch_over_connection` is the production function `fetch_manifest`
            // delegates to — it builds the FetchRequest and evaluates the reply — so this ends where
            // production ends. Only the dial is separated, because a loopback endpoint has no
            // relay or discovery to reach through once the guard has stripped its 127.0.0.1
            // address; on a real network the surviving public address is what production dials.
            let conn = client
                .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                .await
                .expect("dial the plane on its honest address");
            let mut got = None;
            let out = fetch_over_connection(&conn, &presented, |p| {
                got = Some(p.as_bytes().len());
                Ok(())
            }, None)
            .await;
            conn.close(0u32.into(), b"");
            out.expect("a redeemer that sanitized the address it was handed must still be served");
            assert!(got.is_some_and(|n| n > 0), "the manifest actually arrived");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **A ticket address that answers nothing is bounded at the dial** (QURATOR-257). The
    /// blackhole is a `UdpSocket` bound on loopback and never read: bound-but-silent means the
    /// kernel buffers (then drops) every QUIC Initial into that socket's receive queue — **no
    /// ICMP port-unreachable is generated, because the port is open**. That is the distinction
    /// this test turns on: a *refused* dial (nothing bound at the port) can fail in milliseconds
    /// off an OS signal, which would make any "it returned quickly" assertion decorative; a
    /// *silent* address produces no OS-level signal at all, so the only thing that can return
    /// this call in bounded time is our own dial deadline. The 2s deadline is injected through the
    /// `_inner` seam; the assertion is on the error naming the DIAL step specifically, under the
    /// same `dial the manifest plane` context a refused dial would carry — the "(waited 2s)"
    /// deadline message is what distinguishes a timeout from a refusal, which would surface
    /// iroh's own connect error under that context instead.
    ///
    /// MUTATION (P-10) — the orchestrator applies this and must see this test red: in
    /// `fetch_manifest_with_progress_inner` (this file, production line 687 — the `dial_deadline`
    /// first argument of the `with_deadline(` wrapping `endpoint.connect(addr, MANIFEST_ALPN)`),
    /// change `dial_deadline` to `std::time::Duration::from_secs(600)`. The 2s this test passes
    /// in is then ignored, the silent dial blackholes past the outer `TEST_TIMEOUT`, and the
    /// `.expect` on the bounded return reds. (Anchored to the production call, not this comment.)
    #[tokio::test]
    async fn a_silent_ticket_address_is_bounded_by_the_dial_deadline() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            // Held alive for the whole test: dropping the socket would close the port and invite
            // the ICMP-driven fast refusal this test deliberately is not about.
            let blackhole = std::net::UdpSocket::bind(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
            ))
            .expect("bind the silent address");
            let port = blackhole.local_addr().expect("read the silent port").port();

            // A well-formed ticket whose only defect is where it points — same shape
            // `spawn_plane_inner` mints, at a node id that never existed and our silent port.
            let addr = iroh::EndpointAddr::from_parts(
                iroh::SecretKey::generate().public(),
                [iroh::TransportAddr::Ip(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    port,
                ))],
            );
            let addr_json = serde_json::to_string(&addr).expect("EndpointAddr serializes");
            let ticket =
                TransportTicket::issue("req-dial", "small", &addr_json, 1_700_000_000, Some("nonce-1"));

            let client = bind_local_endpoint(&rand::random(), vec![]).await;
            let started = std::time::Instant::now();
            let err = fetch_manifest_with_progress_inner(
                &client,
                &ticket,
                |_| Ok(()),
                None,
                std::time::Duration::from_secs(2),
            )
            .await
            .expect_err("a silent address must fail, never deliver");
            let elapsed = started.elapsed();

            let chained = format!("{err:#}");
            assert!(
                chained.contains("dial the manifest plane for small"),
                "the dial's context survives the deadline wrapper — timeout or not, the caller \
                 still learns which slug's dial failed: {chained}"
            );
            assert!(
                chained.contains("never answered the dial"),
                "the error names the DIAL deadline specifically — the string that distinguishes \
                 our bound from an OS-level refusal (iroh's connect error under the same context, \
                 with no '(waited 2s)' tail): {chained}"
            );
            assert!(
                elapsed < TEST_TIMEOUT,
                "the dial deadline fired first, not the test's outer bound ({elapsed:.1?})"
            );

            client.close().await;
        })
        .await
        .expect("test timed out");
    }

    // `an_unknown_request_and_a_forged_ticket_are_indistinguishable` — DELETED 2026-09-03,
    // QURATOR-177 Option E. The indistinguishability was a property OF THE LEDGER: the plane knew
    // which requests it had issued for, and refused both an unknown request and a right-request/
    // wrong-ticket pair with the same `REFUSAL_NO_MATCH` so a prober learned nothing about which
    // requests existed. With the ledger deleted the plane no longer knows which requests exist —
    // an unresolvable ticket is refused at payload resolution and a slug mismatch at the byte
    // binding, with different messages, and that is inherent to Option E: the ticket is a bearer
    // capability, and the payload it buys is browse-key sealed ciphertext. The surviving
    // same-refusal pin (malformed vs wrong-request-binding, both `REFUSAL_NO_MATCH`) lives in
    // hb-core's `validate_redemption` tests (`ticket.rs`,
    // `a_ticket_is_bound_to_its_own_request`).


    /// A reader that yields exactly the bytes it was given and then **panics** if read again.
    ///
    /// This is what makes the ordering test real. A `Cursor` hits EOF instead, so a check that ran
    /// *after* the body read would still return an error — the right outcome for the wrong reason,
    /// and the test would pass either way (Codex's point, and correct). Panicking on the read that
    /// must never happen turns "was the body read?" into an observable fact.
    struct PanicAfter(std::io::Cursor<Vec<u8>>);

    impl tokio::io::AsyncRead for PanicAfter {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let exhausted = self.0.position() as usize >= self.0.get_ref().len();
            assert!(
                !exhausted,
                "read_framed read past the length prefix — the ceiling was checked AFTER the body, \
                 which is the ordering this test exists to forbid"
            );
            std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    /// **The framing-layer ceiling check** — the piece W3 explicitly left to W1.
    ///
    /// The declared length is refused on the four bytes that declared it, proven two ways: the reader
    /// panics if anything reads past the prefix, and the whole call is bounded so a block also fails.
    /// The reason must point at export, because a manifest over the ceiling is a "too big, export
    /// it", not a corruption.
    #[tokio::test]
    async fn the_framing_refuses_an_over_cap_declared_length_before_reading_the_body() {
        let mut wire = Vec::new();
        let over = (MANIFEST_MAX_TRANSPORT_BYTES + 1) as u32;
        wire.extend_from_slice(&over.to_le_bytes()); // ...and deliberately no body.

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_framed(PanicAfter(std::io::Cursor::new(wire)), MANIFEST_MAX_TRANSPORT_BYTES),
        )
        .await
        .expect("the ceiling must be checked on the declared length, not after reading the body")
        .expect_err("an over-cap declared length is refused");

        let msg = err.to_string();
        assert!(msg.contains("over the"), "the refusal names the ceiling, got: {msg}");
        assert!(msg.to_lowercase().contains("export"), "and names the route out, got: {msg}");

        // Exactly at the ceiling is a legitimate declared length — the boundary is inclusive here
        // too, matching `ManifestPayload::bound`. (Reading the body then fails on EOF, which is a
        // *different* error: the point is that the length itself was accepted.)
        let mut at = Vec::new();
        at.extend_from_slice(&(MANIFEST_MAX_TRANSPORT_BYTES as u32).to_le_bytes());
        let err = read_framed(std::io::Cursor::new(at), MANIFEST_MAX_TRANSPORT_BYTES)
            .await
            .expect_err("no body was sent, so the read fails");
        assert!(
            !err.to_string().contains("over the"),
            "a declared length exactly at the ceiling must be accepted, got: {err}"
        );
    }

    /// **QURATOR-159 — byte progress for the manifest transport.**
    ///
    /// `read_framed_with_progress` over a known body must report monotonically increasing
    /// cumulative bytes ending exactly at the declared total, the frame must round-trip intact,
    /// and the over-cap refusal must still fire before the sink is ever called (the
    /// before-allocation ordering the test above pins, seen from the sink's side).
    ///
    /// **Mutation proof (P-10) — the orchestrator applies this, not this lane.** Both edits are in
    /// `read_body_chunked` above. (1) Change `sink(done as u64, total);` to `sink(total, total);`
    /// — reporting the declared total instead of the cumulative count. With the body spanning
    /// more than one chunk, every sample then carries the same `received`, so the
    /// strictly-increasing assertion (`cumulative bytes strictly increase`) REDS. (2) Delete the
    /// `if let Some(sink) = progress { sink(done as u64, total); }` block outright — the
    /// `the sink fired at least once` assertion REDS. The refusal leg (sink never fires on an
    /// over-cap declared length) is covered by the existing `PanicAfter` ordering test above,
    /// which already reds if the ceiling moves after the body read.
    #[tokio::test]
    async fn read_framed_reports_monotonic_progress_ending_at_the_total() {
        use std::io::Cursor;

        // Larger than one 64 KiB chunk so the chunked loop runs more than once — a single-pass
        // read would report once and prove nothing about monotonicity.
        let body: Vec<u8> = (0..6 * 64 * 1024 + 1).map(|i| (i % 251) as u8).collect();
        let mut wire = Vec::new();
        write_framed(&mut wire, &body).await.unwrap();

        let samples: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());
        let sink = |received: u64, total: u64| {
            samples.lock().unwrap().push((received, total));
        };
        let got = read_framed_with_progress(
            Cursor::new(wire),
            MANIFEST_MAX_TRANSPORT_BYTES,
            Some(&sink),
        )
        .await
        .expect("the frame reads back");
        assert_eq!(got, body, "chunked reading returns exactly the declared body");

        let s = samples.into_inner().unwrap();
        assert!(!s.is_empty(), "the sink fired at least once");
        assert!(
            s.iter().all(|&(_, t)| t == body.len() as u64),
            "every sample carries the declared total"
        );
        assert!(
            s.iter().all(|&(r, _)| r > 0 && r <= body.len() as u64),
            "cumulative bytes stay in (0, total]"
        );
        assert!(
            s.windows(2).all(|w| w[0].0 < w[1].0),
            "cumulative bytes strictly increase: {s:?}"
        );
        assert_eq!(
            s.last().copied(),
            Some((body.len() as u64, body.len() as u64)),
            "the final sample is received == total — the UI's completion signal"
        );
        // Over-cap still refuses with the sink never firing: the ceiling is evaluated on the
        // declared length, before a body byte is read or reported.
        let over: Vec<u8> = ((MANIFEST_MAX_TRANSPORT_BYTES + 1) as u32)
            .to_le_bytes()
            .to_vec();
        let fired = std::sync::atomic::AtomicUsize::new(0);
        let err = read_framed_with_progress(
            Cursor::new(over),
            MANIFEST_MAX_TRANSPORT_BYTES,
            Some(&|_, _| {
                fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .await
        .expect_err("an over-cap declared length is refused");
        assert!(err.to_string().contains("over the"), "got: {err}");
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the sink must not fire for a refused frame"
        );
    }

    // `two_simultaneous_redemptions_deliver_the_manifest_once` — DELETED 2026-09-03, QURATOR-177
    // Option E. Its claim ("exactly one of two simultaneous redemptions may succeed") WAS the
    // spent bit under concurrency: the `Rendezvous` forced both connections inside `issued()`
    // before either could write `already_consumed`. The ledger that fed it is deleted, and a
    // second redemption is a legitimate fetch under Option E — two sequential redemptions both
    // succeed, and which of two racing ones wins the in-flight claim is not a property anyone
    // rules on. What the in-flight set still does — and the replacement test below pins — is
    // concurrency control: one handler per `request_id` at a time, so two streams never interleave
    // their payload writes.

    /// **The in-flight claim is concurrency control, not authorization** (owner ruling 2026-09-03,
    /// QURATOR-177 Option E). While one redemption of a request is mid-flight — here, payload
    /// delivered and the handler waiting on the ACK — a second connection presenting the same
    /// request is refused with the in-flight message; once the first connection ends (the asker
    /// vanished without ACKing, so the handler errors out and drops its claim) the honest retry
    /// succeeds. A second redemption is a LEGITIMATE fetch under Option E; what the claim prevents
    /// is two handlers interleaving their writes for one `request_id` inside one process, never a
    /// second delivery as such.
    ///
    /// MUTATION (P-10) — the orchestrator applies this and must see this test red: in
    /// `ManifestPlane::serve` (this file), make the `None` arm of the `claim` match a pass-through
    /// (delete the `refuse(... "already being redeemed" ...)` call so the second stream is served
    /// mid-flight) — the mid-flight `expect_err` then succeeds and the assertion on its error
    /// message reds.
    #[tokio::test]
    async fn a_second_connection_while_a_redemption_is_in_flight_is_refused_then_retry_succeeds() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // Hold the claim open: send a valid request, read the payload, then stop just short
            // of the ACK — the handler sits in its ACK wait with the claim held.
            {
                let conn = client
                    .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                    .await
                    .expect("dial");
                let (mut send, mut recv) = conn.open_bi().await.unwrap();
                let req =
                    FetchRequest { request_id: ticket.request_id.clone(), ticket: ticket.clone() };
                write_framed(&mut send, &serde_json::to_vec(&req).unwrap()).await.unwrap();
                assert_eq!(recv.read_u8().await.unwrap(), STATUS_OK, "the owner serves it");
                let bytes = read_framed(&mut recv, MANIFEST_MAX_TRANSPORT_BYTES).await.unwrap();
                assert!(!bytes.is_empty(), "the whole payload was read");

                // Mid-flight: a second connection for the same request is refused, naming the
                // in-flight claim — the concurrency control doing its only remaining job.
                let err = fetch_manifest(&client, &ticket, |_| Ok(()))
                    .await
                    .expect_err("a second stream while the first is mid-flight must be refused");
                assert!(
                    err.to_string().contains("already being redeemed"),
                    "the refusal names the in-flight claim, got: {err}"
                );

                // Now vanish without ACKing: the handler's ACK read errors, the claim is dropped.
                conn.close(0u32.into(), b"gone");
            }

            // Room for the handler to observe the close and drop its claim.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // The claim is free and the honest retry succeeds — a second redemption is a
            // legitimate fetch (Option E), never a replay to refuse.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("the retry after the in-flight handler ended must succeed");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **Codex finding 4 — the payload must match the ticket's collection.**
    ///
    /// A source that answers with the wrong collection is the realistic bug here (a slug lookup that
    /// falls through to a default, say), and the wrong manifest is perfectly *self-consistent*, so
    /// nothing downstream would notice. Both sides check; this exercises the owner's side. Since
    /// QURATOR-242 the wire refusal is the uniform `REFUSAL_NO_MATCH`; the two slugs live in the
    /// local `tracing::warn!` where the operator can see what happened.
    #[tokio::test]
    async fn a_manifest_for_the_wrong_collection_is_refused_and_costs_nothing() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            // The ticket says "small"; the source is rigged to hand back a manifest for "big".
            let (server, source, ticket) = spawn_plane(50, "small").await;
            *source.serve_instead.lock().unwrap() = Some(real_payload_for("big", 50));
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            let err = fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect_err("a manifest for another collection must be refused");
            let msg = err.to_string();
            // **Asserting the OWNER's wording, not just "some refusal".** Both sides check, so a test
            // that accepted either message would stay green with the owner's check deleted — the
            // asker's would catch it and the test could not tell the difference. Since QURATOR-242
            // the owner's refusal is the uniform `REFUSAL_NO_MATCH`; the asker's own wrong-collection
            // message ("refusing the peer's manifest: ...") is different text, so this still pins
            // WHICH side refused. The two slugs are in the owner's `tracing::warn!`, not on the wire.
            assert!(
                msg.contains(REFUSAL_NO_MATCH),
                "the OWNER must refuse before sending, got: {msg}"
            );
            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **The other direction of the slug binding: a LYING peer.**
    ///
    /// The owner's check protects against its own lookup bug; this one protects against a peer that
    /// is simply hostile, which is the direction that actually matters — we do not get to assume the
    /// remote validated anything. A hand-rolled server sends a perfectly valid, correctly signed
    /// envelope for a collection we did not ask about. Nothing about the bytes is malformed, so only
    /// the ticket binding can reject them.
    #[tokio::test]
    async fn a_lying_peer_cannot_hand_us_a_manifest_for_another_collection() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let liar = bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
            let addr = serde_json::to_string(&loopback_addr(&liar)).unwrap();
            // We hold a ticket for "mine"; the peer will answer with a valid manifest for "theirs".
            let ticket = TransportTicket::issue("req-1", "mine", &addr, 1_700_000_000, Some("nonce-1"));
            let wrong = real_payload_for("theirs", 40);

            let accept_ep = liar.clone();
            tokio::spawn(async move {
                while let Some(incoming) = accept_ep.accept().await {
                    let Ok(accepting) = incoming.accept() else { continue };
                    let Ok(conn) = accepting.await else { continue };
                    let wrong = wrong.clone();
                    tokio::spawn(async move {
                        let Ok((mut send, mut recv)) = conn.accept_bi().await else { return };
                        let _ = read_framed(&mut recv, FETCH_REQUEST_MAX_FRAME).await;
                        let _ = send.write_u8(STATUS_OK).await;
                        let _ = write_framed(&mut send, wrong.as_bytes()).await;
                        let _ = send.shutdown().await;
                        drain_connection(&conn).await;
                    });
                }
            });

            let client = bind_local_endpoint(&rand::random(), vec![]).await;
            let err = fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect_err("a manifest for a collection we did not ask about must be refused");
            let msg = err.to_string();
            assert!(
                msg.contains("refusing the peer's manifest"),
                "the ASKER must refuse it, got: {msg}"
            );
            assert!(
                msg.contains("theirs") && msg.contains("mine"),
                "and name what it got versus what it asked for, got: {msg}"
            );

            client.close().await;
            liar.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **An unacknowledged transfer is treated as undelivered — and the retry is a legitimate
    /// fetch** (QURATOR-177 Option E, owner ruling 2026-09-03).
    ///
    /// The asker here speaks the protocol by hand: it sends a valid request, reads the whole
    /// payload, and then walks away without acknowledging — the shape of a client that crashed
    /// mid-parse, or read half the bytes and died. The owner's handler errors out ("asker closed
    /// before acknowledging") rather than recording a delivery: the ACK is the assertion that a
    /// usable manifest landed, so its absence means undelivered. Until Option E the point of that
    /// was consumed-on-success — an unacknowledged transfer must not burn the ticket — and the
    /// receipt that pinned it is deleted with the ledger. What survives is the honest accounting
    /// plus the Option E fact the retry now pins directly: a repeat fetch is never refused as a
    /// replay, because replay is not a concept the serve path knows any more.
    #[tokio::test]
    async fn a_peer_that_never_acknowledges_is_undelivered_and_the_retry_succeeds() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            {
                let conn = client
                    .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                    .await
                    .expect("dial");
                let (mut send, mut recv) = conn.open_bi().await.unwrap();
                let req =
                    FetchRequest { request_id: ticket.request_id.clone(), ticket: ticket.clone() };
                write_framed(&mut send, &serde_json::to_vec(&req).unwrap()).await.unwrap();
                assert_eq!(recv.read_u8().await.unwrap(), STATUS_OK, "the owner serves it");
                let bytes = read_framed(&mut recv, MANIFEST_MAX_TRANSPORT_BYTES).await.unwrap();
                assert!(!bytes.is_empty(), "the whole payload was read");
                // ...and now vanish, without acknowledging.
                conn.close(0u32.into(), b"gone");
            }

            // The retry is a legitimate fetch — never a replay to refuse (Option E).
            fetch_manifest(&client, &ticket, |_| Ok(())).await.expect("the retry must succeed");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **The acceptance gate runs BEFORE the acknowledgement — a manifest the caller rejects is
    /// never acknowledged, and a retry is a legitimate fetch** (QURATOR-177 Option E).
    ///
    /// The sibling of `a_peer_that_never_acknowledges_is_undelivered_and_the_retry_succeeds`, one
    /// layer up. That one covers a peer that never answers; this one covers a peer that answers
    /// *correctly at the wire level* and is still rejected by the app: `from_wire` proves the
    /// envelope is self-consistent, under the ceilings, and declares the ticket's slug — it cannot
    /// prove the signature is the browsed peer's, that the body decrypts under our browse-key, or
    /// that the tree is complete. Those checks live in `commands::browse::open_manifest`, behind
    /// the contact store.
    ///
    /// Run the gate after `fetch_manifest` returns and the ACK is already gone: the owner counts a
    /// delivery that never landed, the asker has nothing usable, and the human has to ask again.
    /// That is the `6691377` "consumed on written" defect recurring one layer up — the spent bit
    /// that used to make it worse is deleted with the ledger (Option E), but the mis-accounting is
    /// the surviving harm, and the gate-before-ACK ordering is what prevents it.
    #[tokio::test]
    async fn a_manifest_the_caller_rejects_is_not_acknowledged_and_a_retry_succeeds() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // A gate that refuses everything — standing in for a failed author pin, a browse-key that
            // does not decrypt, or an incomplete tree. The wire exchange itself is entirely valid.
            let err = fetch_manifest(&client, &ticket, |_| Err(anyhow!("author pin failed")))
                .await
                .expect_err("a rejected manifest is not a successful redemption");
            assert!(
                err.to_string().contains("author pin failed"),
                "the caller's reason must survive to the user, got: {err}"
            );

            // The retry succeeds — a repeat fetch is a legitimate fetch (Option E), and the
            // earlier rejection never acknowledged anything, so nothing was mis-counted.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("the ticket is still good after a rejection");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    // `a_failed_receipt_write_poisons_the_ticket_instead_of_allowing_a_replay` — DELETED
    // 2026-09-03, QURATOR-177 Option E, together with the `poisoned` set and the `fail_receipt`
    // rig that drove it. It pinned one-ticket-one-delivery at exactly the moment a durable write
    // failed; with the issued-ticket ledger deleted there is no receipt write that can fail and no
    // spent bit a failed write could desynchronize. Per-process poisoning of a request whose
    // receipt could not be persisted is meaningless without the receipt.


    /// `ticket_node_addr` and `parse_node_addr` are one format, not two: the ticket's opaque string
    /// round-trips back to the same dialable address. This is the seam where hb-core's
    /// deliberate ignorance of iroh meets the transport.
    #[tokio::test]
    async fn the_tickets_opaque_node_addr_round_trips_to_a_dialable_address() {
        let ep = bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
        let ticket = issue_ticket(&ep, "req-1", "slug", 1_700_000_000, None).unwrap();
        let parsed = parse_node_addr(&ticket.node_addr).expect("the ticket address parses back");
        assert_eq!(parsed.id, ep.id(), "the address names this endpoint");
        assert_eq!(parsed, ep.addr(), "the round trip is lossless");
        ep.close().await;
    }

    /// QURATOR-314 — the home-relay wait is BOUNDED. iroh's own `Endpoint::online` docs: "If no
    /// relays are configured, this will pend forever", and `bind_local_endpoint`'s
    /// `presets::Minimal` configures none — so this endpoint can never go online, and the helper
    /// must still return `false` within its injected wait rather than hang. The wait is injected
    /// precisely so this test costs 300 ms, not the production 10 s.
    ///
    /// P-10 mutation: in `await_home_relay`, replace the whole
    /// `match tokio::time::timeout(wait, endpoint.online()).await { … }` body with a bare
    /// `endpoint.online().await; true` — the relay-less `online()` then pends forever, and the
    /// OUTER `tokio::time::timeout(5 s, …)` below turns that hang into this test's `.expect`
    /// failing (a failure, not a hang).
    #[tokio::test]
    async fn await_home_relay_is_bounded_on_a_relayless_endpoint() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;
        // The outer timeout is what makes the mutation a FAILURE rather than a hung test.
        let verdict = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            await_home_relay(&ep, std::time::Duration::from_millis(300)),
        )
        .await
        .expect("the helper must return within its injected wait — a hang is a finding");
        assert!(!verdict, "a relay-less endpoint never goes online");
        ep.close().await;
    }

    /// **QURATOR-112 #6 — a failed payload write fails the redemption outright (never
    /// `Ok(Some(..))`), so a retry can succeed.**
    ///
    /// The serve-path write now carries `SEND_DEADLINE`, and — the piece the ticket flags as worse
    /// than the DoS itself — a write that fails must fail the redemption, never read as delivered,
    /// so the failure is reported and the peer can retry. (Until QURATOR-177 Option E "unspent"
    /// was a durable fact; now it is simply the absence of a delivered outcome.)
    /// A deadline expiry and a hard write error take the *same* `?` out of `serve_manifest_stream`,
    /// and neither may ever produce `Ok(Some(..))`: a receipt only exists after a validated
    /// acknowledgement. The 120s deadline itself cannot be awaited in a unit test (no `tokio`
    /// test-util in this workspace, and `SEND_DEADLINE` is a production constant, not a test knob),
    /// so this drives that shared failure path with a writer that errors immediately instead of
    /// stalling.
    #[tokio::test]
    async fn a_failed_payload_write_fails_the_redemption_and_a_retry_succeeds() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;
        let ticket = issue_ticket(&ep, "req-1", "small", 1_700_000_000, Some("nonce-1")).unwrap();
        let payload = real_payload_for("small", 10);
        let source = TestSource::new(ticket.clone(), payload);

        let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: ticket.clone() };
        let mut frame = Vec::new();
        write_framed(&mut frame, &serde_json::to_vec(&req).unwrap()).await.unwrap();

        // The reader yields a valid request; the writer fails on the very first payload byte — the
        // "peer vanished mid-manifest" shape, which a bounded write must treat as undelivered.
        let err = serve_manifest_stream(FailingWriter, std::io::Cursor::new(frame), source.as_ref())
            .await
            .expect_err("a failed payload write must fail the redemption, never read as delivered");
        assert!(
            err.to_string().contains("write ok status"),
            "the error must name the failed write, got: {err}"
        );

        // And the ticket is untouched: the same request, to a healthy writer, still redeems.
        let mut retry = Vec::new();
        write_framed(&mut retry, &serde_json::to_vec(&req).unwrap()).await.unwrap();
        retry.push(STATUS_OK); // the acknowledgement that follows the manifest
        let served = serve_manifest_stream(
            tokio::io::sink(),
            std::io::Cursor::new(retry),
            source.as_ref(),
        )
        .await
        .expect("a healthy retry of the unspent ticket succeeds");
        assert!(served.is_some(), "the retry delivers and is acknowledged");

        ep.close().await;
    }

    /// A writer whose very first write fails — the "peer stopped reading / zero flow-control window"
    /// shape from QURATOR-112, minus the stall.
    struct FailingWriter;

    impl tokio::io::AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "peer stopped reading",
            )))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// **QURATOR-110 — a refused connection must not pin its in-flight claim for a peer-controlled
    /// 5s.**
    ///
    /// The targeted form of the drain DoS: a peer who knows a legitimate ticket's `request_id` sends
    /// a *valid-JSON* frame carrying that id and a WRONG ticket. The in-flight claim is keyed by
    /// `request_id` and taken before the ticket is checked, so the refusal path then holds the claim
    /// while it drains — and the peer controls that drain by reading the refusal and never closing.
    /// On the old 5s `drain_connection` window, one such connection pins `request_id` for 5s and the
    /// legitimate holder's retry inside that window is refused as "already being redeemed".
    ///
    /// This test drives the attack by hand: a raw client sends the forged request, reads the refusal,
    /// deliberately does NOT close, then waits past the short refusal drain but well inside the old
    /// 5s window. The honest redemption then succeeding proves the claim was released in
    /// `REFUSAL_DRAIN` time, not a peer-controlled 5s.
    #[tokio::test]
    async fn a_refused_connection_releases_its_claim_within_the_short_drain() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // Same request id, wrong ticket: survives parsing and the request-binding gate, so the claim
            // on `request_id` is taken, then refused for the ticket mismatch. The connection is held
            // open (never closed) so the owner's drain is the only thing ending it.
            let mut forged = ticket.clone();
            forged.slug = "some-other-collection".into();
            let conn = client
                .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                .await
                .expect("dial");
            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: forged };
            write_framed(&mut send, &serde_json::to_vec(&req).unwrap()).await.unwrap();
            assert_eq!(
                recv.read_u8().await.unwrap(),
                STATUS_REFUSED,
                "a wrong ticket for the right request id is refused"
            );
            let _ = read_framed(&mut recv, REFUSAL_MAX_FRAME).await.unwrap();
            // `conn`, `send`, `recv` stay in scope and open through the sleep below.

            // Wait past the short refusal drain, but well inside the old 5s window.
            tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;

            // The legitimate holder's claim is free again, and the ticket redeems.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("the claim was released by the short drain, so the honest redemption succeeds");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **QURATOR-243 — a request that cannot pass the gate never takes an in-flight claim.**
    ///
    /// The claim used to be taken from the peeked `request_id` alone, BEFORE the gate ran: any
    /// request, however invalid, held its claim slot for the full refusal window (the refusal
    /// write inside `serve_manifest_stream` carried `SEND_DEADLINE`, 120s — since QURATOR-299,
    /// `REFUSAL_SEND_DEADLINE`) while it was being
    /// refused. The attack the ticket names, driven here by hand in the
    /// `a_refused_connection_releases_its_claim_...` style: a raw client sends a request
    /// borrowing the LEGITIMATE request id but carrying a ticket bound to a DIFFERENT request —
    /// the frame parses, so the old peek found the id, but `validate_redemption`'s request-binding
    /// arm fails it. The connection reads its refusal and stays open, so on the old ordering the
    /// claim on that id is still held in the refusal drain — and the honest redemption of the
    /// same request id, sent with NO window waited out, must SUCCEED anyway. On the old ordering
    /// that honest fetch lands inside the garbage's claim and is refused as "already being
    /// redeemed" for up to 120s per probe.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): in `ManifestPlane::serve`,
    /// at the `let claimable = serde_json::from_slice::<FetchRequest>(&frame)` initializer
    /// (transport.rs, the QURATOR-243 block), delete the
    /// `.filter(|req| validate_redemption(&req.ticket, &req.request_id).is_ok())` line so the
    /// initializer ends at `.ok();` — every parseable frame is claimable again, the garbage
    /// request re-claims `req-1` for its refusal window, the honest fetch inside that window is
    /// refused as "already being redeemed", and this test's final `expect` reds.
    #[tokio::test]
    async fn a_request_that_cannot_pass_the_gate_never_takes_the_in_flight_claim() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // Borrow the LEGITIMATE request id, but bind the ticket to a different request: the
            // frame parses, so the old peek extracted the id, while `validate_redemption` fails
            // the request-binding arm and `serve_manifest_stream` refuses it.
            let mismatched = TransportTicket::issue(
                "someone-elses-request",
                "small",
                &ticket.node_addr,
                1_700_000_000,
                Some("nonce-1"),
            );
            let conn = client
                .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                .await
                .expect("dial");
            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: mismatched };
            write_framed(&mut send, &serde_json::to_vec(&req).unwrap()).await.unwrap();
            assert_eq!(
                recv.read_u8().await.unwrap(),
                STATUS_REFUSED,
                "the gate still refuses the request — skipping the claim skips nothing else"
            );
            let reason = read_framed(&mut recv, REFUSAL_MAX_FRAME).await.unwrap();
            assert_eq!(
                reason,
                REFUSAL_NO_MATCH.as_bytes(),
                "the refusal is the gate's, not the in-flight set's"
            );
            // `conn`, `send`, `recv` stay in scope and open: on the OLD ordering the claim on
            // `req-1` would still be held here, in the refusal drain.

            // No window is waited out. The honest redemption of the same request id must succeed
            // immediately — nothing it could collide with was ever claimed.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("an unclaimable request holds no slot, so the honest redemption succeeds");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **QURATOR-243 — a malformed frame claims nothing, not even the empty string.**
    ///
    /// The old peek extracted only `request_id` with `.unwrap_or_default()`, so a frame that did
    /// not parse as a `FetchRequest` at all claimed the EMPTY STRING as its key: every malformed
    /// frame collided on one slot, and a second malformed frame inside the first's refusal window
    /// was refused as "already being redeemed" — a claim the request had no business taking, held
    /// for a peer-controlled window. Reading on that shape: the collision was mostly benign in
    /// OUTCOME (a malformed frame is refused either way; only the refusal's wording was wrong),
    /// but it was the ticket's slot-parking problem in miniature — the `""` slot held per
    /// malformed frame, for nothing. With the whole-request peek a malformed frame is simply not
    /// claimable, and `serve_manifest_stream` answers each one with the same "malformed fetch
    /// request" refusal, independently.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): in `ManifestPlane::serve`,
    /// in the QURATOR-243 `let _guard = match &claimable { ... }` block (transport.rs), change
    /// the final `None => None,` arm (the one whose comment says "Not claimable: no slot is
    /// taken at all") to `None => self.claim(""),` — unparseable frames then collide on the
    /// empty-string key again, the second malformed frame below is refused as "already being
    /// redeemed", and this test's final assertion reds. (Equivalently: revert the whole
    /// QURATOR-243 hunk to the old `claim_key`/`unwrap_or_default` shape.)
    #[tokio::test]
    async fn a_malformed_frame_claims_nothing_not_even_the_empty_string() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // First malformed frame: not JSON at all. On the old ordering this claims "" for the
            // refusal window; either way it is refused as malformed.
            let garbage: &[u8] = b"not json at all";
            let conn1 = client
                .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                .await
                .expect("dial");
            let (mut send1, mut recv1) = conn1.open_bi().await.unwrap();
            write_framed(&mut send1, garbage).await.unwrap();
            assert_eq!(recv1.read_u8().await.unwrap(), STATUS_REFUSED);
            let reason1 = read_framed(&mut recv1, REFUSAL_MAX_FRAME).await.unwrap();
            assert!(
                String::from_utf8_lossy(&reason1).contains("malformed fetch request"),
                "the first malformed frame is refused as malformed, got: {}",
                String::from_utf8_lossy(&reason1)
            );

            // Second malformed frame, sent while the first's refusal is still draining (`conn1`
            // stays open): it must get the SAME malformed refusal, not "already being redeemed".
            let conn2 = client
                .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                .await
                .expect("dial");
            let (mut send2, mut recv2) = conn2.open_bi().await.unwrap();
            write_framed(&mut send2, garbage).await.unwrap();
            assert_eq!(recv2.read_u8().await.unwrap(), STATUS_REFUSED);
            let reason2 = read_framed(&mut recv2, REFUSAL_MAX_FRAME).await.unwrap();
            assert!(
                String::from_utf8_lossy(&reason2).contains("malformed fetch request"),
                "the second malformed frame must be refused AS MALFORMED, not as a duplicate \
                 redemption — it never had a claim to collide with, got: {}",
                String::from_utf8_lossy(&reason2)
            );

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **QURATOR-180 — the request-binding arm of `validate_redemption`, answered as a HOSTILE
    /// client.**
    ///
    /// `fetch_manifest` always builds `FetchRequest { request_id: ticket.request_id.clone(), .. }`,
    /// so an honest client can never make the outer `request_id` disagree with the ticket it is
    /// presenting — the binding check inside `validate_redemption` (crates/hb-core/src/ticket.rs)
    /// is unreachable from that path, which is why `a_refused_connection_releases_its_claim_...`
    /// above forges the *ticket* (its `.slug`) but leaves `request_id` matching: that test cannot
    /// reach this arm either. This test writes the wire frame by hand instead — the shape a prober
    /// who captured someone else's ticket and tried to redeem it against a request of their own
    /// choosing would send — and drives `serve_manifest_stream` directly (no QUIC needed; see
    /// `a_failed_payload_write_fails_the_redemption_and_a_retry_succeeds` above for the same
    /// Cursor/`Vec<u8>` pattern).
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): in `validate_redemption`
    /// (crates/hb-core/src/ticket.rs), delete the
    /// `if ticket.request_id != for_request_id { return Err(HbError::InvalidTicket(...)); }` arm —
    /// the mismatched request then sails through `validate_redemption`, and `TestSource::payload`
    /// (which checks the TICKET's own `request_id`, unchanged and still matching) serves it; this
    /// test's `served.is_none()` assertion reds.
    #[tokio::test]
    async fn a_ticket_redeemed_under_a_different_request_id_is_refused() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;
        let ticket = issue_ticket(&ep, "req-1", "small", 1_700_000_000, Some("nonce-1")).unwrap();
        let source = TestSource::new(ticket.clone(), real_payload_for("small", 10));

        // A hostile client: the outer request id does not match the ticket's own. `fetch_manifest`
        // can never produce this shape, so it is written onto the wire by hand.
        let req = FetchRequest { request_id: "someone-elses-request".into(), ticket: ticket.clone() };
        let mut frame = Vec::new();
        write_framed(&mut frame, &serde_json::to_vec(&req).unwrap()).await.unwrap();

        let mut response = Vec::new();
        let served =
            serve_manifest_stream(&mut response, std::io::Cursor::new(frame), source.as_ref())
                .await
                .expect("a refusal is Ok(None), not an error — the transport call itself succeeds");
        assert!(served.is_none(), "a ticket redeemed under a different request id must not be served");

        let mut reader = std::io::Cursor::new(response);
        assert_eq!(
            reader.read_u8().await.unwrap(),
            STATUS_REFUSED,
            "a ticket redeemed under a different request id is refused"
        );
        let reason = read_framed(&mut reader, REFUSAL_MAX_FRAME).await.unwrap();
        assert_eq!(
            reason,
            REFUSAL_NO_MATCH.as_bytes(),
            "the request-binding refusal uses the shared constant, not a bespoke message"
        );

        ep.close().await;
    }

    /// **QURATOR-180 — the SHAPE arm of the same gate, isolated from the binding arm above.**
    ///
    /// A structurally invalid ticket (blank `slug`, mirroring hb-core's
    /// `malformed_and_unknown_version_tickets_are_refused`) fails `TransportTicket::verify_shape`
    /// inside `validate_redemption`, before the request-id comparison is even reached.
    /// `issue_ticket` itself refuses to mint this shape (it calls `verify_shape` before returning),
    /// so the ticket is broken by hand after issuance. The outer `request_id` is left matching the
    /// ticket's own so this test cannot accidentally be satisfied by the binding arm above — it is
    /// isolated to the shape check.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): at the `validate_redemption`
    /// call site in `serve_manifest_stream`, give this arm a bespoke refusal string — replace the
    /// `refuse(&mut send, REFUSAL_NO_MATCH)` inside the `if validate_redemption(...).is_err()` arm
    /// with e.g. `refuse(&mut send, "malformed or mismatched ticket")` — and this test's
    /// `REFUSAL_NO_MATCH` equality assertion reds.
    ///
    /// ⚠ The PREVIOUS mutation for this test (delete the leading `ticket.verify_shape()?;` line in
    /// hb-core's `validate_redemption`) NO LONGER reds it since QURATOR-242: the blank slug then
    /// falls through to the slug-binding check, which now refuses with the SAME constant, so the
    /// wire bytes stay green with the shape gate deleted. The shape gate itself is pinned where it
    /// lives, in hb-core's `malformed_and_unknown_version_tickets_are_refused`; this test remains
    /// wire-text coverage for the arm, not the shape gate's mutation proof.
    #[tokio::test]
    async fn a_structurally_malformed_ticket_is_refused_the_same_way() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;
        let mut malformed =
            issue_ticket(&ep, "req-2", "small", 1_700_000_000, Some("nonce-1")).unwrap();
        malformed.slug = String::new();
        let source = TestSource::new(malformed.clone(), real_payload_for("small", 10));

        let req = FetchRequest { request_id: malformed.request_id.clone(), ticket: malformed };
        let mut frame = Vec::new();
        write_framed(&mut frame, &serde_json::to_vec(&req).unwrap()).await.unwrap();

        let mut response = Vec::new();
        let served =
            serve_manifest_stream(&mut response, std::io::Cursor::new(frame), source.as_ref())
                .await
                .expect("a refusal is Ok(None), not an error");
        assert!(served.is_none(), "a structurally invalid ticket must not be served");

        let mut reader = std::io::Cursor::new(response);
        assert_eq!(reader.read_u8().await.unwrap(), STATUS_REFUSED);
        let reason = read_framed(&mut reader, REFUSAL_MAX_FRAME).await.unwrap();
        assert_eq!(
            reason,
            REFUSAL_NO_MATCH.as_bytes(),
            "same constant as the binding-arm refusal — the two arms must be indistinguishable"
        );

        ep.close().await;
    }

    /// **QURATOR-180 — the anti-probing invariant: the two refusal arms are byte-identical on the
    /// wire.** `REFUSAL_NO_MATCH` is deliberately the SAME string for a malformed ticket and a
    /// wrong-request ticket (see the comment at `validate_redemption`'s call site in
    /// `serve_manifest_stream`) so a prober cannot distinguish "no such ticket" from "wrong ticket
    /// for this request". This test drives both hostile shapes from the two tests above and
    /// compares the raw response bytes — status byte, length prefix, and message — not just the
    /// message string, so it also pins that neither arm pads or truncates differently.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): give the shape-check and the
    /// request-binding check inside `validate_redemption` distinct refusal strings at their call
    /// site in `serve_manifest_stream` (e.g. `refuse(&mut send, "no such ticket").await?` for one
    /// arm only, leaving the other on `REFUSAL_NO_MATCH`) — this test's `assert_eq!` on the two
    /// response byte vectors reds, even though each half still redeems correctly as "refused".
    #[tokio::test]
    async fn the_two_refusal_arms_are_byte_identical_on_the_wire() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;

        // Arm 1: well-formed ticket, wrong request id.
        let ticket = issue_ticket(&ep, "req-3", "small", 1_700_000_000, Some("nonce-1")).unwrap();
        let source_a = TestSource::new(ticket.clone(), real_payload_for("small", 10));
        let req_a =
            FetchRequest { request_id: "not-the-tickets-request".into(), ticket: ticket.clone() };
        let mut frame_a = Vec::new();
        write_framed(&mut frame_a, &serde_json::to_vec(&req_a).unwrap()).await.unwrap();
        let mut response_a = Vec::new();
        serve_manifest_stream(&mut response_a, std::io::Cursor::new(frame_a), source_a.as_ref())
            .await
            .unwrap();

        // Arm 2: structurally invalid ticket, matching request id.
        let mut malformed =
            issue_ticket(&ep, "req-4", "small", 1_700_000_000, Some("nonce-1")).unwrap();
        malformed.slug = String::new();
        let source_b = TestSource::new(malformed.clone(), real_payload_for("small", 10));
        let req_b = FetchRequest { request_id: malformed.request_id.clone(), ticket: malformed };
        let mut frame_b = Vec::new();
        write_framed(&mut frame_b, &serde_json::to_vec(&req_b).unwrap()).await.unwrap();
        let mut response_b = Vec::new();
        serve_manifest_stream(&mut response_b, std::io::Cursor::new(frame_b), source_b.as_ref())
            .await
            .unwrap();

        assert_eq!(
            response_a, response_b,
            "a malformed ticket and a wrong-request ticket must be indistinguishable on the wire"
        );

        ep.close().await;
    }

    /// **QURATOR-242 — the anti-probing invariant, post-resolution half: a payload-resolution
    /// failure and a slug-binding failure are byte-identical on the wire.**
    ///
    /// Before QURATOR-242 the two post-resolution arms answered with bespoke text — the resolution
    /// arm embedded the lookup error, the slug-mismatch arm NAMED the cached collection's slug —
    /// so a peer holding a shape-valid forged ticket could distinguish "no such collection in
    /// this node's cache" from "cached, but the binding refused", and in the latter case learned
    /// the cached slug itself. An existence/metadata oracle, not a plaintext leak: the payload
    /// stays ciphertext either way. Both arms now refuse with `REFUSAL_NO_MATCH` — the same
    /// constant the pre-resolution gate above uses — and the distinct reason lives in the local
    /// `tracing` log only.
    ///
    /// Both conditions are driven GENUINELY and independently, through different arms:
    /// - arm A: the ticket is shape-valid and answers the request it is presented under (it was
    ///   issued for that very request), so `validate_redemption` passes — but it names a request
    ///   the source cannot resolve, so `source.payload` errs. The stand-in for "not cached
    ///   here": production resolves by the ticket's `(author_npub,) slug` and misses the same
    ///   way;
    /// - arm B: the source resolves the ticket's own request successfully, but is rigged
    ///   (`serve_instead`) to answer with a manifest describing a different slug, so resolution
    ///   SUCCEEDS and the byte/slug binding refuses.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): restore a distinct string on
    /// ONE arm only. In `serve_manifest_stream`, in the `Ok(declared) =>` arm of the
    /// `payload.declared_slug()` match (the slug-mismatch arm — the one whose `tracing::warn!`
    /// names the field `declared_slug`), replace its `refuse(&mut send, REFUSAL_NO_MATCH)` with
    /// the old bespoke `refuse(&mut send, &format!("refusing to serve: {declared}")).await?;`.
    /// Resolve the line by that arm's position, not by text: this comment quotes the old string,
    /// so a text anchor is non-unique. The byte comparison below reds — arm A's response and arm
    /// B's response differ again, exactly the oracle the ticket describes.
    #[tokio::test]
    async fn post_resolution_refusals_are_byte_identical_on_the_wire() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;

        // Arm A — payload resolution FAILS. The source knows request "req-242-a"; the ticket was
        // minted for "req-242-b" and is presented under its own request id, so it clears
        // `validate_redemption` and dies inside `source.payload`.
        let known =
            issue_ticket(&ep, "req-242-a", "small", 1_700_000_000, Some("nonce-a")).unwrap();
        let unheld =
            issue_ticket(&ep, "req-242-b", "small", 1_700_000_000, Some("nonce-b")).unwrap();
        let source = TestSource::new(known.clone(), real_payload_for("small", 3));
        let req_a = FetchRequest { request_id: unheld.request_id.clone(), ticket: unheld };
        let mut frame_a = Vec::new();
        write_framed(&mut frame_a, &serde_json::to_vec(&req_a).unwrap()).await.unwrap();
        let mut response_a = Vec::new();
        let served_a =
            serve_manifest_stream(&mut response_a, std::io::Cursor::new(frame_a), source.as_ref())
                .await
                .expect("a refusal is Ok(None), not an error");
        assert!(served_a.is_none(), "a ticket the source cannot resolve must not be served");

        // Arm B — payload resolution SUCCEEDS, the slug binding refuses. Same source, its OWN
        // request, rigged to answer with a manifest that describes another collection.
        *source.serve_instead.lock().unwrap() = Some(real_payload_for("big", 3));
        let req_b = FetchRequest { request_id: known.request_id.clone(), ticket: known.clone() };
        let mut frame_b = Vec::new();
        write_framed(&mut frame_b, &serde_json::to_vec(&req_b).unwrap()).await.unwrap();
        let mut response_b = Vec::new();
        let served_b =
            serve_manifest_stream(&mut response_b, std::io::Cursor::new(frame_b), source.as_ref())
                .await
                .expect("a refusal is Ok(None), not an error");
        assert!(served_b.is_none(), "a manifest for another collection must not be served");

        // The oracle closure: full response bytes — status byte, length prefix, and message —
        // must be identical across the two conditions.
        assert_eq!(
            response_a, response_b,
            "a resolution failure and a slug-binding failure must be indistinguishable on the wire"
        );
        let mut reader = std::io::Cursor::new(response_a);
        assert_eq!(
            reader.read_u8().await.unwrap(),
            STATUS_REFUSED,
            "both post-resolution refusals carry the refused status byte"
        );
        let reason = read_framed(&mut reader, REFUSAL_MAX_FRAME).await.unwrap();
        assert_eq!(
            reason,
            REFUSAL_NO_MATCH.as_bytes(),
            "both post-resolution refusals use the shared constant, not a bespoke message"
        );

        ep.close().await;
    }

    /// A writer that never accepts a byte — the "peer stopped reading / zero flow-control window"
    /// stance of QURATOR-299, as a writer whose `poll_write` simply never resolves. Unlike
    /// `FailingWriter` (a hard error, driving the same `?` path without the wait), the only thing
    /// that can end a write against this writer is the deadline wrapping it — which is exactly
    /// what the test below needs to measure.
    struct StallingWriter;

    impl tokio::io::AsyncWrite for StallingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// **QURATOR-299 — a forged ticket that passes the gate is refused within the short refusal
    /// WRITE deadline, not the manifest-sized 120s one.**
    ///
    /// Tickets are unsigned address delivery (owner ruling 2026-09-03, QURATOR-177 Option E), so
    /// any peer can mint a shape-valid `TransportTicket` answering its own request: it clears
    /// `validate_redemption` — shape plus request binding, a correctness check and deliberately
    /// not an authorization — takes the in-flight claim in `ManifestPlane::serve` (and one of the
    /// three redemption permits), and only then dies at payload resolution, the first check that
    /// consults this node's actual state. Until QURATOR-299 that post-claim refusal was written
    /// under `SEND_DEADLINE` (120s, sized for a 16 MiB manifest), so a peer that simply stopped
    /// reading parked the claim for up to 120s per probe; the write is now bounded by
    /// `REFUSAL_SEND_DEADLINE`, the write-side sibling of `REFUSAL_DRAIN` (QURATOR-110 bounded
    /// the drain; this bounds the write that precedes it).
    ///
    /// The attacking ticket is the `post_resolution_refusals_are_byte_identical_on_the_wire`
    /// arm-A fixture: minted for a request the source does not know, presented under its own
    /// request id. Two halves, same path:
    /// - an HONEST peer, simulated by a plain `Vec<u8>` writer: the refusal must arrive as
    ///   readable BYTES — refused status byte, the shared `REFUSAL_NO_MATCH` reason — not merely
    ///   quickly;
    /// - the ATTACKING peer, simulated by `StallingWriter` (never accepts a byte): the deadline
    ///   is the only thing that can end the refusal, so the elapsed time pins the constant.
    ///
    /// MUTATION (P-10, orchestrator applies and must see this red): in `refuse`
    /// (crates/hb-app/src/transport.rs), at transport.rs:345 — the line reading
    /// `        REFUSAL_SEND_DEADLINE,` (first argument of the `with_deadline(` call inside
    /// `refuse`, the deadline wrapping `send.write_u8(STATUS_REFUSED)`) — restore
    /// `        SEND_DEADLINE,`. The stalling half then hangs for the full 120s, this test's
    /// `TEST_TIMEOUT` (30s) fires on the `tokio::time::timeout` wrapper, and the `expect` after
    /// it reds. Resolve the anchor by line number, not text: `SEND_DEADLINE` also appears at the
    /// manifest write and twice on the fetch side, so a bare-text anchor is ambiguous.
    #[tokio::test]
    async fn a_gate_passing_forged_ticket_is_refused_within_the_short_refusal_write() {
        let ep = bind_local_endpoint(&rand::random(), vec![]).await;
        // The source resolves only "req-299-known"; the forged ticket is minted for
        // "req-299-forged" and names a plausible slug. Shape-valid, answering its own request —
        // so it passes the pre-claim gate — and unknown to the source, so it dies at payload
        // resolution, after the claim.
        let known =
            issue_ticket(&ep, "req-299-known", "small", 1_700_000_000, Some("nonce-a")).unwrap();
        let forged =
            issue_ticket(&ep, "req-299-forged", "films", 1_700_000_000, Some("nonce-b")).unwrap();
        let source = TestSource::new(known, real_payload_for("small", 3));
        assert!(
            validate_redemption(&forged, &forged.request_id).is_ok(),
            "fixture check: the forged ticket must pass the gate, so the refusal below is the \
             post-claim one this test exists to bound"
        );

        let req = FetchRequest { request_id: forged.request_id.clone(), ticket: forged };
        let mut frame = Vec::new();
        write_framed(&mut frame, &serde_json::to_vec(&req).unwrap()).await.unwrap();

        // The honest half: a peer that reads gets readable refusal bytes on the same path.
        let mut response = Vec::new();
        let served_honest =
            serve_manifest_stream(&mut response, std::io::Cursor::new(frame.clone()), source.as_ref())
                .await
                .expect("a refusal is Ok(None), not an error");
        assert!(served_honest.is_none(), "a ticket the source cannot resolve must not be served");
        let mut reader = std::io::Cursor::new(response);
        assert_eq!(
            reader.read_u8().await.unwrap(),
            STATUS_REFUSED,
            "an honest peer reads a refused status byte, not a connection error"
        );
        let reason = read_framed(&mut reader, REFUSAL_MAX_FRAME).await.unwrap();
        assert_eq!(
            reason,
            REFUSAL_NO_MATCH.as_bytes(),
            "the post-claim refusal is the shared constant — shortening the write deadline a \
             gate-passing ticket is held for changes the timing, never the wording"
        );

        // The attacking half: a peer that never reads. The deadline is the only way out.
        let started = std::time::Instant::now();
        let err = tokio::time::timeout(
            TEST_TIMEOUT,
            serve_manifest_stream(StallingWriter, std::io::Cursor::new(frame), source.as_ref()),
        )
        .await
        .expect(
            "test timed out — the refusal write outlasted TEST_TIMEOUT, i.e. it is still bounded \
             by a manifest-sized deadline (this is the mutation this test exists to catch)",
        )
        .expect_err("a never-reading peer must end in the refusal write's deadline, not be served");
        let elapsed = started.elapsed();
        assert!(
            err.to_string().contains("the peer stopped reading the refusal response"),
            "the error must be the refusal write deadline, got: {err}"
        );
        assert!(
            elapsed >= REFUSAL_SEND_DEADLINE,
            "the write waited its full short deadline ({elapsed:?}) — an instant failure here \
             would mean the fixture is not driving the deadline at all"
        );
        assert!(
            elapsed < SEND_DEADLINE,
            "a refusal must not be bounded by the 120s manifest write deadline; took {elapsed:?}"
        );

        ep.close().await;
    }

    /// **QURATOR-299 — the same attack against the live plane: a gate-passing forged ticket from
    /// a peer that never reads releases its claim in seconds, not the 120s write window.**
    ///
    /// Modeled on `a_refused_connection_releases_its_claim_within_the_short_drain` (QURATOR-110),
    /// with the one difference the QURATOR-299 ticket names: there the attacker READ the refusal
    /// and never closed, so only the 500ms drain was parking the claim; here the attacker never
    /// reads at all — the stance whose only bound used to be `refuse()`'s `SEND_DEADLINE` (120s),
    /// because the forged ticket passes `validate_redemption` and so TAKES the claim in
    /// `ManifestPlane::serve` before payload resolution refuses it. The forged ticket carries the
    /// honest request id and a plausible slug of its own: shape-valid, request-bound, resolvable
    /// to a manifest describing another slug — so the post-claim byte/slug binding refuses it.
    /// Anyone can mint that shape; tickets are unsigned address delivery.
    ///
    /// The attacker sends its forged request, then neither reads nor closes anything. Once the
    /// short refusal write deadline plus the drain has passed — still far inside the old 120s
    /// window — the legitimate holder's redemption of the SAME request id must succeed: the
    /// deadlines released the claim, not the peer's goodwill.
    ///
    /// MUTATION (P-10): shared with
    /// `a_gate_passing_forged_ticket_is_refused_within_the_short_refusal_write` above — that test
    /// is the deterministic red for restoring `SEND_DEADLINE` at the `refuse()` call site
    /// (transport.rs:345); this one adds the end-to-end outcome on the live plane (and reds too
    /// whenever the attacking connection's refusal write genuinely stalls on flow control,
    /// parking the claim past the window slept below).
    #[tokio::test]
    async fn a_gate_passing_forged_ticket_releases_its_claim_within_the_short_refusal_window() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, _source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // Forged: the honest request id (so the claim collides with the honest redemption), a
            // plausible slug of the attacker's choosing. Passes the gate; refused post-claim.
            let forged = TransportTicket::issue(
                &ticket.request_id,
                "a-plausible-slug",
                &ticket.node_addr,
                1_700_000_000,
                Some("nonce-299"),
            );
            let conn = client
                .connect(parse_node_addr(&ticket.node_addr).unwrap(), MANIFEST_ALPN)
                .await
                .expect("dial");
            let (mut send, recv) = conn.open_bi().await.unwrap();
            let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: forged };
            write_framed(&mut send, &serde_json::to_vec(&req).unwrap()).await.unwrap();
            // Nothing is read, nothing is closed — the connection and both stream halves stay
            // alive through the sleep and the honest fetch below, exactly the "peer that simply
            // stopped reading" stance. (Dropping `recv` would send STOP_SENDING and ERROR the
            // owner's write instead of stalling it — a different failure than the one under
            // test, so it stays held.)
            let _attacker_holds_everything_open = (conn, send, recv);

            // Past BOTH unwind paths, with margin — and still far inside the old 120s write window.
            //
            // ⚠ The budget must cover the Err path, not just the Ok one (review finding,
            // 2026-09-19). Ok path: `refuse()` completes (this refusal is ~55 bytes — status byte +
            // u32 prefix + the 47-byte REFUSAL_NO_MATCH — so it fits any initial QUIC stream credit
            // and buffers instantly even against a peer that never reads), then `Ok(None)` →
            // `drain_refusal`, releasing at ~0.5s. Err path: the write genuinely stalls, `refuse()`
            // errors at REFUSAL_SEND_DEADLINE, and `serve`'s Err arm holds `_guard` through
            // `drain_connection` — a SECOND 5s bound — releasing at ~10s. Sleeping only
            // `REFUSAL_SEND_DEADLINE + REFUSAL_DRAIN + 1s` (6.5s) would false-RED against correct
            // code the moment the rig ever lands on the Err path.
            //
            // This row therefore pins CLAIM RELEASE, not the 5s constant: in the ordinary regime it
            // releases at ~0.5s and stays green under the SEND_DEADLINE mutation. The constant's
            // discriminator is its `StallingWriter` sibling
            // (`a_gate_passing_forged_ticket_is_refused_within_the_short_refusal_write`), which is
            // where the P-10 anchor lives.
            const DRAIN_CONNECTION_BOUND: std::time::Duration = std::time::Duration::from_secs(5);
            tokio::time::sleep(
                REFUSAL_SEND_DEADLINE
                    + DRAIN_CONNECTION_BOUND
                    + REFUSAL_DRAIN
                    + std::time::Duration::from_millis(1_000),
            )
            .await;

            // The honest holder redeems the same request id: the claim was released by the
            // deadlines, not held for the peer's read window.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect(
                    "the claim was released within the short refusal window, so the honest \
                     redemption succeeds",
                );

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }
}

