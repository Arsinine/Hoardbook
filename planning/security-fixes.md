# Security Fixes — implementation plan

Branch: `feat/security-fixes` · task_id: `security-fixes`
Source of findings: `planning/security-review.md` (multi-LLM-reviewed). Scope chosen by user: **all findings, incl. H1 + H3.**
Git policy: surgical staging — commit only files touched by these fixes; leave the pre-existing 214 working-tree changes untouched.

## Problem
The audit found the P2P/relay layer never binds to the authenticated iroh peer identity (even though that identity == the Ed25519 `hb_id`), the relay is reached over plaintext HTTP, the DM mailbox is world-readable, and several DoS / hardening gaps exist. This plan fixes them, grouped into coherent workstreams.

## iroh 0.98 API facts (verified against cached crate source)
- `iroh::EndpointAddr { id: EndpointId, addrs: BTreeSet<TransportAddr> }` — `iroh-base-0.98.0/src/endpoint_addr.rs:42`.
- `type EndpointId = PublicKey` — `key.rs:70`. `PublicKey::from_bytes(&[u8;32]) -> Result<Self, KeyParsingError>` (validates the point) — `key.rs:122`. `PublicKey::as_bytes() -> &[u8;32]`.
- `Connection::remote_id(&self) -> EndpointId` (from the peer's TLS cert, authenticated) — `iroh-0.98.1/src/endpoint/connection.rs:564`.

---

## Workstream A — Transfer-protocol identity binding (H2 + H17)
New helper module `crates/hb-app/src/transfer.rs` (or a small `identity_check` fn) shared by both sides.

### H2 — client verifies the download peer's node id matches the expected hb_id
- `request_download` (sharing.rs:45): rename `_peer_hb_id` → `peer_hb_id`, pass it into `download_file`.
- `download_file` (transfer.rs:309): after deserializing `peer_addr: EndpointAddr` (transfer.rs:343) and **strictly BEFORE the `endpoint.connect()` call (transfer.rs:346)**, compute
  `let expected = iroh::EndpointId::from_bytes(&hb_core::hb_id_decode(peer_hb_id)?)?;`
  and reject if `peer_addr.id != expected` (error: "peer address does not match expected identity"). Doing it before `connect()` means no QUIC handshake is ever opened to an attacker-substituted address. This defeats relay/MITM `node_addr` substitution because the attacker can't present a TLS cert for the victim's key.
- Keep `expected_sha256` support; out of scope to repopulate listings with hashes (tracked as follow-up), but the node-id binding already closes the substitution path.

### H17 — server authorizes `require_follow` against the authenticated remote id, not request JSON
- `handle_xfer_connection` (transfer.rs:102) receives `conn`. Derive `let remote_hb_id = hb_core::hb_id_encode(conn.remote_id().as_bytes());` BEFORE reading the request.
- Replace the `req.requester_hb_id` check (transfer.rs:128-137) with a contacts lookup against `remote_hb_id`.
- **Remove `requester_hb_id` from `XferRequest` entirely** (wire-format break, acceptable at v0.1.0 alongside Workstream E). Eliminates the honor-system field as a permanent footgun rather than leaving it deprecated-but-present. Update the client `download_file` request construction accordingly.
- Same change applies to `handle_node_connection` / DM accept path if it ever gates on identity (it currently validates `msg.to`, which is fine; no change needed there, but note `conn.remote_id()` is now available for future authz).

### M7 — validate remote-supplied `req.slug` before any filesystem access
- At the top of `handle_xfer_connection`, after parsing `XferRequest`, reject if `!collection::is_valid_slug(&req.slug)` (reuse the existing validator; may need to make it `pub(crate)` reachable from transfer.rs, or duplicate the 2-line check). Prevents path traversal / existence-probing via `req.slug` reaching `share_settings_path`.

### M8 — canonicalize the joined file path and confirm it stays within root (symlink-safe)
- After building `file_path = Path::new(&root).join(rel)` (transfer.rs:170) and before opening, `canonicalize()` both `root` and `file_path` and verify the canonical file path `starts_with` the canonical root; reject otherwise. This closes symlink escapes that the `..`/absolute check misses. Handle the not-yet-exists case (canonicalize fails on missing files) by canonicalizing the parent or erroring as "file not found".

Tests: spoofed `requester_hb_id` no longer exists (compile-time); a non-contact authenticated id is rejected under require_follow; matching authenticated id accepted; download refuses a mismatched `EndpointAddr.id`; invalid `req.slug` rejected; a symlink inside root pointing outside root is refused; **combined MITM integration test** — a mock relay returns a third party's `node_addr` for a target peer, and the H2 identity check rejects it before any bytes flow.

---

## Workstream B — Relay transport + mailbox auth (H1 + H3)

### H1 — require HTTPS for relays (gated, ships independent of infra)
- `relay.rs`: add `fn is_acceptable_relay_url(url) -> bool` — must be `https://`. Apply in `set_relay_urls`, `check_url`, and at construction (filter out rejected URLs with a `tracing::warn!`).
- **Insecure relays are allowed ONLY when `HB_ALLOW_INSECURE_RELAY=1` is set in the environment** (read once at startup). This is intended for dev/test and is OFF by default in release, so the code can merge without waiting on infra. Emit a loud `warn!` when the flag is active. This replaces a string-based localhost exemption (which deepseek correctly flagged as DNS-rebinding-bypassable) — the gate is the env flag, not the hostname.
- Change `BOOTSTRAP_RELAYS` to `https://`. **Infra caveat (in risks):** the relay binary serves plain HTTP behind a TLS-terminating reverse proxy; the operator must front the bootstrap host with TLS before release, else release clients (flag off) lose connectivity. Add a `relay/README` note.
- Tests set `HB_ALLOW_INSECURE_RELAY=1` (or call the validator with the flag forced) so they don't need a live TLS endpoint.

### H3 — authenticate mailbox reads (signed-timestamp request)
- Relay `GET /v1/messages/:pubkey` gains required auth via query params `?signed_at=<rfc3339>&signature=<hex>`:
  - Verify `timestamp_is_fresh(signed_at)` (reuse existing ±300s helper).
  - Reconstruct the signed value server-side (same pattern as heartbeat to avoid canonicalization ambiguity): `MailboxReadRequest { public_key, signed_at }`, verify `crypto::verify(pubkey, value, signature)`.
  - Only then return messages. This proves possession of the recipient private key → third parties can no longer harvest mailboxes.
  - **Replay window (accepted risk, documented):** a captured signed read-request is replayable within the ±300s freshness window. Post-H1 the request only travels over HTTPS, so capture requires breaking TLS; and a replay only lets the holder re-read the *recipient's own* mailbox (no new info disclosed to a party who couldn't already produce the signature). Accepted for v0.1.0; a server-issued one-time nonce is the future hardening if needed. Documented in the README migration note.
