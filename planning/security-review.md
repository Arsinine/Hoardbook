# Hoardbook — Security Review (whole-codebase attack-surface audit)

Date: 2026-05-29 · Reviewer: Claude (first pass, pre multi-LLM fan-out)
Scope: `hb-app`, `hb-core`, `hb-relay`, `hb-dpapi` source. Excludes vendored `wmi` and generated SvelteKit/schema files.

Hoardbook is a Tauri (Rust + SvelteKit) P2P "social phonebook". Identity = Ed25519 keypair (`hb1_…`). Documents are JCS-canonicalized + Ed25519-signed envelopes. A central relay (HTTP) brokers presence (heartbeats), DM mailboxes, and peer discovery. File sharing is direct P2P over iroh/QUIC.

## Threat model assumed
- Network attacker on the path between client and relay (the relay is reached over plaintext HTTP).
- Malicious or compromised relay.
- Malicious peer (downloader or uploader) speaking the iroh protocols.
- Hostile content authors (profiles/collections/messages rendered in the webview).
- Local attacker with read access to the user's home dir (lower priority).

---

## Findings (severity-ranked)

### H1 — All relay traffic is plaintext HTTP; bootstrap relay is hardcoded and unremovable
`crates/hb-app/src/relay.rs:10` — `BOOTSTRAP_RELAYS = ["http://141.98.199.138:3000"]`. `crates/hb-relay/src/main.rs:26` binds plain HTTP. `set_relay_urls` (relay.rs:61) always re-prepends the bootstrap relay, so a user cannot opt out of trusting this one server over cleartext.

Impact:
- Passive: message **metadata** (sender hb_id, recipient hb_id, timestamps, sizes) and any `encrypted:false` message content are exposed to anyone on the wire. Heartbeats leak presence + iroh `node_addr`.
- Active MITM: impersonate the relay, censor or replay messages, and (critical) **return an attacker-controlled `node_addr`** for `GET /v1/peer/:pubkey` (feeds H2).

Recommendation: require HTTPS for relay URLs (reject `http://` except explicit localhost/dev), ship the bootstrap relay as `https://`, and pin/verify the cert or relay identity.

### H2 — P2P download source is not authenticated against the expected peer identity (MITM file substitution)
`crates/hb-app/src/commands/sharing.rs:45` — `request_download` takes `_peer_hb_id` but **ignores it** (underscore-prefixed). It connects to `peer_node_addr` (an iroh `EndpointAddr` JSON that originated from the relay) in `crates/hb-app/src/transfer.rs:343`. iroh authenticates the *node id contained in that addr*, but nothing checks that node id equals the node id derived from `peer_hb_id`. Because the iroh secret key **is** the Ed25519 identity key (`crates/hb-app/src/lib.rs:40` `iroh::SecretKey::from_bytes(private_bytes)`), that check is possible and cheap — it just isn't done.

Compounding:
- `expected_sha256` is `Option` and in practice `None` — `DirectoryItem` no longer carries a hash (`collection.rs` test `no_sha256_in_draft`), so transferred bytes are unverified.
- Transfer payload is raw bytes with no signature (unlike GetProfile, which returns signed envelopes).

Impact: a malicious or MITM relay (trivial given H1) returns the attacker's `node_addr`; the victim connects to the attacker and downloads attacker-chosen file content under a trusted peer's name. Downloaded files may be opened/executed by the user.

Recommendation: derive the expected iroh node id from `peer_hb_id` and refuse to connect to any `EndpointAddr` whose node id differs. Optionally carry signed per-file hashes in the collection listing and always verify.

### H3 — Relay DM mailbox is world-readable and never deleted
`crates/hb-relay/src/handlers.rs:189` `GET /v1/messages/:pubkey` requires no authentication and is **not rate-limited** (`main.rs:67` — only publish/heartbeat go through the limiter). hb_ids are public identifiers. `get_messages_for` (db.rs:166) does not delete on read; messages live for `EXPIRY_DAYS = 30`.

Impact: anyone can poll any user's mailbox and harvest all stored (E2E-encrypted) ciphertext plus full metadata (from, to, sent_at), repeatedly. Content confidentiality holds for `encrypted:true` messages (which is what the client sends), but this is a standing metadata/traffic-analysis exposure and a ciphertext-harvesting oracle. Over HTTP (H1) even a passive observer gets it.

Recommendation: require proof-of-possession of the recipient key (e.g., signed/nonce-challenged fetch) to read a mailbox; delete or mark-delivered on fetch; rate-limit GETs.

### H17 — `require_follow` is bypassable: transfer server trusts an unauthenticated `requester_hb_id` [added by reviewer: cdx-1]
`crates/hb-app/src/transfer.rs:128` — when a collection is shared followers-only, the server compares `req.requester_hb_id` against its contacts list (`:134`). But `requester_hb_id` is an unauthenticated field in the `XferRequest` JSON sent by the *downloader* — the struct comment (transfer.rs:94) literally calls it "Honor-system requester identity." Any peer can set it to the hb_id of anyone the sharer follows and download restricted files. The iroh connection's cryptographically authenticated remote node identity (which equals the requester's Ed25519/`hb_id`) is never consulted.

