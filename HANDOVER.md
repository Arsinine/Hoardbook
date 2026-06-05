# Session Handover

**Status as of 2026-06-05:** Security fixes (workstreams A–F) committed to `main` in `02fd1a4`. T18, T19, T26, T27, T28 marked complete in MILESTONE1.md. Working tree is clean.

> Build note: `/mnt/c` (WSL2 9p mount) throws intermittent I/O errors (`os error 22`, `rustc-LLVM IO failure`) under heavy compile load, and the host C: drive runs near-full. If a build dies with an I/O error, just re-run it (artifacts persist). `CARGO_INCREMENTAL=0` reduces the churn.

---

## Done this session (verified green)

### Workstream A — Transfer-protocol identity binding
- **H2** `crates/hb-app/src/transfer.rs` `download_file` — verifies `peer_addr.id == EndpointId::from_bytes(hb_id_decode(expected_peer_hb_id))` **before** `endpoint.connect()`; rejects relay/MITM-substituted node addrs. `crates/hb-app/src/commands/sharing.rs` `request_download` now passes `peer_hb_id` (was `_peer_hb_id`, ignored) and dropped the unused `identity` state param.
- **H17** `transfer.rs` `handle_xfer_connection` — `require_follow` now checks contacts against `conn.remote_id()` (authenticated iroh identity), not a self-claimed field. `requester_hb_id` **removed from `XferRequest`** entirely.
- **M7** `transfer.rs` — `is_valid_slug(&req.slug)` rejected before any fs access.
- **M8** `transfer.rs` — `tokio::fs::canonicalize` on root + file, `starts_with` check (symlink-escape + Windows UNC safe), keeping the existing `..`/absolute reject.

### Workstream B — Relay transport + mailbox auth
- **H1** `crates/hb-app/src/relay.rs` — `is_acceptable_relay_url` requires `https://` unless `HB_ALLOW_INSECURE_RELAY=1`; applied in `new`/`set_relay_urls`/`check_url`; `fetch_messages` hard-errors when zero relays remain (fail-loud). Bootstrap stays `http://` (filtered out in release → forces fail-loud until a TLS relay exists).
- **H3** `crates/hb-relay/src/handlers.rs` `get_messages` — requires `?signed_at&signature`; verifies freshness + JCS-signed `{purpose:"hoardbook.mailbox.read.v1", public_key, signed_at}`, reconstructed from the **path** pubkey so the signed key must equal the mailbox key. Client side: `relay.rs::fetch_messages` signs + sends the query (now takes `&HoardbookKeypair`; caller `chat.rs::get_messages` updated). GET endpoints (`get_messages`, `get_peer`) now rate-limited (added `ConnectInfo`).

### Workstream C — Relay DoS hardening
- **M5** `crates/hb-relay/src/state.rs` — `RateLimiter::sweep()` + opportunistic eviction in `check`; periodic 60s sweep task in `crates/hb-relay/src/main.rs`.
- **M6** `crates/hb-relay/src/db.rs` — `count_messages_from_to` + `count_messages_from`; `handlers.rs::publish` enforces `MAX_MESSAGES_PER_PAIR=50` and `MAX_MESSAGES_PER_SENDER=200` (global 500 kept).
- **M13** `crates/hb-app/src/node.rs` — node-request cap 1 MiB → 64 KiB.

### Workstream D — Webview hardening (Rust/config parts only)
- **M4 CSP** `crates/hb-app/tauri.conf.json` — `csp` set (was `null`) to a strict `default-src 'self'` policy with Tauri `ipc:`/`asset:` allowances. **Needs a runtime smoke test** (see Remaining).
- **zeroize** `crates/hb-app/Cargo.toml` (+dep) and `crates/hb-app/src/commands/identity.rs` — transient `[u8;32]` private-key buffers in `import_keypair`/`get_identity` are `.zeroize()`d after use.