- Add the rate-limiter to the GET endpoints too (presence + messages) to blunt hammering.
- Client `fetch_messages` (relay.rs:165) signs `{public_key, signed_at}` and appends the query params.
- Keep messages stored (multi-device fetch) — auth, not delete-on-read, is the fix. (Delete-on-read noted as optional future.)
- `get_peer` presence stays public (others need your `node_addr` to connect); the node-id binding (H2) makes a lying relay non-exploitable. Documented, not changed.
- Migration note: this is a breaking relay API change; acceptable at v0.1.0. Bump relay route understanding in README.

Tests: unauthenticated GET rejected; valid signed request returns messages; stale/forged signature rejected; wrong-key signature rejected.

---

## Workstream C — Relay DoS hardening (M5 + M6 + M13)
- **M5**: `RateLimiter::check` opportunistically evicts entries whose window expired, plus a periodic sweep task in `main.rs` (every `window`), or switch to a capacity-bounded map. Simplest: on each `check`, if map grows beyond N, drop expired entries; and a background sweep every 60s. Add a test that stale entries are removed.
- **M6**: change the mailbox cap to per-`(from_key, to_key)`. New `count_messages_from_to(pool, from, to)` and enforce a per-sender cap (e.g., 50) in `publish`, keeping the global 500 as a backstop. Test: one sender can't exceed the per-pair cap; a second sender still gets through.
- **M13**: lower node-request cap (node.rs:101) from 1 MiB to 64 KiB to match transfer. Test: oversized request rejected.

