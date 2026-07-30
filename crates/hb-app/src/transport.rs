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
//! ```
//!
//! **Both length prefixes are bounded before a byte is allocated** ([`read_framed`]). That is the
//! framing-layer check W3 deliberately left to W1: `ManifestPayload::from_wire` is the *second*
//! ceiling check, and by the time bytes reach it they are already in memory. A hostile peer
//! declaring 500 MB is refused on the declared length, not after the allocation.
//!
//! ## Authorization — the capability is the ticket, not the address
//!
//! There is no binding token here and the retired follower gate is **not** re-imported. The asker
//! presents the [`TransportTicket`] it received, and the owner's node checks it against what it
//! actually issued for that request, then runs [`authorize_redemption`] — which additionally
//! refuses a redeemer whose contact standing has changed since approval (the revocation property
//! that "valid until redeemed" would otherwise cost).
//!
//! The ticket rode a **sealed NIP-17 DM to exactly one recipient**, so presenting it back is what
//! proves receipt. Note what the capability is *not*: read access. The payload stays browse-key
//! encrypted, so a ticket without the share code buys ciphertext — the distinction `SEMANTICS.md`
//! reserves between a share code (grants *read*) and a transport ticket (grants *connect and
//! fetch*).
//!
//! **Consumed on success is enforced by the type system, not by this module's care.** The owner
//! side holds a `RedemptionGrant`, and the only route to a `ConsumedTicket` is
//! `into_consumed(&ManifestPayload)`. A connection that dies mid-write drops the grant, which is a
//! no-op: the ticket stays unspent and the retry works. That is why the consume call sits *after*
//! [`drain_connection`](crate::conn::drain_connection) — the drain is what makes "the peer read it"
//! observable, and without it a fast link can close ahead of the last chunk.

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
    fn record_consumed(&self, receipt: &ConsumedTicket);
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

/// Serve one request over an already-accepted bi-stream. Returns the receipt when a manifest was
/// delivered, `Ok(None)` when the request was refused for a stated reason.
///
/// Generic over the streams so the framing and the gate are testable without QUIC; the real caller
/// is [`serve_manifest_connection`].
pub(crate) async fn serve_manifest_stream(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    recv: impl tokio::io::AsyncRead + Unpin,
    source: &dyn ManifestSource,
) -> Result<Option<(ConsumedTicket, ManifestPayload)>> {
    let frame = read_framed(recv, FETCH_REQUEST_MAX_FRAME).await?;
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
        refuse(&mut send, "no approved manifest request matches this ticket").await?;
        return Ok(None);
    };

    // The ticket presented must be the ticket issued. This — not the node address, and not the
    // request id — is the capability: it rode a sealed DM to one recipient.
    if req.ticket != issued.ticket {
        refuse(&mut send, "no approved manifest request matches this ticket").await?;
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

    send.write_u8(STATUS_OK).await.context("write ok status")?;
    write_framed(&mut send, payload.as_bytes()).await?;
    send.shutdown().await.context("shutdown send")?;

    // Consumed on SUCCESS: only reachable with the payload in hand, and only after it was written.
    Ok(Some((grant.into_consumed(&payload), payload)))
}

