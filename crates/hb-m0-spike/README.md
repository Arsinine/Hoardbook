# hb-m0-spike — M0 foundation spike

Throwaway crate that de-risks the four unknowns the **v0.9 Nostr pivot** rests on,
*before* the M1 `hb-core` rewrite commits to them (`HOARDBOOK_SPEC.md` → Path to v1.0,
M0; `HANDOVER.md` §B). It touches none of the shipped v0.4.3 code. Delete it once M1
lands the production equivalents.

**Verdict: GO on all four legs.** No blocker found. Caveats below are for M1 to carry,
not reasons to stop.

## How to run

```sh
# Legs 1–3 are offline crypto/protocol — this is the authoritative gate:
cargo test -p hb-m0-spike            # 12 tests

# Human-readable demo of legs 1–3:
cargo run -p hb-m0-spike

# Leg 4 needs a relay. Spin an ephemeral strfry (Docker), then point the runner at it:
docker run -d --name hb-m0-strfry -p 7777:7777 \
  -v "$PWD/crates/hb-m0-spike/strfry.conf:/etc/strfry.conf:ro" \
  -v hb-m0-strfrydb:/app/strfry-db dockurr/strfry
HB_M0_RELAY=ws://127.0.0.1:7777 cargo run -p hb-m0-spike
docker rm -f hb-m0-strfry && docker volume rm hb-m0-strfrydb     # teardown
```

The big builds use a WSL-native target dir to dodge `/mnt/c` 9p flakiness:
`export CARGO_TARGET_DIR=/home/zephyrus/hb-target` (per `HANDOVER.md` gotchas).

## Stack proven

`nostr-sdk 0.43` + `nostr 0.43.1` (feature `nip44`) **co-resolve and compile alongside
`iroh 0.98`** in the workspace — the single biggest scaffold risk, cleared. Cold build
~1m40s; the crate alone rebuilds in seconds.

## The four legs

### Leg 1 — the `nostr` crate (identity) · GO

`identity.rs`. Generate a secp256k1 `Keys`; encode the pubkey as a bech32 `npub`
(NIP-19) that round-trips (bech32 checksum rejects typos — retires the old
`hb1_`/double-SHA256 scheme); build + **offline-sign** a NIP-01 event
(`EventBuilder::new(kind, content).sign_with_keys(&keys)`); `event.verify()` checks the
canonical id **and** the Schnorr sig. Tampering the signed content breaks verification.

### Leg 2 — the `npub` → iroh-node binding · GO

`binding.rs`. The v0.9 replacement for the shipped `hb_id == conn.remote_id()` equality
(H2/H17). Modelled as a signed **presence event** (`KIND_PRESENCE`) carrying the iroh
node key (a real `iroh::SecretKey`'s Ed25519 `EndpointId`) as a tag. Because NIP-01 signs
a hash over `(pubkey, created_at, kind, tags, content)`, the Schnorr signature covers the
node key *and* the timestamp, so `verify_binding()` proves the `npub` vouched for exactly
that node key at exactly that time. Tested rejections: a **swapped node key** (re-uses the
original sig → recomputed id mismatch), a **stale** binding (> 30 min), and a
**future-dated** binding (beyond the ±300 s skew window). This is the honest cross-system
proof — secp256k1 identity vouching for an Ed25519 transport key.

### Leg 3 — NIP-44 listing encryption under a symmetric browse-key · GO (the crux)

`listing.rs`. The genuinely-uncertain leg: listings are "NIP-44 ciphertext under your
browse-key", but the browse-key is a **shared 32-byte symmetric secret**, whereas NIP-44's
headline API is ECDH/conversation-key based. **Resolved:** `nostr 0.43`'s
`nip44::v2::ConversationKey::new([u8; 32])` accepts a raw key directly (alongside the ECDH
`derive`), so we feed it a key **derived from the browse-key through a versioned HKDF**
(salt `hoardbook/browse-key`, version byte in the HKDF `info`). That honours the spec's
"crypto/KDF version byte in the browse-key derivation" and gives a clean forward-compat
seam: a version bump domain-separates (v1 ciphertext won't decrypt under v2). Tested: round
-trip, wrong-key rejection (MAC fails → the open web sees ciphertext only), nonce-uniqueness
across encryptions, and the KDF-version flag-day.

### Leg 4 — a stock strfry relay accepting our kinds + a fresh key · GO (with an operational note)

`relay.rs`. Publishes one presence (`11111`, replaceable `1xxxx`) and one listing
(`31111`, addressable `30xxx`, `d`=slug) from a **brand-new** key to a live strfry, fetches
both back by filter, and re-verifies end-to-end (binding verifies, listing decrypts). A
successful round-trip *is* the proof the relay stored and returned our custom kinds.
Result: `fetched 2 · binding_ok=true · listing_ok=true`.

**Operational finding (feeds the spec's open "public-relay survey" question):**
vanilla strfry accepts all kinds + any key, **but off-the-shelf relay *images* may not.**
`dockurr/strfry` ships a write-policy **whitelist plugin enabled by default**
(`/app/write-policy.py`) that rejected our fresh key with `"blocked: pubkey … not in
whitelist"`. The vendored `strfry.conf` here disables it (`writePolicy.plugin = ""`) to
test true default behaviour. Two further gotchas this leg surfaced:
- The image needs its LMDB dir to exist — mount a volume at `/app/strfry-db` or it exits
  with `mdb_env_open: No such file or directory`.
- nostr-sdk's `client.connect()` returns **before** the websocket handshake; publishing
  immediately fails with `"relay not connected"`. Use `try_connect(timeout)` (or
  `connect()` + `wait_for_connection`). M1's relay client must wait for connection.

## Decisions captured for M1

- **Crates:** `nostr-sdk` for the relay `Client`; `nostr` (feature `nip44`) for the protocol
  layer (stable `nostr::nips::nip44::v2` path). Both 0.43.x; unify with `iroh 0.98`.
- **Binding shape:** a signed presence event with the node key in a tag + `created_at` as
  the freshness stamp — no bespoke signature format needed; reuses NIP-01 verification.
- **Browse-key crypto:** versioned-HKDF → `ConversationKey::new` → NIP-44 v2.
- **Provisional kinds** `KIND_PRESENCE = 11111`, `KIND_LISTING = 31111` are **unregistered**
  placeholders in the correct NIP-01 ranges. Locking the real kinds is an open spec
  question (`HOARDBOOK_SPEC.md` → Open Questions) and an M1 deliverable.

## Out of scope (deliberately not covered by M0)

NIP-17 gift-wrapped DMs and NIP-59 private collections (M2/Phase 2); NIP-65 relay-list
discovery and multi-relay dedup (M2); base64 content framing per the NIP-44 spec (this
spike hex-encodes ciphertext into event content for simplicity); the real public-relay
survey across third-party relays.