---

## Workstream D — App command / webview hardening (M4 + L15)
- **M4 (CSP)**: set a strict CSP in `tauri.conf.json` (`default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' https: ...`). Verify the SvelteKit build runs under it (dev uses `devUrl`; may need a dev-only relaxation). 
- **M4 (key export) + L15 (wipe)**: gate `export_keypair`, `save_keypair_file`, and `wipe_data` behind an explicit confirmation. Use the Tauri dialog plugin (`tauri-plugin-dialog`) `ask`/`confirm` invoked from the frontend immediately before the command, AND keep the command itself — defense in depth is the CSP + the fact these are now the only sensitive sinks. (If adding the dialog plugin is too heavy, minimum viable: require a `confirm: true` argument the UI only sets after a modal, documented as not a hard security boundary.) Decision recorded for reviewers: prefer the dialog-plugin confirm.

---

## Workstream E — Crypto / envelope hardening (L11 + L12 + L16) — FORMAT-BREAKING
These change signed/encrypted bytes; existing signed data and ciphertext become invalid. Acceptable at v0.1.0 (no deployed userbase guaranteed), but flagged.
- **L11**: include `doc_type`, `signed_at`, `public_key` in the signed material. Implement by signing a canonical header+payload object (e.g., `{header:{doc_type,signed_at,public_key}, payload}`) rather than payload alone. `SignedEnvelope::create`/`verify` updated together. All existing signing call sites unaffected in shape.
- **L12**: add AEAD AAD to `encrypt_for`/`decrypt_from` = canonical `{from, to, sent_at}` or at least `from||to`. Both sides must derive identical AAD; `from`/`to` are available (sender/recipient pubkeys). Caller passes context.
- **L16**: convert heartbeat to a `SignedEnvelope` (add `DocType::Heartbeat`) so the signed bytes are self-contained, eliminating the brittle server-side reconstruction — done in this workstream (format-breaking is already accepted here), not deferred. Add the byte-for-byte regression test alongside.

---

## Workstream F — Docs / low-risk (L9 + L10 + L14)
- **L14**: remove the non-existent `/v1/directory`, `/v1/channel/*`, `/v1/name/*` rows from README.md:57-60 (or move under a clearly-marked "Planned (not yet implemented)" heading).
- **L10**: document that `allow_dms` is a client-side display filter, not relay-enforced; optionally enforce at relay publish (reject to recipients who set a "no stranger DMs" flag) — defer enforcement, document for now.
- **L9**: document plaintext-at-rest on Linux/macOS; optional passphrase keystore deferred (tracked, not built — large surface).

---

## Edge cases & risks
- **H1 connectivity break**: switching bootstrap to https without a TLS-fronted server bricks relay connectivity for release clients (flag off). MUST coordinate infra before release. The `HB_ALLOW_INSECURE_RELAY=1` flag keeps dev/test green without a live TLS endpoint.
- **H3 / H1 are breaking relay API changes**: old clients stop working. Fine pre-1.0; note in README.
- **E (L11/L12) invalidates existing signed/encrypted data**: anyone with stored envelopes/messages loses them. Acceptable now; do it before any real userbase.
- **CSP** may break the SvelteKit webview (inline styles, dev server). Verify build + dev run; relax minimally.
- **iroh `remote_id()`** requires the connection to have completed the TLS handshake before `accept_bi()`; it does by the time we handle the stream.
- **PublicKey::from_bytes is fallible** (rejects non-canonical points) — propagate as a clean error, not panic.

## Test strategy
- Unit/integration in each crate's existing `#[cfg(test)]` modules. New tests: download node-id mismatch rejected; `require_follow` honors authenticated id and rejects spoofed `requester_hb_id`; mailbox GET auth (valid/stale/forged/wrong-key); rate-limiter eviction; per-pair mailbox cap; node-request size cap; envelope header tamper rejected (L11); AAD mismatch fails decrypt (L12); heartbeat reconstruction cross-check (L16); relay-url https enforcement (insecure allowed only under `HB_ALLOW_INSECURE_RELAY=1`).
- Gate: full `cargo test --workspace` green before code-review fan-out. `cargo clippy` clean.