/// Accept one connection on the manifest plane and serve it.
///
/// The [`drain_connection`] call is load-bearing and its position is deliberate: it holds the
/// connection open until the asker closes it, so the last chunk is not raced by a
/// `CONNECTION_CLOSE` on a fast link. The receipt is recorded **after** the drain — a transfer the
/// peer never finished reading has not succeeded, and must not burn the ticket.
pub async fn serve_manifest_connection(
    conn: iroh::endpoint::Connection,
    source: Arc<dyn ManifestSource>,
) -> Result<()> {
    let (send, recv) = conn.accept_bi().await.context("accept_bi")?;
    let served = serve_manifest_stream(send, recv, source.as_ref()).await;
    drain_connection(&conn).await;
    match served {
        Ok(Some((receipt, _payload))) => {
            source.record_consumed(&receipt);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
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
pub async fn fetch_manifest(
    endpoint: &iroh::Endpoint,
    ticket: &TransportTicket,
) -> Result<ManifestPayload> {
    ticket.verify_shape().map_err(|e| anyhow!("ticket refused before dialling: {e}"))?;
    let addr = parse_node_addr(&ticket.node_addr)?;
    let conn = endpoint
        .connect(addr, MANIFEST_ALPN)
        .await
        .with_context(|| format!("dial the manifest plane for {}", ticket.slug))?;
    let result = fetch_over_connection(&conn, ticket).await;
    conn.close(0u32.into(), b"");
    result
}

async fn fetch_over_connection(
    conn: &iroh::endpoint::Connection,
    ticket: &TransportTicket,
) -> Result<ManifestPayload> {
    let (mut send, mut recv) = conn.open_bi().await.context("open_bi")?;
    let req = FetchRequest { request_id: ticket.request_id.clone(), ticket: ticket.clone() };
    write_framed(&mut send, &serde_json::to_vec(&req)?).await?;
    send.shutdown().await.context("shutdown request stream")?;

    match recv.read_u8().await.context("read status byte")? {
        STATUS_OK => {
            // Bounded on the declared length first (the framing check), then again by `from_wire`.
            let bytes = read_framed(&mut recv, MANIFEST_MAX_TRANSPORT_BYTES).await?;
            ManifestPayload::from_wire(bytes)
                .map_err(|e| anyhow!("the peer's manifest was refused: {e}"))
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
) -> Result<TransportTicket> {
    let ticket = TransportTicket::issue(request_id, slug, &ticket_node_addr(endpoint)?, issued_at);
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
    /// `split_listing` → `build_manifest_envelope`. **Not a hand-written fixture** — that is the
    /// W7.2 blind spot Suite MAN was added to close, and a transport test fed hand-built parts
    /// would prove nothing about what production actually puts on the wire.
    pub(crate) fn real_payload(entries: usize) -> ManifestPayload {
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
            r#"{{"schema_v":1,"slug":"big","name":"Big","entries":[{}]}}"#,
            items.join(",")
        );
        let parts = split_listing("big", &listing, 40_000).expect("split the listing");
        let plaintexts: Vec<String> = parts.into_iter().map(|p| p.json).collect();
        let env = build_manifest_envelope(
            &id,
            "big",
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
    }

    impl TestSource {
        pub(crate) fn new(ticket: TransportTicket, payload: ManifestPayload) -> Arc<Self> {
            Arc::new(Self {
                ticket,
                standing: Mutex::new(ContactStanding::Good),
                consumed: Mutex::new(Vec::new()),
                payload,
            })
        }
    }

    impl ManifestSource for TestSource {
        fn issued(&self, request_id: &str) -> Option<IssuedTicket> {
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
            if slug != self.ticket.slug {
                return Err(anyhow!("unknown collection {slug}"));
            }
            Ok(self.payload.clone())
        }

        fn record_consumed(&self, receipt: &ConsumedTicket) {
            self.consumed.lock().unwrap().push(receipt.clone());
        }
    }

    /// Spawn the production accept loop for `source` on a fresh loopback endpoint, and return the
    /// endpoint plus a ticket addressed at it.
    async fn spawn_plane(
        payload: ManifestPayload,
        slug: &str,
    ) -> (iroh::Endpoint, Arc<TestSource>, TransportTicket) {
        let server = bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
        // The ticket carries the LOOPBACK address, not `endpoint.addr()` — a wildcard bound socket
        // is not dialable, and `ticket_node_addr` is exercised separately.
        let addr = serde_json::to_string(&loopback_addr(&server)).unwrap();
        let ticket = TransportTicket::issue("req-1", slug, &addr, 1_700_000_000);
        let source = TestSource::new(ticket.clone(), payload);

        let accept_ep = server.clone();
        let accept_source: Arc<dyn ManifestSource> = source.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accept_ep.accept().await {
                let Ok(accepting) = incoming.accept() else { continue };
                let Ok(conn) = accepting.await else { continue };
                let _ = serve_manifest_connection(conn, accept_source.clone()).await;
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
    #[test]
    fn the_planes_framing_is_pinned() {
        assert_eq!(MANIFEST_ALPN, b"/hoardbook/manifest/1", "the plane's ALPN is a wire contract");
        assert_eq!(STATUS_OK, 0, "the ok status byte is a wire contract");
        assert_eq!(STATUS_REFUSED, 1, "the refused status byte is a wire contract");
        assert_eq!(
            std::mem::size_of::<u32>(),
            4,
            "the length prefix is u32-LE — a width change is a protocol change"
        );
        // Distinct from the retired file plane, so a peer still speaking it finds nothing listening.
        assert_ne!(MANIFEST_ALPN, b"/hoardbook/xfer/1");
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
            let payload = real_payload(10_000);
            assert!(
                payload.len() > 2 * 1024 * 1024,
                "the fixture must actually be multi-MB, got {} bytes",
                payload.len()
            );
            let expected = payload.envelope().unwrap();
            let (server, source, ticket) = spawn_plane(payload.clone(), "big").await;

            let client = bind_local_endpoint(&rand::random(), vec![]).await;
            let got = fetch_manifest(&client, &ticket).await.expect("fetch the manifest");

            assert_eq!(got.len(), payload.len(), "the whole payload arrived");
            assert_eq!(
                got.envelope().unwrap(),
                expected,
                "the envelope survived the plane byte-for-byte"
            );

            let receipt = await_receipt(&source).await;
            assert_eq!(receipt.delivered_bytes, payload.len(), "the receipt records what arrived");
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
            let (server, source, ticket) = spawn_plane(real_payload(50), "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            fetch_manifest(&client, &ticket).await.expect("the first redemption succeeds");
            // Wait for the receipt before replaying: without it this test races the owner's
            // post-drain bookkeeping and would pass or fail on timing rather than on the rule.
            await_receipt(&source).await;
            let err = fetch_manifest(&client, &ticket)
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
            let (server, source, ticket) = spawn_plane(real_payload(50), "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            *source.standing.lock().unwrap() = ContactStanding::Blocked;
            let err = fetch_manifest(&client, &ticket)
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
            fetch_manifest(&client, &ticket)
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
            let (server, source, ticket) = spawn_plane(real_payload(50), "small").await;
            let client = bind_local_endpoint(&rand::random(), vec![]).await;

            let mut invented = ticket.clone();
            invented.request_id = "req-never-approved".into();
            let unknown = fetch_manifest(&client, &invented).await.expect_err("unknown request");

            // Same request id, but a ticket body this node did not issue (a different slug bound in).
            let mut forged = ticket.clone();
            forged.slug = "some-other-collection".into();
            let mismatch = fetch_manifest(&client, &forged).await.expect_err("forged ticket");

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

    /// **The framing-layer ceiling check** — the piece W3 explicitly left to W1.
    ///
    /// The declared length is refused on the four bytes that declared it. Proven by sending *only*
    /// the prefix and no body at all: if the check ran after the read, this would block waiting for
    /// 8 MiB that never arrives rather than returning an error. And the reason must point at export,
    /// because a manifest over the ceiling is a "too big, export it", not a corruption.
    #[tokio::test]
    async fn the_framing_refuses_an_over_cap_declared_length_before_reading_the_body() {
        let mut wire = Vec::new();
        let over = (MANIFEST_MAX_TRANSPORT_BYTES + 1) as u32;
        wire.extend_from_slice(&over.to_le_bytes()); // ...and deliberately no body.

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_framed(std::io::Cursor::new(wire), MANIFEST_MAX_TRANSPORT_BYTES),
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

    /// `ticket_node_addr` and `parse_node_addr` are one format, not two: the ticket's opaque string
    /// round-trips back to the same dialable address. This is the seam where hb-core's
    /// deliberate ignorance of iroh meets the transport.
    #[tokio::test]
    async fn the_tickets_opaque_node_addr_round_trips_to_a_dialable_address() {
        let ep = bind_local_endpoint(&rand::random(), vec![MANIFEST_ALPN.to_vec()]).await;
        let ticket = issue_ticket(&ep, "req-1", "slug", 1_700_000_000).unwrap();
        let parsed = parse_node_addr(&ticket.node_addr).expect("the ticket address parses back");
        assert_eq!(parsed.id, ep.id(), "the address names this endpoint");
        assert_eq!(parsed, ep.addr(), "the round trip is lossless");
        ep.close().await;
    }
}

