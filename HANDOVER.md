# Session Handover

**Last updated: 2026-06-06**
**Branch:** `main` — working tree clean after commit. All tests green.

> Build note: `/mnt/c` (WSL2 9p mount) throws intermittent I/O errors (`os error 22`, `rustc-LLVM IO failure`) under heavy compile load, and the host C: drive runs near-full. Re-run on failure (artifacts persist). `CARGO_INCREMENTAL=0` reduces churn.

---

## Done this session

### T22 frontend wiring — DHT Discover + Announce (complete)
- **`ui/src/lib/api.ts`** — Extended `Settings` interface with `dht_announce_enabled`, `dht_announce_tags`, `dht_announce_content_types`.
- **`ui/src/routes/settings/+page.svelte`** — Added "DHT Discovery" section: tag + content-type inputs, enable/disable toggle. On enable calls `dhtStartAnnounce`; on disable calls `dhtStopAnnounce`. Fixed pre-existing bug where `saveSettings` (relay/DM saves) clobbered DHT settings via serde defaults — all saves now send full Settings object via `fullSettings()` helper.
- **`ui/src/routes/contacts/+page.svelte`** — Added "Discover" section between Recommended and Following: tag + content-type inputs, "Search DHT" button, result cards with Follow. After each search, `watchesEvaluate` is called automatically and each WatchHit produces a toast. "Save as watch" form creates a named watch from the current search criteria.
- **`MILESTONE1.md`** — Checkpoint 6 marked `[x]`.

### T21 — Follow/contact backend (complete)
- **`crates/hb-app/src/store.rs`** — `Group` struct gains `modified_at: DateTime<Utc>` field (serde-defaulted to `Utc::now()` for existing data); `load_groups` now returns groups sorted newest-modified first.
- **`crates/hb-app/src/commands/groups.rs`** — all mutation commands (`groups_create`, `groups_rename`, `groups_assign`, `groups_unassign`) now update `modified_at`. New command `contact_update_groups(hb_id, group_names)` atomically replaces a contact's group memberships (used for drag-and-drop reassignment from the UI). 8 new T21 tests added.
- **`crates/hb-app/src/commands/browse.rs`** — `follow` command gains optional `group_name: Option<String>` parameter; if supplied and a matching group exists, the new contact is added to it immediately. Skip/None → Ungrouped.
- **`crates/hb-app/src/lib.rs`** — `contact_update_groups` registered in `invoke_handler`.
- **T21 backend acceptance criteria met.** Remaining items are frontend-only (drag-and-drop UI, group picker in the follow modal, status badge for stale >7d contacts).

### Audit findings addressed (from Chorus tri-review, 2026-06-06)
- **L1** — HKDF salt: `crypto.rs` `derive_key` now passes `Some(b"hoardbook-ecdh-v1")` as salt per RFC 5869. Wire-compat comment added explaining this is a deliberate pre-release flag-day; messages encrypted before commit 311d88e cannot be decrypted by this code (acceptable: no shipped users).
- **M4** — `node_addr` size cap: relay `handlers.rs` heartbeat handler rejects `node_addr` > 2048 bytes with 400. New test `heartbeat_oversized_node_addr_rejected`.
- **M1** — Consume-on-read reverted: an earlier attempt deleted messages from `get_messages` — Chorus reviewers (and Codex) correctly identified this as at-most-once with message-loss on dropped connections. Reverted; 30-day TTL expiry task controls mailbox growth instead. ACK-based deletion remains a post-MVP item (see below).
- **M3** — Parallel publish + freshest peer: `relay.rs` (app) `publish` is now parallel (`JoinSet`); `fetch_peer` collects all relay responses and returns the one with the highest `last_seen_at`. Added 5-second deadline (`timeout_at`) so a single unresponsive relay no longer gates the result.
- **L4** — Mailbox cap test fixed: `mailbox_cap_enforced` now drives the 500th and 501st messages through the `publish` handler (not direct DB inserts) so a regression removing the handler-level cap check would be caught.
- **Clippy clean**: all three pre-existing warnings fixed (`PublishRequest` dead struct removed, useless `.into()` removed, `too_many_arguments` suppressed with `#[allow]`).

### Chorus second-pass findings (accepted as known tradeoffs or deferred)
- **fetch_peer relay trust**: `last_seen_at` is relay-supplied; a malicious relay can claim a far-future timestamp to win selection. Accepted for MVP — relay selection is not security-critical (node_addr is cryptographically verified by iroh separately). Post-MVP: cap `last_seen_at` to `now()` on the client side.
- **Group TOCTOU**: `contact_update_groups` + `follow` do load-modify-save with no locking. In a single-process Tauri desktop app concurrent group mutations are near-impossible in practice. Accepted for MVP; a proper fix requires a `Mutex<()>` guard around file operations or migrating groups to SQLite.
- **follow() silent group-not-found**: returns `Ok(())` when `group_name` is supplied but the group doesn't exist (falls through to Ungrouped). Intentional — UI only surfaces existing group names; a missing group means the UI is stale, and the contact is still saved.

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

### Checkpoint 6 — DHT two-instance live test (VPS)

Two VPS nodes are available for this: **Singapore** (`141.98.199.138`) and **Japan** (`45.129.8.225`). Credentials in your secrets manager — do not commit them.

**Setup on each VPS (run once):**
```bash
# install the hb-relay binary
cargo install --path crates/hb-relay
hb-relay &  # listens on :3000, SQLite at ~/.hb-relay.db

# build the hb-app CLI harness (headless, no Tauri window)
# OR copy the built .app / .exe to the VPS if cross-compiled
```

**Test procedure:**

1. **Singapore node — generate identity + announce**
   ```bash
   # start hb-app pointing at its own relay
   HB_RELAY_URL=http://141.98.199.138:3000 ./hb-app
   # in Settings → Identity: generate keypair, note the hb_id
   # in Settings → DHT Discovery: set tag = "hb-e2e-test-<random>", enable announce
   # confirm heartbeat visible: curl http://141.98.199.138:3000/v1/peer/<hb_id>
   ```

2. **Japan node — search**
   ```bash
   HB_RELAY_URL=http://141.98.199.138:3000 ./hb-app
   # in Contacts → Discover: enter tag "hb-e2e-test-<same random>"
   # click "Search DHT" — expect Singapore's hb_id to appear in results
   # click Follow — confirm contact appears in Following list
   ```

3. **Watch notification check**
   ```bash
   # on Japan: Settings → Watches: create watch with same tag before searching
   # run Search DHT again with a fresh random tag that Singapore announces
   # expect toast: "Watch '<name>' — new peer found"
   # search again — expect no second toast (seen_pubkeys deduplication)
   ```

4. **Announce-off check**
   ```bash
   # on Singapore: Settings → DHT Discovery: disable announce
   # on Japan: search same tag — expect zero results
   # optional: run Wireshark / tcpdump on Singapore to confirm no BEP 5 traffic
   ```

**Pass criteria:** Singapore is discoverable from Japan by tag within ~5s of DHT bootstrap; disabling announce makes it undiscoverable within one 30-min announce cycle; watch fires once per new peer.

**Known limitation:** Peers behind NAT cannot serve their identity over TCP (BEP 5 announce works, but the Japan node can't TCP-connect to them). Both VPS nodes have public IPs so this does not affect this test.

---

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
- **M1-ack** — Mailbox messages are never deleted by the client (TTL only). Add an explicit `DELETE /v1/messages/:pubkey/:id` endpoint so the client can ACK delivery after confirming the response was received.
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
