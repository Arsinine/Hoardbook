# Session Handover

**Last updated: 2026-06-06**
**Branch:** `main` — working tree clean after commit. All tests green.

> Build note: `/mnt/c` (WSL2 9p mount) throws intermittent I/O errors (`os error 22`, `rustc-LLVM IO failure`) under heavy compile load, and the host C: drive runs near-full. Re-run on failure (artifacts persist). `CARGO_INCREMENTAL=0` reduces churn.

---

## Done this session

### T21 — Follow/contact backend (complete)
- **`crates/hb-app/src/store.rs`** — `Group` struct gains `modified_at: DateTime<Utc>` field (serde-defaulted to `Utc::now()` for existing data); `load_groups` now returns groups sorted newest-modified first.
- **`crates/hb-app/src/commands/groups.rs`** — all mutation commands (`groups_create`, `groups_rename`, `groups_assign`, `groups_unassign`) now update `modified_at`. New command `contact_update_groups(hb_id, group_names)` atomically replaces a contact's group memberships (used for drag-and-drop reassignment from the UI). 8 new T21 tests added.
- **`crates/hb-app/src/commands/browse.rs`** — `follow` command gains optional `group_name: Option<String>` parameter; if supplied and a matching group exists, the new contact is added to it immediately. Skip/None → Ungrouped.
- **`crates/hb-app/src/lib.rs`** — `contact_update_groups` registered in `invoke_handler`.
- **T21 backend acceptance criteria met.** Remaining items are frontend-only (drag-and-drop UI, group picker in the follow modal, status badge for stale >7d contacts).

### Audit findings addressed (from Chorus tri-review, 2026-06-06)
- **L1** — HKDF salt: `crypto.rs` `derive_key` now passes `Some(b"hoardbook-ecdh-v1")` as salt per RFC 5869.
- **M4** — `node_addr` size cap: relay `handlers.rs` heartbeat handler rejects `node_addr` > 2048 bytes with 400. New test `heartbeat_oversized_node_addr_rejected`.
- **M1** — Consume-on-read: relay `handlers.rs` `get_messages` now deletes delivered messages after a successful DB fetch (`db::delete_messages_for`). Prevents unbounded mailbox growth.
- **M3** — Parallel publish + freshest peer: `relay.rs` (app) `publish` is now parallel (`JoinSet`); `fetch_peer` collects all relay responses and returns the one with the highest `last_seen_at` rather than the first.
- **L4** — Mailbox cap test fixed: `mailbox_cap_enforced` now drives the 500th and 501st messages through the `publish` handler (not direct DB inserts) so a regression removing the handler-level cap check would be caught.
- **Clippy clean**: all three pre-existing warnings fixed (`PublishRequest` dead struct removed, useless `.into()` removed, `too_many_arguments` suppressed with `#[allow]`).

### Previous session (preserved for context)
- T20 — iroh-direct profile fetch (`c912b3e`, `4c9dc00`)
- T24 + T25 — iroh-first DM send + unified inbox (`1330028`)
- Security fixes A–F (`02fd1a4`)

---

## Test counts

| Crate | Tests |
|---|---|
| hb-core | 42 |
| hb-relay | 41 |
| hb-app | 74 |

`cargo clippy --workspace -- -D warnings` — zero warnings/errors.

---

## What's next

### T21 frontend gaps (not blocking backend)
- **Group picker in follow modal** — `follow` command now accepts `group_name?: string` from JS. Wire the picker UI.
- **`contact_update_groups` wiring** — drag-and-drop reassignment should call `contact_update_groups(hb_id, [newGroupName])`. Command is registered and ready.
- **Status badge for stale contacts** — a `CachedPeer` with `last_fetched` > 7 days ago should show a "Stale" badge in the contact list.

### Checkpoint 4 smoke test (manual, no code needed)
- Two local instances: A publishes profile + collection, B pastes A's hb_id.
- Confirm B's profile card shows A's data and no profile/collection HTTP traffic hits the relay.

### Security (pre-ship, not MVP blockers)
- **Frontend confirm dialogs**: `export_keypair`, `save_keypair_file`, `wipe_data` callable with no confirmation. Wire `tauri-plugin-dialog` confirm modal in Svelte.
- **CSP smoke test**: run `npm run tauri dev` and confirm nothing blocked (check `connect-src`/`img-src`).
- **Transfer integration tests**: `handle_xfer_connection`/`download_file` need inner-fn refactor (duplex pattern) to test without live QUIC.

### Remaining audit findings (open)
- **H1** — Mailbox read tokens are replay-reusable (±300s window, no nonce tracking). Re-evaluate once TLS bootstrap relay is live.
- **H2** — Linux private key stored as plaintext JSON. Use `keyring` crate (secret-service) on Linux.
- **H3** — DM queue is in-memory; restarts silently lose messages. Persist to SQLite, drain on delivery.
- **H5** — IP-only rate limiting. Add per-sender key rate limit on `publish`.
- **M2** — `get_messages` fetches full mailbox every poll. Add `?since=<ISO8601>` parameter.
- **M5** — `resolve_peer` clones `Option<iroh::Endpoint>` across an await.
- **L2** — Dedup key `(sender, sent_at_rfc3339)` collides for same-second messages. Add nonce to `ChatMessage`.
- **L3** — `read_json_lenient` swallows parse errors silently for contacts.

### Infra
- **Bootstrap relay TLS**: `relay.rs` ships `http://141.98.199.138:3000`. Stand up `https://` TLS endpoint and update `BOOTSTRAP_RELAYS`.

---

## Out of scope (intentionally not built)
- Signed per-file SHA-256 in collection listings
- Passphrase-encrypted keystore on Linux/macOS
- Relay-enforced `allow_dms`
- Server-issued nonce for mailbox-read auth (accepted risk over HTTPS)
- Two-pane directory viewer UI for browse (T20 frontend polish)