## Out of scope (tracked follow-ups, not built here)
- Repopulating signed per-file SHA-256 in collection listings (complements H2).
- Passphrase-encrypted keystore on Linux/macOS (L9).
- Relay-enforced `allow_dms` (L10 enforcement).
- Standing up the actual TLS relay endpoint (infra, not code).

## Chorus review revisions (applied — round 1, verdict `request_changes`)
Concrete deltas folded in before implementation, per reviewer findings:

1. **iroh API precision (codex, opencode):** in `handle_xfer_connection`, `conn` is a completed `Connection` (post-handshake), so `conn.remote_id() -> EndpointId` is infallible and authenticated — confirm at impl time the exact re-export path (`iroh::EndpointId` vs `iroh::PublicKey`) and pin iroh to `=0.98.1`. `hb_core::hb_id_decode` already returns `[u8;32]`, so `EndpointId::from_bytes(&hb_id_decode(..)?)` type-checks; still wrap any `Vec`-sourced bytes with explicit `try_into::<[u8;32]>()` + clean error, never `unwrap`.
2. **H3 signing format (codex, opencode, gemini):** the signed `MailboxReadRequest` MUST be **JCS-canonicalized** (same as every other signed object) and carry a purpose/version field: `{ purpose: "hoardbook.mailbox.read.v1", public_key, signed_at }` for domain separation. Server MUST reject unless the signed `public_key` **equals the `:pubkey` path param** (prevents a valid signature for key A reading key B's mailbox). Add a clock-skew note (±300s also bounds future-dated).
3. **H1 fail-loud (opencode):** if every relay URL is filtered out by the https check and `HB_ALLOW_INSECURE_RELAY` is unset, the client MUST surface a hard error on first relay use — never silently operate with zero relays. Document relay TLS-cert trust as a known assumption (pinning is future work). Ensure CI release jobs do NOT inherit `HB_ALLOW_INSECURE_RELAY`.
4. **L12 AAD design locked (opencode):** `sent_at` IS a plaintext field of `ChatMessage` (in the signed payload), and `from`=`envelope.public_key`, `to`=recipient hb_id are both available to the receiver — so AAD = JCS `{from, to, sent_at}` is reconstructable on both sides. Lock to that (drop the vague "or from||to").
5. **L11 serialization (opencode, gemini):** sign JCS of the **full `{header, payload}` object**, not JCS(payload) concatenated with header fields.
6. **L16 no dead path (opencode):** the relay heartbeat handler switches fully to `SignedEnvelope::verify`; delete the old server-side reconstruction so it can't rot into a working-looking bypass.
7. **Migration for format breaks (codex):** L11/L12/L16 invalidate existing local signed docs, queued relay mail, and heartbeats. Add a `version`/format marker and, on startup, purge or ignore incompatible local data with a clean log line; the relay rejects old-format envelopes with a clear error rather than treating them as corruption.
8. **M8 two-layer + Windows (opencode, gemini):** keep the existing `..`/absolute rejection AND canonicalize; if the target file doesn't exist, canonicalize the parent but still guard against parent-symlink escape; normalize Windows UNC `\\?\` prefixes consistently on both `root` and `file_path` before `starts_with`.
9. **Per-`from_key` publish cap (opencode):** add a per-sender publish rate/volume cap so a sender can't flood many distinct recipients under the existing per-IP and per-pair caps. Fold into Workstream C.
10. **Best-effort `zeroize` (opencode):** zero Ed25519 private-key bytes after use where practical (Tauri shares the webview process). Small hardening line; not a hard boundary.
11. **Relay-operator trust boundary (opencode):** document in Workstream F that the relay sees DM metadata and stores ciphertext — H3 stops third parties, not the operator.
12. **Test additions (all):** invalid-slug patterns (`../secret`, `/etc/passwd`, null byte, over-long); symlink-escape enumerated in the test list; mailbox wrong-identity (signed key ≠ path pubkey) rejected; H1 all-http-no-flag errors loudly; L11 per-field header tamper (doc_type/public_key/signed_at/truncate); L12 per-field AAD mismatch (from/to/sent_at); M5 over-threshold rejection (not just eviction); H3 clock-skew ±301s rejected; CSP asserted by an automated build check.

**Numbering note:** finding IDs (H1–H3, H17, M4–M8, M13, L9–L16) are the audit's running counter — gaps like M1–M3/L1–L8 never existed and are not omissions.

## Chorus round 2 revisions (applied — verdict `request_changes`, convergent)
Two narrow, constructive deltas; gemini approved outright. Locked before implementation:

R2-1. **L12 AAD `sent_at` must be an UNENCRYPTED field (opencode — only true blocker).** AAD = JCS `{from, to, sent_at}` where **`sent_at` = the outer `SignedEnvelope` header's `signed_at` (L11), and `to`/`from` are the plaintext envelope fields** — all available *before* decryption. Only `ChatMessage.content` is ciphertext. The recipient reconstructs AAD from the unencrypted header/envelope, then decrypts; AAD must never depend on any field inside the encrypted `content`, or the tag can't be verified pre-decryption (broken AEAD).
R2-2. **H2 decoded-ID length-safety made explicit (codex).** `hb_core::hb_id_decode` already returns `[u8; 32]` (verified — crypto.rs), so `EndpointId::from_bytes(&hb_id_decode(peer_hb_id)?)` type-checks directly with no `try_into`. If that return type ever changes to `Vec<u8>`, add `.try_into::<[u8;32]>().map_err(|_| BadId)?` before `from_bytes`. State this in code comments.
R2-3. **Migration wording + test (opencode, non-blocking).** Pick **purge** (destructive) for incompatible local data at v0.1.0 — not "purge/ignore". Add a migration-path test: old-format local data is dropped at startup with a clean log line, and the relay rejects an old-format envelope with a clear error.

## Reviewer agreement
Round 2 — Chorus `review-only`. **Verdict: `request_changes` (convergent — 1 approve, 2 constructive single-delta).**
- **gemini-cli-1 (google):** `approve` — all 12 round-1 findings resolved, iroh API + H3 + M7/M8 + migration sound.
- **codex-cli-0 (openai):** `request_changes` — single delta R2-2 (H2 decoded-ID exact-length explicitness); everything else "adequately resolved".
- **opencode-cli-2 → fell back to anthropic/claude-sonnet-4-6:** `request_changes` — single blocker R2-1 (L12 AAD `sent_at` field location), plus non-blocking R2-3.
- **Note on the opencode slot:** the intended deepseek reviewer (`deepseek/deepseek-v4-pro`) did NOT run — it has failed `verdict_ambiguous` on 19/19 attempts (produces a review but no parser-detectable verdict token) and Chorus cross-lineage-fell-back to Claude. So both "opencode" reviews in this task were actually Claude, not deepseek.

Round 1 — Chorus `review-only` (3 active CLI lineages). **Verdict: `request_changes`.**
- **gemini-cli-1 (google):** `approve`. Verified iroh API + identity-binding; flagged Windows UNC canonicalization, strict signature↔path binding for H3, and a clock-skew test.
- **codex-cli-0 (openai):** `request_changes`. iroh connection-state precision (`Connecting` vs completed `Connection::remote_id`), `from_bytes` length safety, env-flag test-determinism, H3 domain separation, format-break migration step, backend-dialog layering.
- **opencode-cli-2 (opencode→fell back to anthropic/claude-sonnet-4-6):** `request_changes`. L12 AAD `sent_at` reconstructability (critical), H3 JCS serialization unspecified, H1 silent no-relay fallback, M8 two-layer defense, missing per-`from_key` publish limiter, zeroize, and a detailed test-gap list.

All concrete, valid findings folded into "Chorus review revisions" above. The two `request_changes` were constructive (tightening, not rejection); plan is implementation-ready with those deltas. Cockpit: http://127.0.0.1:5050/runs/security-fix-implementation-plan-for-hoardbook-a-tauri-rust
