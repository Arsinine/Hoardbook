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
//! ACK precisely so the owner never spends a ticket on a manifest the asker then rejects. Everything about "consumed on success" rests on it — see
//! [`serve_manifest_stream`]. Every step also carries a deadline, so a peer cannot hold a handler
//! open by connecting and going quiet.
//!
//! ## Authorization — the capability is the ticket, not the address
//!
//! There is no binding token here and the retired follower gate is **not** re-imported. The asker
//! presents the [`TransportTicket`] it received, and the owner's node checks it against what it
//! actually issued for that request, then runs [`authorize_redemption`] — which additionally
//! refuses a redeemer whose contact standing has changed since approval (the revocation property
//! that "valid until redeemed" would otherwise cost).
//!
//! The ticket rode a **sealed NIP-17 DM to one recipient**, so presenting it back is evidence of
//! receipt — but note precisely what it is: a **BEARER capability**. The owner verifies the ticket
//! and the *stored recipient's* live standing; it does not authenticate the connecting peer as that
//! recipient, and it cannot, because this design deliberately has no `npub`→node-key map to check
//! against. A recipient who forwards the ticket JSON lets someone else dial and spend it. What that
//! costs is the OWNER's address plus one ciphertext delivery — binding a ticket to the asker's
//! transport identity would need the asker's node key on the wire, which is an owner-level protocol
//! decision, not a local fix. Note also what the capability is *not*: read access. The payload stays browse-key
//! encrypted, so a ticket without the share code buys ciphertext — the distinction `SEMANTICS.md`
//! reserves between a share code (grants *read*) and a transport ticket (grants *connect and
//! fetch*).
//!
//! **Consumed on success needs both halves: the type system AND the receipt.** The owner side holds
//! a `RedemptionGrant`, and the only route to a `ConsumedTicket` is `into_consumed(&ManifestPayload)`
//! — so no code path burns a ticket without the goods. But that signature proves only that *we* had
//! the payload, which is why the grant is spent only after the asker's ACK: a peer that read half the
//! payload and died would otherwise still burn the ticket, which is precisely the "human back in the
//! loop after a dropped connection" failure the owner's ruling exists to prevent. A connection that
//! dies mid-write drops the grant, which is a no-op: the ticket stays unspent and the retry works.
//! [`drain_connection`](crate::conn::drain_connection) still precedes it — without the drain a fast
//! link can close ahead of the last chunk.
//!
//! **One ticket, one delivery.** [`ManifestPlane`] holds an in-flight set keyed by `request_id`,
//! because the read of `already_consumed` and the write of the receipt are far apart and two
//! concurrent connections would otherwise both pass the gate.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use hb_core::ticket::{
    authorize_redemption, ConsumedTicket, ContactStanding, TransportTicket, TICKET_TAG,
};
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
/// literals drift the first time someone improves one of them.
const REFUSAL_NO_MATCH: &str = "no approved manifest request matches this ticket";

/// How long the owner's side waits for a peer to open a stream and finish its request frame.
///
/// Without this a peer could connect and send nothing — or dribble an 8 KiB request one byte at a
/// time — and hold a handler forever. `read_exact` has no deadline of its own, so the only bound was
/// the peer's goodwill.
const HANDSHAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the owner's side waits for the asker's acknowledgement after writing the payload.
/// Generous: the asker has to read up to 8 MiB, parse it, and verify its integrity before it can
/// honestly ACK. A timeout is **not** a success — the ticket stays unspent.
const ACK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

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

/// What the owner's node knows about a request at redeem time: the ticket it *actually issued*, the
/// redeemer's **live** standing, and whether the ticket has already been spent.
///
/// The standing is read here rather than carried in the ticket deliberately — a ticket is valid
/// until redeemed, so standing must be evaluated at redemption or a peer blocked after approval
/// could still redeem (owner ruling 2026-07-30).
#[derive(Debug, Clone)]
pub struct IssuedTicket {
    pub ticket: TransportTicket,
    pub standing: ContactStanding,
    pub already_consumed: bool,
}