### Workstream E — Crypto/envelope hardening (format-breaking; OK, no userbase)
- **L11** `crates/hb-core/src/envelope.rs` — `create`/`verify` now sign `{header:{doc_type,public_key,signed_at}, payload}` via new `signing_value()` helper (was payload-only). Header tamper now breaks the signature.
- **L12** `crates/hb-core/src/crypto.rs` — `encrypt_for`/`decrypt_from` take an `aad: &[u8]`. `chat.rs::message_aad` binds JCS `{from,to,sent_at}` (all unencrypted fields → reconstructable pre-decryption).
- **L16** `envelope.rs` — added `DocType::Heartbeat`. `handlers.rs::heartbeat` now takes `Json<SignedEnvelope>` and verifies it (old hand-reconstruction deleted). Client `relay.rs::send_heartbeat` sends a `SignedEnvelope`.

### Workstream F — Docs
- `README.md` relay API table corrected (removed nonexistent `/v1/directory|channel|name` → marked "Planned"; updated publish/heartbeat/peer/messages descriptions) + Security notes (https requirement, mailbox/operator trust boundary, `allow_dms` is client-side, key-at-rest).

### New tests added
- hb-core: `verify_rejects_tampered_header` (L11), `aad_mismatch_fails_decrypt` (L12).
- hb-relay: `get_messages_valid_auth_returns_ok`, `_unsigned_or_forged_rejected`, `_wrong_identity_rejected`, `_stale_auth_rejected` (H3). Heartbeat tests migrated to `SignedEnvelope`.

---

## Remaining TODOs (as of 2026-06-05)

### Blocking MVP (Checkpoint 8)

1. **T20 — iroh-direct profile fetch.** `browse.rs` commands exist and `relay.fetch_peer()` retrieves `node_addr` from the relay. The remaining piece: use that `node_addr` to open an iroh connection, send a `get_profile` request (T17 protocol), receive + verify signed envelopes, and populate `profile`/`collections` on the returned peer. Stale-cache fallback and offline error card also need wiring.

2. **T24 — iroh-first DM send.** `send_message` works via relay today. Needs: check online status + `node_addr`, attempt iroh `send_dm`, fall back to `relay.publish()` only on failure.

3. **T25 — DM inbox deduplication + placeholder.** Real-time `dm-received` event and relay poll both work. Remaining: dedup across sources by `(from_key, sent_at)`, `[Unable to decrypt]` placeholder on decryption failure, inbox-grouped-by-sender UI.

4. **T21 — Follow/contact UX gaps.** Multi-group membership, drag-and-drop reassignment, and group picker shown on follow are not confirmed implemented.

### Security / quality (pre-ship)

5. **Frontend confirm dialogs.** `export_keypair`, `save_keypair_file`, `wipe_data` callable from webview with no confirmation. Wire `tauri-plugin-dialog` confirm modal in Svelte UI before invoking these commands.

6. **CSP runtime verification.** CSP in `tauri.conf.json` untested against live SvelteKit webview — run `npm run tauri dev` and confirm nothing is blocked.

7. **Transfer integration tests.** `handle_xfer_connection`/`download_file` need inner-fn refactor (like `node.rs::handle_node_stream` does with duplex streams) to test without live QUIC.

8. **`cargo clippy --workspace`** — run before any release tag.

### Infra

9. **Bootstrap relay TLS.** `relay.rs` ships `http://141.98.199.138:3000`, filtered in release builds. Stand up `https://` TLS endpoint and update `BOOTSTRAP_RELAYS` or release clients have no default relay.

## Out of scope (tracked, intentionally not built)
- Signed per-file SHA-256 in collection listings (complements H2 content integrity).
- Passphrase-encrypted keystore on Linux/macOS (L9).
- Relay-enforced `allow_dms` (L10).
- Server-issued nonce for mailbox-read (H3 currently accepts a ±300s replay window over HTTPS — documented accepted risk).