Impact: `require_follow` is not an access control. This is the serving-side counterpart to H2 — both stem from the transfer protocol never binding to the authenticated iroh peer identity.

Recommendation: derive the requester identity from the authenticated iroh remote node id (don't trust `requester_hb_id`), or require the `XferRequest` to be signed by the requester key and verify before applying `require_follow`. Add tests for a spoofed `requester_hb_id` and a relay-substituted `node_addr`. Treat H2 + H17 as one "transfer-protocol identity binding" workstream.

### M4 — CSP disabled while private-key-export commands are exposed to the webview
`crates/hb-app/tauri.conf.json:34` — `"security": { "csp": null }`. `export_keypair` and `save_keypair_file` (`commands/identity.rs:172`/`:183`, both registered in `lib.rs:224-225`) return/write the **plaintext private key** and are invokable by any JS running in the webview.

Current state: the only `{@html}` sinks render trusted local `icons.*` SVG constants; peer-controlled strings (display names, bios, file names, chat text) go through Svelte's default escaping, so there is no live XSS today. The risk is defense-in-depth: one future `{@html}` on remote data, a templating mistake, or a compromised frontend dependency becomes full identity-key theft because there is no CSP and no human-confirmation gate on key export.

Recommendation: set a strict CSP; require an explicit OS-level confirmation (or move export behind a re-auth) for `export_keypair`/`save_keypair_file`; restrict Tauri capabilities to the minimum.

### M5 — Relay rate-limiter map grows unbounded (memory-exhaustion DoS)
`crates/hb-relay/src/state.rs:10` — `state: HashMap<String,(Instant,u32)>` keyed by client IP, entries are only ever reset for an IP that comes back; **nothing evicts stale IPs**. An attacker rotating source addresses (trivial across an IPv6 /64) inflates the map without bound.

Recommendation: periodically sweep entries older than `window`, or use a fixed-capacity / LRU structure.

### M6 — Per-recipient mailbox can be flooded by a single sender
`handlers.rs:78` enforces `count_messages_for(to) >= MAX_MESSAGES_PER_RECIPIENT (500)` across **all** senders. One attacker can send 500 validly-signed messages (each a unique `sent_at`) and block every legitimate sender to that recipient until expiry/cap relief.

Recommendation: cap per `(from, to)` pair, not just per recipient; evict oldest on overflow; or require recipient-side acknowledgement.

### M7 — Remote-supplied `req.slug` reaches a filesystem path without slug validation
`crates/hb-app/src/transfer.rs:115` parses `XferRequest` from a remote peer and passes `req.slug` to `store.load_share_settings(&req.slug)` → `share_settings_path` = `base/sharing/{slug}.json` (`store.rs:276`) with **no `is_valid_slug` check** (only `req.path` is traversal-guarded at transfer.rs:163). Local collection commands validate slugs, but this remote path does not.

Impact: path traversal / file-existence probing of the data dir via `..` segments in `slug`. Direct content exfiltration is limited (the file must parse as `ShareSettings`), but it is unvalidated remote input on a filesystem path and an existence/error oracle.

Recommendation: `is_valid_slug(&req.slug)` at the top of `handle_xfer_connection`.

### M8 — File-transfer path check does not resolve symlinks
`transfer.rs:163` rejects absolute paths and `..` components, then joins onto `root`, but never canonicalizes the result and verifies it is still within `root`. A symlink inside a shared directory that points outside `root` lets a downloader read arbitrary files the sharer can read.

Recommendation: `canonicalize()` the joined path and confirm it `starts_with` the canonicalized `root` before opening.

### L9 — Private key stored in plaintext at rest on Linux/macOS
`store.rs:147` — non-Windows writes `keypair.json` as plaintext (mode 600); only Windows wraps it with DPAPI. No passphrase option. (The code does warn on first write.) Consider an optional passphrase-encrypted keystore for non-Windows.

### L10 — `allow_dms` filtering is client-side only
`commands/chat.rs:91` and `node.rs:164` filter stranger DMs in the client. The relay still stores and serves them, and mailbox flooding (M6) by strangers is unaffected. The setting is cosmetic, not an access control.

### L11 — Signature covers only `payload`, not the envelope header
`crates/hb-core/src/envelope.rs:47` — `doc_type`, `signed_at`, and `public_key` are outside the signed bytes. Currently safe (freshness/recipient use signed payload fields; `verify()` ties `public_key` to the signature by verifying against it), but a relay/MITM can mutate `doc_type`/`signed_at` without breaking the signature. Consider binding the header into the signed material.

### L12 — No identity binding / AAD in the message AEAD
`crypto.rs:264` — `derive_key` uses a fixed HKDF `info = "hoardbook-chat-v1"` with no salt and no associated data; `encrypt_for`/`decrypt_from` pass no AAD. ECDH already binds the two key-holders, so risk is low, but adding AAD (e.g., `sender || recipient || sent_at`) would harden against any cross-context ciphertext reuse.

### M13 — iroh node-request size cap (1 MiB) much larger than other limits (peer-driven DoS) [added by reviewer: deepseek]
`crates/hb-app/src/node.rs:101` caps a node request at 1 MiB, versus 64 KiB for transfer requests (transfer.rs:110) and 6 KiB for relay publish. The largest legitimate node request is a `SendDm` envelope, which is bounded by the publish cap. A peer can force a 1 MiB allocation per connection. Recommendation: lower the node-request cap to ~64 KiB.

### L14 — README documents relay endpoints that do not exist (documentation/planning risk) [reviewers: deepseek, cdx-1]
README.md:57-60 documents `GET /v1/directory`, `GET|POST /v1/channel/:channel`, `GET /v1/name/:display_name`, none of which are in the router (`main.rs:67-72`). Per cdx-1: this is a **documentation/planning risk, not current exploitable surface** — do not treat these as live attack surface unless/until the routes are added. They imply a public directory / channel / name-registry whose authz model will need its own review before shipping.

### L15 — `wipe_data` is irreversibly destructive with no confirmation gate [added by reviewer: deepseek]
`commands/identity.rs:198` deletes all local data and is invokable by any webview JS. Same root cause as M4 (no CSP, sensitive command exposed). Recommendation: require an OS-level confirmation dialog before wiping.

### L16 — Heartbeat is verified over a server-reconstructed body, not a self-contained signed envelope [added by reviewer: deepseek]
`handlers.rs:132` rebuilds `HeartbeatBody` from individual request fields and verifies the signature over the re-serialized value. Correct today, but brittle: if `HeartbeatBody`'s schema and the wire fields ever diverge, verification could pass over different bytes than the client signed. Recommendation: use a full `SignedEnvelope` for heartbeats (as messages do), or add a byte-for-byte cross-check test against a client-signed reference.

---

## Verified-good (no action)
- `verify_strict` used for signature verification (rejects malleable/weak-key sigs) — crypto.rs:297.
- Random 24-byte XChaCha20-Poly1305 nonce per message; correct Ed25519→X25519 conversion with clamping — crypto.rs:186,237.
- Relay verifies envelope signatures on publish and reconstructs the signed heartbeat body server-side — handlers.rs:57,132.
- Client re-verifies signatures on every fetched message and de-dupes — relay.rs:184.
- All SQL uses parameter binding (sqlx `bind`), no string interpolation — db.rs.
- Local collection slug validation blocks `../`, null bytes, separators — collection.rs:288 + tests.
- DM recipient match enforced on direct iroh DMs — node.rs:68.
- Timestamp freshness (±300s), mailbox cap, `(from,sent_at)` dedup on publish — handlers.rs:72, db.rs:44.
- Updater uses minisign pubkey + HTTPS GitHub endpoint — tauri.conf.json:39.
- `#![forbid(unsafe_code)]` on the relay; DPAPI FFI is standard and frees the output blob.
- Read size caps on iroh framing (64 KiB xfer request, 1 MiB node request) and 6 KiB publish body.

## Reviewer agreement

Multi-LLM fan-out (round 1, retried once on the user's instruction). Lineage status: **codex = partial-agree, opencode = agree, gemini = disabled.** Formal lineage quorum (≥1 codex + ≥1 opencode in agreement) was not stamped because codex returned `agree="partial"` (constructive), but its disagreement was a *missing finding*, not a rejection — addressed below.

- **deepseek (opencode):** `agree`. Independently verified all original findings (H1–H3, M4–M8, L9–L12) against source with line-accurate evidence; verified-good section confirmed. Added M13 (1 MiB node-request cap), L14 (README endpoints), L15 (`wipe_data` no confirmation), L16 (heartbeat reconstruction). All folded in.
- **cdx-1 (codex):** `partial`. Confirmed the high-priority findings are real. Raised one valid missed finding → added as **H17** (`require_follow` bypass via unauthenticated `requester_hb_id`). Recommended reframing the README endpoints as documentation risk → applied to **L14**.
- **kimi (opencode):** failed to produce output on both attempts (stub only); deepseek covered the opencode lineage.

All reviewer-raised findings that I could verify against source were incorporated. No reviewer contradicted any original finding; the only substantive delta was H17 (added) and the L14 reframe. A round 2 (to flip codex to a clean `agree`) was not run — codex's points were unambiguous and verifiable, and this is an audit deliverable with no merge gate. Re-run `work review --round 2` if a formally-stamped quorum is desired.