/// The owner side's seam onto app state. A trait so the plane is testable over real QUIC without
/// Tauri, and so the plane itself holds no store handle.
///
/// **Note what `payload` returns and does not take:** a [`ManifestPayload`] for a slug. There is no
/// path parameter and no byte-slice parameter, so an implementation cannot answer with a collection
/// file even if it wanted to — mechanism 1 reaching one layer up from the type into the seam.
pub trait ManifestSource: Send + Sync + 'static {
    /// What we issued for `request_id`, or `None` if we issued nothing (an invented request id).
    fn issued(&self, request_id: &str) -> Option<IssuedTicket>;

    /// The manifest for an authorized request.
    fn payload(&self, slug: &str) -> Result<ManifestPayload>;

    /// Record the receipt, so a replay of the same ticket is refused by the next `issued` call.
    ///
    /// **Fallible on purpose.** A lost write leaves the ticket unspent on disk, so a replay would be
    /// served a second manifest — the one-ticket-one-delivery property is only as durable as this
    /// call. Returning `Err` lets [`ManifestPlane`] fail closed instead of merely logging.
    fn record_consumed(&self, receipt: &ConsumedTicket) -> Result<()>;
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
pub(crate) async fn read_framed(
    mut recv: impl tokio::io::AsyncRead + Unpin,
    max: usize,
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
    recv.read_exact(&mut buf).await.context("read frame body")?;
    Ok(buf)
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
    send.write_u8(STATUS_REFUSED).await.context("write refused status")?;
    let mut msg = reason.as_bytes();
    if msg.len() > REFUSAL_MAX_FRAME {
        msg = &msg[..REFUSAL_MAX_FRAME];
    }
    write_framed(&mut send, msg).await?;
    send.shutdown().await.context("shutdown after refusal")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Owner side — serve one manifest for one ticket
// ---------------------------------------------------------------------------

/// Serve one request over an already-accepted bi-stream. Returns the receipt when the asker
/// **acknowledged** a valid manifest, `Ok(None)` when the request was refused for a stated reason.
///
/// Generic over the streams so the framing and the gate are testable without QUIC; the real caller
/// is [`ManifestPlane::serve`].
pub(crate) async fn serve_manifest_stream(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    source: &dyn ManifestSource,
) -> Result<Option<(ConsumedTicket, ManifestPayload)>> {
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

    let Some(issued) = source.issued(&req.request_id) else {
        // Deliberately not "no such request" — an unknown id and a wrong ticket get the same
        // answer, so a prober learns nothing about which requests exist.
        refuse(&mut send, REFUSAL_NO_MATCH).await?;
        return Ok(None);
    };

    // The ticket presented must be the ticket issued. This — not the node address, and not the
    // request id — is the capability: it rode a sealed DM to one recipient.
    if req.ticket != issued.ticket {
        refuse(&mut send, REFUSAL_NO_MATCH).await?;
        return Ok(None);
    }

    let grant = match authorize_redemption(
        &issued.ticket,
        &req.request_id,
        issued.already_consumed,
        issued.standing,
    ) {
        Ok(g) => g,
        Err(e) => {
            refuse(&mut send, &format!("ticket refused: {e}")).await?;
            return Ok(None);
        }
    };

    let payload = match source.payload(&issued.ticket.slug) {
        Ok(p) => p,
        Err(e) => {
            // The grant is dropped here without `into_consumed`, so the ticket is untouched and the
            // asker can retry once the owner's side is healthy. A failure must cost nothing.
            refuse(&mut send, &format!("could not produce the manifest: {e}")).await?;
            return Ok(None);
        }
    };

    // **Bind the bytes to the ticket.** `ManifestSource::payload(slug)` is a naming convention, and
    // a convention is not enforcement: a source that answers with collection B for a ticket naming
    // collection A would otherwise be served, and the asker would accept it as self-consistent
    // (it *is* — it is a perfectly valid envelope for the wrong collection). A ticket names one
    // collection; the bytes must agree. Checked on both sides — see `fetch_over_connection`.
    match payload.declared_slug() {
        Ok(declared) if declared == issued.ticket.slug => {}
        Ok(declared) => {
            refuse(
                &mut send,
                &format!(
                    "refusing to serve: the manifest describes '{declared}' but this ticket is for \
                     '{}'",
                    issued.ticket.slug
                ),
            )
            .await?;
            return Ok(None);
        }
        Err(e) => {
            refuse(&mut send, &format!("could not read the manifest's own slug: {e}")).await?;
            return Ok(None);
        }
    }

    send.write_u8(STATUS_OK).await.context("write ok status")?;
    write_framed(&mut send, payload.as_bytes()).await?;
    send.shutdown().await.context("shutdown send")?;

    // ── The receipt frame: what turns "we wrote it" into "they have it" ──
    //
    // Writing the bytes proves only that WE had them. The asker acknowledges only after
    // `ManifestPayload::from_wire` and the slug check have both passed on its side, so an ACK means
    // a validated manifest is in the asker's hands. **Without this, a peer that read half the
    // payload and died would still burn the ticket** — the connection closes, the drain returns, and
    // the receipt is recorded for a transfer that never landed. That is exactly the "human back in
    // the loop after a dropped connection" failure the owner's ruling exists to prevent.
    //
    // A timeout here is NOT a success: the grant is dropped, the ticket stays unspent, the retry
    // works.
    match with_deadline(
        ACK_DEADLINE,
        "the asker never acknowledged the manifest — treating it as undelivered",
        recv.read_u8(),
    )
    .await
    {
        Ok(Ok(STATUS_OK)) => Ok(Some((grant.into_consumed(&payload), payload))),
        Ok(Ok(other)) => Err(anyhow!("asker sent {other} instead of an acknowledgement")),
        Ok(Err(e)) => Err(anyhow!("asker closed before acknowledging: {e}")),
        Err(e) => Err(e),
    }
}

/// The manifest plane's accept side: a [`ManifestSource`] plus the **in-flight set** that makes one
/// ticket one delivery.
///
/// The set is the fix for a real race, not a defensive flourish. `ManifestSource::issued` reads
/// `already_consumed` and `record_consumed` writes it much later — after the payload, after the
/// drain, after the ACK. Two connections presenting the same valid ticket could therefore both read
/// `already_consumed == false`, both serve the manifest, and both record afterwards: **one ticket,
/// two deliveries.** No test caught it because the tests serialize their accept loop and only replay
/// after the first receipt has landed.
///
/// Holding the `request_id` for the whole lifetime of a redemption closes it at the only layer that
/// can see both ends of the window. It is per-process, which is the right scope: a ticket names one
/// issuer, and the issuer is one node.
pub struct ManifestPlane {
    source: Arc<dyn ManifestSource>,
    in_flight: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Requests whose receipt could NOT be persisted. Held for the process lifetime and refused by
    /// [`ManifestPlane::claim`], so a durable-write failure cannot become a second delivery.
    ///
    /// **Honest limit, stated rather than implied:** this is per-process. A receipt write that fails
    /// and is followed by a restart leaves the ticket redeemable again — closing that needs a durable
    /// spent-marker written before the ACK, which is a protocol change, not a local fix.
    poisoned: std::sync::Mutex<std::collections::HashSet<String>>,
}

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
            poisoned: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// Claim `request_id` for this redemption, or `None` if one is already running.
    fn claim(&self, request_id: &str) -> Option<InFlightGuard<'_>> {
        // A request whose receipt we failed to persist is never served again this process.
        if self.poisoned.lock().map(|p| p.contains(request_id)).unwrap_or(true) {
            return None;
        }
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
    /// `CONNECTION_CLOSE` on a fast link. The receipt is recorded **after** the drain, and only when
    /// the asker acknowledged — a transfer the peer never finished reading has not succeeded and
    /// must not burn the ticket.
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
        let frame = with_deadline(
            HANDSHAKE_DEADLINE,
            "the asker never finished sending its request",
            read_framed(&mut recv, FETCH_REQUEST_MAX_FRAME),
        )
        .await??;

        let claim_key = serde_json::from_slice::<FetchRequest>(&frame)
            .map(|r| r.request_id)
            .unwrap_or_default();
        let _guard = match self.claim(&claim_key) {
            Some(g) => g,
            None => {
                let mut send = send;
                // Concurrent redemption of the same ticket: refuse the second, do not serve it.
                refuse(&mut send, "this ticket is already being redeemed on another connection")
                    .await?;
                drain_connection(&conn).await;
                return Ok(());
            }
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
            Ok(Some((receipt, _payload))) => {
                // Fail CLOSED on a persistence failure: the asker HAS the manifest (they ACKed), so
                // we must not let the ticket look unspent. Poisoning refuses any replay for the rest
                // of this process. Previously this was logged and moved on — a hole documented in a
                // comment instead of being closed.
                if let Err(e) = self.source.record_consumed(&receipt) {
                    if let Ok(mut p) = self.poisoned.lock() {
                        p.insert(receipt.request_id().to_string());
                    }
                    tracing::error!(
                        request_id = receipt.request_id(),
                        "could not persist a manifest receipt; refusing further redemptions of this \
                         ticket for the rest of this session: {e}"
                    );
                }
                conn.close(0u32.into(), b"delivered");
                Ok(())
            }
            Ok(None) => {
                drain_connection(&conn).await;
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
/// The owner spends the ticket on our acknowledgement, so the ACK must mean **"a manifest we can
/// actually use arrived"**. `from_wire` does not prove that: it proves the bytes are a structurally
/// self-consistent envelope, under the ceilings, declaring the ticket's slug. It cannot prove the
/// envelope is signed by *the peer we are browsing*, that the body decrypts under *our* browse-key,
/// or that the tree is complete — those need the contact store, which this layer deliberately does
/// not have.
///
/// Running that gate *after* `fetch_manifest` returned would ACK first and reject second: the asker
/// ends up with nothing usable and the ticket is **already spent**, so the human has to ask again and
/// the owner has to approve again. That is the `6691377` defect — "consumed on success" quietly
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
    ticket.verify_shape().map_err(|e| anyhow!("ticket refused before dialling: {e}"))?;
    let addr = parse_node_addr(&ticket.node_addr)?;
    let conn = endpoint
        .connect(addr, MANIFEST_ALPN)
        .await
        .with_context(|| format!("dial the manifest plane for {}", ticket.slug))?;
    let result = fetch_over_connection(&conn, ticket, accept).await;
    conn.close(0u32.into(), b"");
    result
}

async fn fetch_over_connection(
    conn: &iroh::endpoint::Connection,
    ticket: &TransportTicket,
    accept: impl FnOnce(&ManifestPayload) -> Result<()>,
) -> Result<ManifestPayload> {
    let (mut send, mut recv) = conn.open_bi().await.context("open_bi")?;
    let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: ticket.clone() };
    write_framed(&mut send, &serde_json::to_vec(&req)?).await?;
    // **Deliberately not shut down here.** The send half stays open to carry the acknowledgement —
    // that frame is what lets the owner distinguish "delivered" from "written at". Closing early
    // would put us back to the owner burning tickets on transfers that never landed.

    match recv.read_u8().await.context("read status byte")? {
        STATUS_OK => {
            // Bounded on the declared length first (the framing check), then again by `from_wire`.
            let bytes = read_framed(&mut recv, MANIFEST_MAX_TRANSPORT_BYTES).await?;
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
            send.write_u8(STATUS_OK).await.context("write acknowledgement")?;
            send.shutdown().await.context("shutdown after acknowledgement")?;
            // Wait (bounded) for the owner to close, rather than closing ourselves. `shutdown` only
            // queues the FIN; closing straight after lets `CONNECTION_CLOSE` overtake the ACK, and
            // the owner then treats a delivered manifest as undelivered. Same failure as the original
            // teardown truncation race, mirrored — so the same remedy, on this side.
            drain_connection(conn).await;
            Ok(payload)
        }
        STATUS_REFUSED => {
            let msg = read_framed(&mut recv, REFUSAL_MAX_FRAME).await?;
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
/// who has it. They get no data — `issued()` returns `None` and they get the standard refusal — but
/// they get **liveness**, on demand, for as long as the app runs.
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

fn parse_node_addr(raw: &str) -> Result<iroh::EndpointAddr> {
    serde_json::from_str(raw).context("the ticket's node address is not a dialable endpoint address")
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

    /// A `ManifestSource` over a single approved request, recording what it consumed.
    pub(crate) struct TestSource {
        pub ticket: TransportTicket,
        pub standing: Mutex<ContactStanding>,
        pub consumed: Mutex<Vec<ConsumedTicket>>,
        pub payload: ManifestPayload,
        /// Rig the source to answer with this payload regardless of the slug asked for — the
        /// "lookup fell through to the wrong collection" bug, which no honest implementation would
        /// write on purpose and which the slug binding exists to catch.
        pub serve_instead: Mutex<Option<ManifestPayload>>,
        /// Make `record_consumed` fail, standing in for a full disk or a permission error — the case
        /// that used to be logged and stepped over, leaving the ticket replayable.
        pub fail_receipt: Mutex<bool>,
        /// Rendezvous used only by the concurrency test — see [`Self::rendezvous`].
        rendezvous: Option<Rendezvous>,
    }

    /// Makes two redemptions **provably** overlap instead of hopefully overlapping.
    ///
    /// The first caller into `issued()` waits here for a second; if none comes it proceeds after a
    /// short grace period. That asymmetry is deliberate. A plain 2-party barrier would deadlock the
    /// *fixed* code, because with the in-flight guard in place the second connection is refused
    /// before it ever reaches `issued()` — so the barrier would wait forever for a party that is
    /// never coming.
    ///
    /// Why bother: without this the test was **timing-dependent**. Bypassing the guard and running
    /// the test alone failed it every time, but the same bypass under full-suite load passed — the
    /// second connection simply arrived after the first had recorded its receipt. A regression test
    /// that reds only on an idle machine is not a regression test.
    struct Rendezvous {
        seen: std::sync::Mutex<usize>,
        arrived: std::sync::Condvar,
        grace: std::time::Duration,
    }

    impl Rendezvous {
        fn wait_for_a_second_caller(&self) {
            let mut seen = self.seen.lock().unwrap();
            *seen += 1;
            if *seen >= 2 {
                self.arrived.notify_all();
                return;
            }
            let _ = self.arrived.wait_timeout(seen, self.grace);
        }
    }

    impl TestSource {
        pub(crate) fn new(ticket: TransportTicket, payload: ManifestPayload) -> Arc<Self> {
            Arc::new(Self {
                ticket,
                standing: Mutex::new(ContactStanding::Good),
                consumed: Mutex::new(Vec::new()),
                payload,
                serve_instead: Mutex::new(None),
                fail_receipt: Mutex::new(false),
                rendezvous: None,
            })
        }
    }

    impl TestSource {
        /// Turn on the rendezvous: every call into `issued` waits (briefly) for a second caller, so
        /// two simultaneous redemptions are guaranteed to be inside the gate together.
        pub(crate) fn with_rendezvous(mut self: Arc<Self>) -> Arc<Self> {
            let inner = Arc::get_mut(&mut self).expect("no other handles yet");
            inner.rendezvous = Some(Rendezvous {
                seen: std::sync::Mutex::new(0),
                arrived: std::sync::Condvar::new(),
                grace: std::time::Duration::from_millis(750),
            });
            self
        }
    }

    impl ManifestSource for TestSource {
        fn issued(&self, request_id: &str) -> Option<IssuedTicket> {
            if let Some(r) = &self.rendezvous {
                r.wait_for_a_second_caller();
            }
            if request_id != self.ticket.request_id {
                return None;
            }
            Some(IssuedTicket {
                ticket: self.ticket.clone(),
                standing: *self.standing.lock().unwrap(),
                already_consumed: !self.consumed.lock().unwrap().is_empty(),
            })
        }

        fn payload(&self, slug: &str) -> Result<ManifestPayload> {
            if let Some(rigged) = self.serve_instead.lock().unwrap().clone() {
                return Ok(rigged);
            }
            if slug != self.ticket.slug {
                return Err(anyhow!("unknown collection {slug}"));
            }
            Ok(self.payload.clone())
        }

        fn record_consumed(&self, receipt: &ConsumedTicket) -> Result<()> {
            // A test source can be told to fail its receipt write, which is how the fail-closed
            // poisoning is exercised without a real disk error.
            if *self.fail_receipt.lock().unwrap() {
                return Err(anyhow!("simulated receipt-write failure"));
            }
            self.consumed.lock().unwrap().push(receipt.clone());
            Ok(())
        }
    }

    /// Spawn the production accept loop for `source` on a fresh loopback endpoint, and return the
    /// endpoint plus a ticket addressed at it.
    async fn spawn_plane(
        entries: usize,
        slug: &str,
    ) -> (iroh::Endpoint, Arc<TestSource>, TransportTicket) {
        // Built FOR this slug: the payload and the ticket must agree, or a test would be exercising
        // the slug-binding refusal instead of the path it means to test. (Every test was — the
        // fixture said "big" while the tickets said "small", and the new binding check found it.)
        spawn_plane_inner(entries, slug, false).await
    }

    /// As `spawn_plane`, but every `issued()` call waits briefly for a second — so a concurrency test
    /// cannot silently stop testing concurrency.
    async fn spawn_plane_with_rendezvous(
        entries: usize,
        slug: &str,
    ) -> (iroh::Endpoint, Arc<TestSource>, TransportTicket) {
        spawn_plane_inner(entries, slug, true).await
    }

    async fn spawn_plane_inner(
        entries: usize,
        slug: &str,
        rendezvous: bool,
    ) -> (iroh::Endpoint, Arc<TestSource>, TransportTicket) {
        let payload = real_payload_for(slug, entries);
        let server = bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
        // The ticket carries the LOOPBACK address, not `endpoint.addr()` — a wildcard bound socket
        // is not dialable, and `ticket_node_addr` is exercised separately.
        let addr = serde_json::to_string(&loopback_addr(&server)).unwrap();
        let ticket = TransportTicket::issue("req-1", slug, &addr, 1_700_000_000, Some("nonce-1"));
        let source = TestSource::new(ticket.clone(), payload);
        let source = if rendezvous { source.with_rendezvous() } else { source };

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

    /// Wait (bounded) for the owner's side to record its receipt.
    ///
    /// **Not a convenience — an ordering property made explicit.** The receipt is recorded *after*
    /// `drain_connection`, i.e. after the asker has closed, so the instant the asker holds the
    /// manifest the ticket is not yet marked spent. That ordering is deliberate: a transfer the peer
    /// never finished reading has not succeeded and must not burn the ticket. Asserting on
    /// `consumed` without this wait is a race — it read 0 on the first run of the multi-MB case.
    async fn await_receipt(source: &TestSource) -> ConsumedTicket {
        for _ in 0..100 {
            if let Some(r) = source.consumed.lock().unwrap().first().cloned() {
                return r;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the owner's side never recorded the receipt for a delivered manifest");
    }

    /// **The acceptance test: a multi-MB manifest crosses a real connection.** 10,000 entries of
    /// realistic long filenames through the real splitter — ~60 parts and ~2.4 MB of sealed
    /// envelope, which is many QUIC frames rather than the single-datagram case a small fixture
    /// would test.
    ///
    /// 10,000 is not arbitrary: it is the owner's stated human browse limit, the number MECH-2's
    /// ceiling was derived from. **Measured here, that shape is ~245 bytes of sealed envelope per
    /// entry, not the ~70 MECH-2 assumed** — realistic nested paths and long names cost more than
    /// bare filenames. So 8 MiB carries ~33k such entries rather than ~75k. The ceiling still sits
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

            let receipt = await_receipt(&source).await;
            assert_eq!(receipt.delivered_bytes(), payload.len(), "the receipt records what arrived");
            assert_eq!(
                source.consumed.lock().unwrap().len(),
                1,
                "a successful transfer consumes the ticket exactly once"
            );

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **A replay of a spent ticket is refused** — over the real plane, not just in hb-core's unit
    /// test. The second fetch is a fresh connection presenting the same ticket.
    #[tokio::test]
    async fn a_spent_ticket_is_refused_on_a_second_connection() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            fetch_manifest(&client, &ticket, |_| Ok(())).await.expect("the first redemption succeeds");
            // Wait for the receipt before replaying: without it this test races the owner's
            // post-drain bookkeeping and would pass or fail on timing rather than on the rule.
            await_receipt(&source).await;
            let err = fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect_err("a replay of a consumed ticket must be refused");
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("already") || msg.contains("redeem"),
                "the refusal must name the replay, got: {msg}"
            );

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **The revocation test, end to end.** An asker blocked after approval is refused at redeem
    /// time — and the ticket is not burned, so restoring standing restores the approval rather than
    /// forcing a fresh request.
    #[tokio::test]
    async fn a_redeemer_blocked_after_approval_is_refused_then_restored() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            *source.standing.lock().unwrap() = ContactStanding::Blocked;
            let err = fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect_err("a blocked redeemer must be refused");
            assert!(
                err.to_string().contains("no longer an approved contact"),
                "the refusal must name the standing check, got: {err}"
            );
            assert!(
                source.consumed.lock().unwrap().is_empty(),
                "a refused redemption must not consume the ticket"
            );

            *source.standing.lock().unwrap() = ContactStanding::Good;
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("restoring the contact restores the approval — the ticket was not burned");

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// A ticket for a request this node never approved, and a forged ticket for one it did, get the
    /// **same** refusal — a prober learns nothing about which requests exist.
    #[tokio::test]
    async fn an_unknown_request_and_a_forged_ticket_are_indistinguishable() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            let mut invented = ticket.clone();
            invented.request_id = "req-never-approved".into();
            let unknown = fetch_manifest(&client, &invented, |_| Ok(())).await.expect_err("unknown request");

            // Same request id, but a ticket body this node did not issue (a different slug bound in).
            let mut forged = ticket.clone();
            forged.slug = "some-other-collection".into();
            let mismatch = fetch_manifest(&client, &forged, |_| Ok(())).await.expect_err("forged ticket");

            assert_eq!(
                unknown.to_string(),
                mismatch.to_string(),
                "an unknown request and a forged ticket must be indistinguishable"
            );
            assert!(source.consumed.lock().unwrap().is_empty());

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

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

    /// **Codex finding 2 — one ticket, one delivery, under real concurrency.**
    ///
    /// Two connections present the same valid ticket at the same moment. Before the in-flight set
    /// both could read `already_consumed == false`, both serve the manifest, and both record
    /// afterwards. The old tests could not catch this: they awaited the accept loop serially and only
    /// replayed after the first receipt landed, so the window never existed.
    // multi_thread: the rendezvous blocks a worker thread on a condvar, which would stall a
    // single-threaded runtime rather than let the second connection through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_simultaneous_redemptions_deliver_the_manifest_once() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane_with_rendezvous(400, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            // Fired together, on separate connections, with no ordering between them. The
            // rendezvous inside `issued()` is what makes the overlap a fact rather than a hope.
            let (a, b) = tokio::join!(
                fetch_manifest(&client, &ticket, |_| Ok(())),
                fetch_manifest(&client, &ticket, |_| Ok(()))
            );

            let winners = [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count();
            assert_eq!(
                winners, 1,
                "exactly one of two simultaneous redemptions may succeed — got {winners}. \
                 a={a:?} b={b:?}"
            );
            await_receipt(&source).await;
            assert_eq!(
                source.consumed.lock().unwrap().len(),
                1,
                "and the ticket is consumed exactly once"
            );

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
    /// nothing downstream would notice. Both sides check; this exercises the owner's side, and the
    /// refusal names both slugs so the operator can see what happened.
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
            // asker's would catch it and the test could not tell the difference. "refusing to serve"
            // is only ever the owner's.
            assert!(
                msg.contains("refusing to serve"),
                "the OWNER must refuse before sending, got: {msg}"
            );
            assert!(
                msg.contains("big") && msg.contains("small"),
                "the refusal must name both the manifest's slug and the ticket's, got: {msg}"
            );
            assert!(
                source.consumed.lock().unwrap().is_empty(),
                "a refused delivery must not consume the ticket"
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

    /// **Codex finding 3 — a peer that never acknowledges does not burn the ticket.**
    ///
    /// The asker here speaks the protocol by hand: it sends a valid request, reads the whole payload,
    /// and then walks away without acknowledging — the shape of a client that crashed mid-parse, or
    /// read half the bytes and died. Before the receipt frame the owner recorded the ticket as
    /// consumed anyway (write + drain + timeout = "success"), so the asker's retry was refused as a
    /// replay for a transfer that never landed.
    #[tokio::test]
    async fn a_peer_that_never_acknowledges_leaves_the_ticket_unspent() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(50, "small").await;
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

            // Give the owner's side room to (wrongly) record a receipt.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            assert!(
                source.consumed.lock().unwrap().is_empty(),
                "an unacknowledged transfer must not consume the ticket — a failed delivery costs \
                 nothing, which is the whole point of consumed-on-success"
            );

            // And the honest retry works, which is what the owner's ruling actually asked for.
            fetch_manifest(&client, &ticket, |_| Ok(())).await.expect("the retry must succeed");
            await_receipt(&source).await;

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **The acceptance gate runs BEFORE the acknowledgement — a manifest the caller rejects must not
    /// burn the ticket.**
    ///
    /// The sibling of `a_peer_that_never_acknowledges_leaves_the_ticket_unspent`, one layer up. That
    /// one covers a peer that never answers; this one covers a peer that answers *correctly at the
    /// wire level* and is still rejected by the app: `from_wire` proves the envelope is
    /// self-consistent, under the ceilings, and declares the ticket's slug — it cannot prove the
    /// signature is the browsed peer's, that the body decrypts under our browse-key, or that the tree
    /// is complete. Those checks live in `commands::browse::open_manifest`, behind the contact store.
    ///
    /// Run the gate after `fetch_manifest` returns and the ACK is already gone: the asker has nothing
    /// usable, the ticket is spent, and the human has to ask again while the owner approves again.
    /// That is `6691377`'s "consumed on written" defect recurring with the boundary moved from *we
    /// wrote it* to *they parsed it*, when what the ruling means is *they can use it*.
    ///
    /// **⚠ RED-verifying this needs the FAITHFUL pre-fix ordering, and the obvious probe is not it.**
    /// Moving the gate to just after the ACK but *before* `drain_connection` leaves this test GREEN:
    /// the early return skips the drain, so `CONNECTION_CLOSE` outruns the ACK and the owner never
    /// sees it — the teardown race accidentally does the fix's job. Only placing the gate after the
    /// drain (where a caller running it *after* `fetch_manifest` returns effectively puts it) reddens
    /// this. The lesson generalises: **when a probe passes, check whether some unrelated mechanism is
    /// masking the defect rather than concluding the guard holds.**
    #[tokio::test]
    async fn a_manifest_the_caller_rejects_leaves_the_ticket_unspent() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(50, "small").await;
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

            // Room for the owner to (wrongly) record a receipt if the ACK leaked out.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            assert!(
                source.consumed.lock().unwrap().is_empty(),
                "a manifest the asker could not accept must not consume the ticket — the ACK is the \
                 assertion that a USABLE manifest arrived, not merely a parseable one"
            );

            // And the approval survives, so a fixed client (or a re-added contact) can still redeem.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("the ticket is still good after a rejection");
            await_receipt(&source).await;

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

    /// **A receipt that could not be persisted must not leave the ticket replayable.**
    ///
    /// The asker has ACKed, so it *has* the manifest. If the durable write then fails, the ticket
    /// still reads unspent on disk and a second connection would be served a second delivery —
    /// breaking one-ticket-one-delivery at exactly the moment the evidence says it was delivered.
    /// This was previously logged and stepped over: the hole was described in a comment instead of
    /// being closed, which is the same shape as the guard that asserted a property it never
    /// implemented.
    ///
    /// The fix is per-process poisoning, and its limit is stated rather than implied: a restart
    /// after a failed write does make the ticket redeemable again. Closing *that* needs a durable
    /// spent-marker written before the ACK, which is a protocol change.
    #[tokio::test]
    async fn a_failed_receipt_write_poisons_the_ticket_instead_of_allowing_a_replay() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (server, source, ticket) = spawn_plane(50, "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            *source.fail_receipt.lock().unwrap() = true;

            // The delivery itself succeeds — the asker gets the manifest and acknowledges it.
            fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect("the manifest is delivered; only the RECEIPT write fails");
            assert!(
                source.consumed.lock().unwrap().is_empty(),
                "the receipt write failed, so nothing was recorded — this is the setup, not the claim"
            );

            // The claim: a replay is refused anyway, because the plane poisoned the request.
            let replay = fetch_manifest(&client, &ticket, |_| Ok(()))
                .await
                .expect_err("a ticket whose receipt could not be persisted must not be served again");
            assert!(
                replay.to_string().contains(REFUSAL_NO_MATCH)
                    || replay.to_string().contains("already being redeemed")
                    || replay.to_string().contains("ticket refused"),
                "the replay is refused, got: {replay}"
            );

            client.close().await;
            server.close().await;
        })
        .await
        .expect("test timed out");
    }

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
}

