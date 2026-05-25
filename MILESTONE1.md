# HOARDBOOK — IMPLEMENTATION PLAN
**Version:** 2.1 | **Based on spec:** v0.4 + owner decisions | **Date:** 2026-05-23

---

## DECISIONS LOG

All open questions from v1.0 resolved:

| # | Question | Decision |
|---|---|---|
| 1 | DM wire format | Existing code: static X25519 from Ed25519 identity key, `base64(nonce[24]\|\|ciphertext)`. No ephemeral keys. |
| 2 | `Profile.email`, `.location`, `.social_links` | Keep. Document as approved extensions beyond the base spec. |
| 3 | `DirectoryItem.sha256` | Remove from signed payload entirely. |
| 4 | `POST /v1/deactivate` | Remove. Relay no longer stores profile/collections; endpoint is moot. |
| 5 | Rate-limit HTTP status | 429 Too Many Requests. |
| 6 | Key rotation / succession | Remove entirely. One identity forever. No succession documents, no rotation flow. |
| 7 | Private key storage | Local DPAPI-encrypted file (`~/.hoardbook/identity/keypair.bin`) on Windows. Plain `chmod 600` JSON file on Linux. No Credential Manager, no keyring crate. |

---

## ARCHITECTURAL DECISIONS

Two major decisions taken by the product owner that supersede the original spec:

### 1. Background Service Model

Hoardbook runs like a BitTorrent client. Pressing the window close button minimises to the system tray — it does not terminate the process. The iroh endpoint, heartbeat task, and DHT announce all keep running. The node is its own server. Peers connect directly via iroh QUIC to fetch profile and collections. Explicitly quitting from the tray icon terminates the process.

### 2. Relay Demoted to Bootstrap + DM Relay

The relay no longer caches profile or collection documents. Its only jobs are:

- **Heartbeat store** — who is online and what is their iroh NodeAddr
- **DM store-and-forward** — async messaging when the recipient's node is temporarily offline
- **Relay peering / health** — discovery of community relay nodes

Profile and collection data is served peer-to-peer by the running node over iroh. The relay is a bootstrap node in the BitTorrent sense, not a content cache.

### Consequences

| Component | Before | After |
|---|---|---|
| Relay `documents` table | Stores signed profiles + successions | **Dropped** |
| Relay `collections` table | Stores signed collection snapshots | **Dropped** |
| `POST /v1/publish` | Accepts profile/collection/succession/message | **Messages only** |
| `GET /v1/peer/:pubkey` | Returns profile + collections + online status | **Online status + NodeAddr only** |
| Profile editor "Publish" | Pushes to relay | Signs locally; served via iroh |
| Collection "Publish" | Pushes to relay | Signs locally; served via iroh |
| Key rotation / succession | Planned feature | **Removed** |
| Offline peer display | Shows relay-cached data | Shows **local contact cache** (stale badge) |
| DMs | Relay store-and-forward only | **Direct iroh delivery when both online; relay fallback** |
| Peer address discovery | Relay heartbeat only | Each node maintains `peers.json`; gossip exchange on every iroh connection |

### 3. Each Client is its own Relay — Gossip Peer Address Exchange

Every running Hoardbook node maintains a local peer address cache at `~/.hoardbook/peers.json`. The cache maps `hb_id → { node_addr, relay_url, last_seen_at }`. It is seeded from the bootstrap relay's heartbeat data and grows through every direct peer interaction.

**Protocol:** Whenever two nodes establish an iroh connection — for a profile fetch, a DM, or any other request — they exchange their full known peer lists via an `exchange_peers` message immediately after the primary request completes. Each node merges the received list into its own cache, deduplicating by hb_id and keeping the entry with the most-recent `last_seen_at`. The exchange runs as a background task; it never delays the operation that opened the connection.

**Effect:** The network becomes increasingly self-sufficient. A node that has been running and interacting for a while can bootstrap new peers entirely from its local cache, without contacting the relay at all. This is the same Peer Exchange (PEX) mechanic used in BitTorrent. The relay remains the entry point for brand-new nodes; long-lived nodes rely on it minimally.

**Cache limits:** Maximum 1000 entries; evict oldest by `last_seen_at` when full. Entries not updated in 30 days are evicted on startup.

---

## DEPENDENCY MAP

`A → B` = B cannot start until A is done. `∥` = can be built in parallel.

```
hb-core
  T1 (HbId) ∥ T2 (Ed25519) ∥ T3 (JCS)  →  T4 (SignedEnvelope)  →  T5 (type audit)

hb-relay (all depend on T6; T7–T11 parallel with each other)
  T5 → T6 (schema) → T7 (publish: msg only)
                    → T8 (heartbeat)
                    → T9 (get_peer: status+NodeAddr)
                    → T10 (health + peering)
                    → T11 (TTL + mailbox cap)

hb-app identity + onboarding
  T5 → T12 (identity gen + DPAPI file)
  T12 → T13 (onboarding wizard)
  T12 + T7 + T10 → T14 (profile editor — signs locally)

hb-app collection
  T12 + T5 → T15 (collection add/scan/draft)
  T15 + T14 → T16 (collection sign — local only)

iroh node + background service  [NEW]
  T12 + T16 → T17 (iroh node server — serve profile + collections + accept direct DMs)
  T12 + T8 → T18 (heartbeat task — carries NodeAddr)
  T12 → T19 (system tray + background service)

browse + follow
  T17 + T18 + T10 → T20 (browse: iroh direct; local cache fallback)
  T20 → T21 (follow + contact list + groups)

DHT
  T12 + T10 → T22 (DHT announce + search)
  T22 → T23 (saved tag watches)

DMs
  T12 + T17 → T24 (DM: direct iroh path + relay fallback)
  T24 + T10 → T25 (DM inbox: direct receive + relay poll)

polish
  T16 → T26 (export listing)  ∥
  T12 + T10 + T22 + T23 → T27 (settings page)
```

**Parallel opportunities:**
- T1 ∥ T2 ∥ T3
- T7 ∥ T8 ∥ T9 ∥ T10 ∥ T11 (all relay handlers, once T6 done)
- T14 ∥ T15 (both depend on T12, not each other)
- T17 ∥ T18 ∥ T19 (iroh server, heartbeat, tray — all depend on T12)
- T22 ∥ T24 (DHT and DMs, both depend on T12)
- T26 ∥ T27 (polish tasks)

**Circular dependencies:** None.

---

## PHASE 0 — FOUNDATION

---

### TASK 1 [x]: HbId Format — Encoding, Validation, Checksum

**Depends on:** none  **Parallel with:** T2, T3

**Scope:** `hb_id_encode(pubkey: &[u8; 32]) → String` and `hb_id_decode(id: &str) → Result<[u8; 32], HbError>`. Format: `hb1_` prefix + base58(pubkey[32] || SHA256d(pubkey)[0..4]). Distinct error variants for wrong prefix, wrong length, bad base58, and checksum mismatch. Spec §Core Concepts — "The Key".

**Acceptance criteria:**
- [x] `hb_id_decode(hb_id_encode(key)) == Ok(key)` for any 32-byte input
- [x] Flipping any character returns `Err(InvalidChecksum)` or `Err(InvalidId)`
- [x] `"hb2_..."` returns `Err(InvalidPrefix)` specifically
- [x] Non-base58 characters (`0`, `O`, `I`, `l`) rejected
- [x] Wrong-length strings rejected

**Tests required:**
Unit: `encode_decode_roundtrip` ✓, `checksum_tamper_detection` ✓ (`id_checksum_rejects_tampering`), `prefix_rejection` ✓, `length_rejection_short` ✓, `length_rejection_long` ✓, `base58_excludes_ambiguous` ✓
Integration: `hb_id_survives_envelope_roundtrip` ✓
E2E: paste own hb_id into Browse input — validates immediately before any network call

**Verification steps:**
1. `cargo test -p hb-core -- hb_id` ✓ (36/36 passing)
2. `cargo clippy -p hb-core -- -D warnings` ✓ (zero warnings)

**Definition of done:** All acceptance criteria checked; all tests passing.

---

### TASK 2 [x]: Ed25519 Keypair — Generation, Signing, Verification

**Depends on:** T1  **Parallel with:** T3

**Scope:** `HoardbookKeypair` in `hb-core/src/crypto.rs` using `ed25519-dalek`. Methods: `generate()`, `sign(payload: &Value) → String` (hex sig over JCS bytes), `verify(pubkey_bytes, payload, sig_hex) → Result<()>`, `hb_id() → String`. Private key never serialised in this module. Spec §Why Cryptographic Identity?.

**Acceptance criteria:**
- [x] `generate()` produces unique keypairs on every call
- [x] `sign` + `verify` on same payload returns `Ok(())`
- [x] Mutated payload causes `Err(InvalidSignature)`
- [x] Wrong public key causes `Err(InvalidSignature)`
- [x] Signing uses JCS-canonical bytes, not raw JSON string

**Tests required:**
Unit: `generate_uniqueness` ✓, `sign_verify_roundtrip` ✓ (`sign_and_verify`), `verify_tampered_payload` ✓ (`verify_rejects_tampered_payload`), `verify_wrong_pubkey` ✓ (`verify_rejects_wrong_key`)
Integration: `sign_order_invariant` ✓

**Verification steps:**
1. `cargo test -p hb-core -- crypto` ✓ (all passing)

**Definition of done:** All acceptance criteria checked; all tests passing.

---

### TASK 3 [x]: JCS Canonicalization (RFC 8785)

**Depends on:** none  **Parallel with:** T1, T2

**Scope:** `jcs::canonicalize(value: &Value) → Vec<u8>`. Keys sorted recursively by Unicode code point; no whitespace; correct number and string escaping per RFC 8785. All 8 official RFC 8785 Appendix B test vectors must pass. Spec §Data Model — "Canonical JSON".

**Acceptance criteria:**
- [x] `canonicalize({"b":1,"a":2})` == `b'{"a":2,"b":1}'`
- [x] Nested objects recursively sorted
- [x] Identical output on 1000 consecutive calls of the same value
- [x] All 8 RFC 8785 Appendix B test vectors pass ✓

**Tests required:**
Unit: `key_sort` ✓ (`object_keys_sorted`), `nested_sort` ✓ (`nested_object`), `deterministic_1000x` ✓ (`deterministic_across_insertion_order`), `rfc8785_test_vectors` ✓ (`rfc8785_cross_vector` + 7 new `rfc8785_b_*` tests)
Integration: `cross_runtime_byte_equality` — compare Rust output vs Node.js `canonicalize` package on same input

**Implementation notes:**
- serde_json/ryu already produces `1e+30` (with `+` sign) for `1E30` — no special handling needed
- Added whole-number float stripping: `56.0_f64` → `"56"` (RFC §3.2.2.3); serde_json/ryu emits `"56.0"`, stripped to `"56"`
- UTF-8 byte comparison for key sort is equivalent to Unicode code point order for all valid UTF-8

**Verification steps:**
1. `cargo test -p hb-core -- jcs` ✓ (13/13 passing, 43/43 overall)

**Definition of done:** RFC 8785 vectors pass; cross-runtime byte equality confirmed.

---

### TASK 4 [x]: SignedEnvelope — Create, Sign, Verify, Parse

**Depends on:** T1, T2, T3  **Parallel with:** T5

**Scope:** `SignedEnvelope` with fields `doc_type: DocType`, `payload: Value`, `public_key: String`, `signature: String`, `signed_at: DateTime<Utc>`. `DocType` enum: `Profile`, `Collection`, `Message` — **Succession removed**. Methods: `create`, `verify`, `parse_payload`. Spec §Data Model — "Signed document envelope".

**Acceptance criteria:**
- [x] `create` + `verify` returns `Ok(())`
- [x] Mutated payload field causes `Err(InvalidSignature)`
- [x] Signature survives JSON serialise → deserialise roundtrip
- [x] `DocType` has no `Succession` variant
- [x] `Option` fields absent from JSON when `None` (not serialised as `null`)
- [x] `Vec` fields always present as `[]`, never omitted

**Tests required:**
Unit: `create_and_verify`, `tampered_payload_rejected`, `json_roundtrip`, `optional_fields_absent_not_null`, `vec_fields_always_serialised`
Integration: `relay_rejects_tampered_envelope`

**Verification steps:**
1. `cargo test -p hb-core -- envelope`

**Definition of done:** All acceptance criteria checked; `DocType::Succession` absent from codebase.

---

### TASK 5 [x]: Core Type Audit — Spec Alignment

**Depends on:** T4  **Parallel with:** none

**Scope:** Resolve all code divergences against the spec and decisions log:

1. **Remove `DirectoryItem.sha256`** from the type and all serialised forms. Spec: *"No file hashes are exposed."*
2. **Remove `Collection.total_bytes` and `Collection.sorted`** — not in spec.
3. **Remove `Succession` type** from `types.rs` — key rotation removed.
4. **Document `Profile.email`, `.location`, `.social_links`** as approved extensions with a `// Approved extension: not in base spec` comment.
5. **Assert `Profile.content_types` doc-comment**: "Computed as union of all published collections; never edited directly."
6. **Verify** `ContentType` strings are the six spec values: "video", "audio", "image", "text", "software", "other".

**Acceptance criteria:**
- [x] `DirectoryItem` serialised JSON contains no `sha256` key
- [x] `Collection` serialised JSON contains no `total_bytes` or `sorted` keys
- [x] `Succession` type does not exist anywhere in `hb-core`
- [x] `DocType::Succession` does not exist anywhere in `hb-core`
- [x] All existing `hb-core` tests pass after removals

**Tests required:**
Unit: `directory_item_no_hash_in_json`, `collection_no_internal_fields`, `content_types_union_sorted_deduped`

**Verification steps:**
1. `cargo test -p hb-core` — full suite green
2. `grep -r "sha256\|Succession\|total_bytes\|\.sorted" crates/hb-core/src/` — zero results

**Definition of done:** All removals confirmed; all tests passing.

---

### CHECKPOINT 0 [x]: hb-core Foundation Complete

**Gate condition:** All hb-core types compile cleanly, `DocType::Succession` does not exist, `sha256` field does not exist, and all existing tests pass.

**Automated gate:** `cargo test -p hb-core && cargo clippy -p hb-core -- -D warnings`

**Rollback plan:** Tag before Phase 0. hb-core changes are purely additive/subtractive at the type level; relay and app crates will fail to compile if they reference removed types, making regressions immediately visible.

---

## PHASE 1 — RELAY BINARY

---

### TASK 6 [x]: Relay SQLite Schema and DB Layer

**Depends on:** none  **Parallel with:** T1–T3

**Scope:** The relay schema now has exactly two tables. **All profile, collection, and succession storage is removed.**

```sql
CREATE TABLE heartbeats (
  pubkey    TEXT PRIMARY KEY,
  last_seen INTEGER NOT NULL,
  node_addr TEXT           -- iroh NodeAddr; null in relay-only mode
);

CREATE TABLE messages (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  from_key   TEXT    NOT NULL,
  to_key     TEXT    NOT NULL,
  envelope   TEXT    NOT NULL,
  sent_at    TEXT    NOT NULL,
  stored_at  INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  UNIQUE(from_key, sent_at)
);
CREATE INDEX idx_messages_to ON messages(to_key, sent_at DESC);
```

DB layer (`db.rs`) exposes: `upsert_heartbeat`, `get_heartbeat`, `insert_message`, `get_messages_for`, `count_messages_for`, `expire_messages`, `count_stored_peers`. Migration runs the `DROP TABLE IF EXISTS documents`, `DROP TABLE IF EXISTS collections`, `DROP TABLE IF EXISTS channel_messages` cleanup already present in the existing migrate function. Schema migrations are idempotent.

**Acceptance criteria:**
- [x] Migration runs without error on fresh DB and on re-run
- [x] `documents` and `collections` tables do not exist after migration
- [x] Duplicate `(from_key, sent_at)` inserts are silently ignored (INSERT OR IGNORE)
- [x] `count_messages_for` counts only non-expired messages
- [x] `expire_messages` deletes rows where `expires_at < unixepoch()`
- [x] Heartbeat rows are never deleted by the expiry function

**Tests required:**
Unit: `migration_idempotent`, `message_dedup_silently_ignored`, `expire_removes_old_messages`, `heartbeat_not_expired`, `count_messages_non_expired_only`
Integration: `heartbeat_and_message_roundtrip`

**Verification steps:**
1. `cargo test -p hb-relay -- db`
2. `cargo sqlx prepare --check`

**Definition of done:** All acceptance criteria checked; schema matches spec exactly.

---

### TASK 7 [x]: POST /v1/publish Handler — Messages Only

**Depends on:** T4, T5, T6  **Parallel with:** T8, T9, T10, T11

**Scope:** The publish handler accepts **only `type: "message"`**. Any other type returns 400. Flow: parse `{type, document}`; reject non-message types immediately; size-check (6 KB limit on encrypted content); deserialise as `SignedEnvelope`; call `verify()`; parse `ChatMessage`; validate recipient hb_id format; check timestamp freshness (±5 min); check mailbox cap (500 messages via `count_messages_for`); insert. Returns 200 / 400 / 413 / 429 (rate limit). Spec §Relay API — "POST /v1/publish" (scoped to messages).

**Acceptance criteria:**
- [x] `type: "profile"` → 400 immediately
- [x] `type: "collection"` → 400 immediately
- [x] `type: "succession"` → 400 immediately
- [x] Valid signed message envelope → 200
- [x] Tampered message envelope → 400
- [x] Message timestamp >5 min old → 400
- [x] Invalid recipient hb_id → 400
- [x] 501st message to same recipient → 400 with "mailbox" in body
- [x] Rate limit exceeded → 429

**Tests required:**
Unit: `publish_non_message_types_rejected`, `publish_valid_message`, `publish_tampered_message`, `publish_stale_timestamp`, `publish_invalid_recipient`, `mailbox_cap_enforced`
Integration: `publish_then_fetch_messages`

**Verification steps:**
1. `cargo test -p hb-relay -- handlers::publish`

**Definition of done:** All acceptance criteria checked; non-message types hard-rejected.

---

### TASK 8 [x]: POST /v1/heartbeat Handler

**Depends on:** T4, T6  **Parallel with:** T7, T9, T10, T11

**Scope:** Verify signature over `JCS({node_addr?, public_key, signed_at})`; check timestamp freshness (±5 min); upsert heartbeat row. Rate limit: 1 request/minute per key (429 on excess). `node_addr` stored when present; set NULL when absent. Spec §Relay API — "POST /v1/heartbeat".

**Acceptance criteria:**
- [x] Valid heartbeat → 200; `last_seen` updated in DB within 1 second
- [x] Stale timestamp → 400
- [x] Invalid signature → 400
- [x] Second heartbeat from same key within 60 s → 429
- [x] `node_addr` stored when provided; NULL when not

**Tests required:**
Unit: `heartbeat_valid`, `heartbeat_stale`, `heartbeat_invalid_sig`, `heartbeat_rate_limited`, `node_addr_stored_and_cleared`
Integration: `heartbeat_sets_online_status`

**Verification steps:**
1. `cargo test -p hb-relay -- handlers::heartbeat`

**Definition of done:** All acceptance criteria checked.

---

### TASK 9 [x]: GET /v1/peer/:pubkey — Status and NodeAddr Only

**Depends on:** T6  **Parallel with:** T7, T8, T10, T11

**Scope:** Returns **only** online status and NodeAddr — no profile, no collections. Response:
```json
{
  "online":      true,
  "last_seen_at": 1716400000,
  "node_addr":   "<iroh NodeAddr base64>"
}
```
`node_addr` present only when `online: true`. `last_seen_at` is a Unix integer. Unknown key → 200 with `{"online":false,"last_seen_at":null}`. Invalid key format → 400. Online threshold: heartbeat within last 600 seconds. Spec §Relay API — "GET /v1/peer/:pubkey" (scoped to status).

**Acceptance criteria:**
- [x] Unknown valid key → 200 with `online: false`, no `node_addr`
- [x] Invalid key format → 400
- [x] Online peer (recent heartbeat) → `online: true` + `node_addr` present
- [x] Offline peer (heartbeat >600 s ago) → `online: false`, no `node_addr`
- [x] `last_seen_at` is Unix integer, not ISO string
- [x] Response contains no `profile`, `collections`, or `succession` fields

**Tests required:**
Unit: `get_peer_unknown_key`, `get_peer_invalid_format`, `get_peer_online`, `get_peer_offline_after_600s`, `response_has_no_profile_fields`
Integration: `heartbeat_then_get_peer_shows_online`

**Verification steps:**
1. `cargo test -p hb-relay -- handlers::get_peer`
2. `curl relay/v1/peer/:known_key | jq 'has("profile")'` → `false`

**Definition of done:** All acceptance criteria checked; response schema confirmed profile-free.

---

### TASK 10 [x]: GET /v1/messages/:pubkey

**Depends on:** T6  **Parallel with:** T7, T8, T9, T11

**Scope:** Validate hb_id format; return up to 100 most-recent non-expired messages addressed to `pubkey`, oldest first. Spec §Relay API — "GET /v1/messages/:pubkey".

**Acceptance criteria:**
- [x] Invalid key format → 400
- [x] Returns messages chronologically (oldest first)
- [x] Returns at most 100 messages even if 150 exist
- [x] Expired messages not returned

**Tests required:**
Unit: `get_messages_invalid_key`, `get_messages_chronological`, `get_messages_cap_100`, `get_messages_excludes_expired`

**Verification steps:**
1. `cargo test -p hb-relay -- handlers::get_messages`

**Definition of done:** All acceptance criteria checked.

---

### TASK 11 [x]: GET /v1/health, Relay Peering, TTL Expiry, and Mailbox Cap

**Depends on:** T6  **Parallel with:** T7, T8, T9, T10

**Scope:** Three concerns bundled as one task since they share only the DB layer:

**(A) Health endpoint:** Returns `{"ok":true,"stored_peers":<heartbeat count>,"peers":[<relay URLs>]}`. `peers` seeded from `PEER_RELAYS` env var. `stored_peers` is the count of distinct heartbeat rows (i.e., peers the relay has ever seen heartbeat from).

**(B) Relay peering (client-side, built in T27):** On startup, app queries health of all known relays and accumulates advertised peer URLs (depth ≤ 1 trust; cap at 20). Relays failing 3 consecutive health checks deprioritised.

**(C) TTL expiry:** Hourly Tokio background task calls `db::expire_messages`. Message TTL: 30 days. Heartbeat rows never expire.

**(D) Mailbox cap:** 500 messages per recipient. The cap is enforced in T7 via `count_messages_for`. Exceeding it → 400 with "mailbox" in body.

**Acceptance criteria:**
- [x] `GET /v1/health` returns valid JSON with `ok: true`, `stored_peers`, and `peers`
- [x] `peers` populated from `PEER_RELAYS` env var
- [x] Message from 31 days ago absent after expiry task runs
- [x] Heartbeat rows survive expiry
- [x] 501st message to recipient → 400

**Tests required:**
Unit: `health_response_format`, `expire_old_messages`, `heartbeat_survives_expiry`, `mailbox_cap_constant_is_500`
Integration: `relay_peering_depth_one_accepted`, `relay_peering_depth_two_rejected`

**Verification steps:**
1. `cargo test -p hb-relay -- health expiry`
2. `PEER_RELAYS=https://relay2.example.com cargo run -p hb-relay` → `curl /v1/health | jq .peers`

**Definition of done:** All acceptance criteria checked; relay peering depth-limited trust enforced.

---

### CHECKPOINT 1 [x]: Relay Binary Complete

**Gate condition:** Relay binary serves all five endpoints (heartbeat, get_peer, publish, get_messages, health) with correct behaviour. Profile and collection storage does not exist. Relay is a pure bootstrap + DM relay.

**Human review items:**
- `curl relay/v1/peer/:pubkey | jq 'keys'` — must show only `["last_seen_at","node_addr","online"]`, no `profile` or `collections`
- POST a profile-type envelope → confirm 400
- POST a valid message envelope → 200; GET messages → message present
- Confirm `documents` and `collections` tables do not exist in the SQLite file

**Automated gate:** `cargo test -p hb-relay && cargo test -p hb-core`

**Rollback plan:** Tag before Phase 1. SQLite file can be deleted and re-migrated safely; it holds no user data at this stage.

---

## PHASE 2 — IDENTITY + ONBOARDING

---

### TASK 12 [x]: Identity Generation and Local Encrypted Storage

**Depends on:** T4, T5  **Parallel with:** none in this phase

**Scope:** Tauri commands: `identity_generate_keypair() → Result<String>` (returns hb_id), `identity_export_backup(path: PathBuf) → Result<()>`, `identity_import(path: PathBuf) → Result<String>`, `identity_get() → Result<Option<String>>`.

**Key storage:**
- **Windows:** DPAPI-encrypted binary file at `~/.hoardbook/identity/keypair.bin`. Call `CryptProtectData` (via the `dpapi` crate or direct Windows API binding) to encrypt `StoredKeypair` JSON bytes before writing. Call `CryptUnprotectData` to decrypt on load. No Credential Manager involvement.
- **Linux:** Plain JSON at `~/.hoardbook/identity/keypair.json` with `chmod 600`. A one-time warning is shown on first write: *"Your key is stored as a file. Keep your home directory secure."*

**Backup export:** Writes `StoredKeypair { version: 1, hb_id, private_key_hex }` as plain JSON to a user-chosen path. This file is portable (not DPAPI-encrypted) so it can be imported on any machine.

**Import:** Reads plain JSON backup, validates hb_id format, re-encrypts with DPAPI (Windows) or writes with `chmod 600` (Linux), replaces local key file.

**`~/.hoardbook/` directory** created with mode `700`.

**Acceptance criteria:**
- [x] On Windows, private key is stored as a DPAPI-encrypted file, not plaintext
- [x] On Linux, key file has permissions `600`; warning shown on first generate
- [x] `generate()` called twice produces different hb_ids
- [x] Export + import roundtrip returns same hb_id
- [x] After import, all subsequent signing uses the imported key
- [x] `StoredKeypair` `Debug` output shows `[REDACTED]` for `private_key_hex`
- [x] No Credential Manager entries created at any point

**Tests required:**
Unit: `keypair_generate_unique`, `export_import_roundtrip`, `stored_keypair_debug_redacts`
Integration: `dpapi_encrypt_decrypt_roundtrip` (Windows only; skipped on Linux)
E2E: generate key → export backup → delete key file → reimport → app functions normally

**Verification steps:**
1. `cargo test -p hb-app -- identity`
2. Windows: generate key → check `~/.hoardbook/identity/keypair.bin` is not plain JSON
3. Linux: check file permissions are `600`

**Definition of done:** All acceptance criteria checked; no Credential Manager involvement on Windows.

---

### TASK 13 [x]: Onboarding Wizard — 3-Step UI

**Depends on:** T12  **Parallel with:** none

**Scope:** SvelteKit route `/onboarding`. Shown only on first run (no key file present). **Step 1:** Generate button → calls `identity_generate_keypair`, displays hb_id, offers backup export ("Export backup file" / "I'll do it later"). Qurator import option with privacy overlay. **Step 2:** Profile fields (display_name, bio, tags, est_size, since, contact_hint, willing_to) — all optional; Skip advances without saving. **Step 3:** Add a collection (path, name, depth) — optional; Skip or Done → main app. No key rotation prompt anywhere in the wizard. Spec §Onboarding.

**Acceptance criteria:**
- [x] Wizard shown on first launch; absent on all subsequent launches
- [x] Generate button disabled during key generation; spinner shown
- [x] hb_id displayed and copyable after generation
- [x] Step 2 Skip writes nothing
- [x] No key rotation or succession mentioned anywhere in the wizard
- [x] Qurator import shows privacy overlay before proceeding

**Tests required:**
Unit: `wizard_not_shown_if_key_exists`, `step2_skip_writes_nothing`
Integration: `step1_generates_and_advances`
E2E: fresh install → complete wizard → relaunch → wizard absent

**Verification steps:**
1. `npm run check` in `crates/hb-app/ui`
2. Delete `~/.hoardbook/`, launch dev app — wizard appears; complete; relaunch — wizard absent

**Definition of done:** All acceptance criteria checked; tested on Windows and Linux.

---

### TASK 14 [x]: Profile Editor — Sign Locally, No Relay Publish

**Depends on:** T12  **Parallel with:** T15

**Scope:** Tauri commands: `profile_get() → Result<Option<Profile>>`, `profile_save(profile: Profile) → Result<()>`. Profile is signed locally and written to `~/.hoardbook/identity/profile.signed.json`. The signed file is what the iroh node server (T17) will serve to connecting peers. There is **no relay publish** — profile data lives on the node, not the relay. The "Publish" button in the UI becomes "Save and Activate" — it signs the profile and makes it live for incoming peer connections. Unpublish means deleting the signed file; the node will return "no profile" to connecting peers. UI: form with all spec profile fields + approved extensions (email, location, social_links). `content_types` absent from form (computed). Live preview. Spec §Profile Editor.

**Acceptance criteria:**
- [x] Profile saved as `profile.signed.json` locally
- [x] No HTTP call to any relay on profile save
- [x] `content_types` in the signed file is computed as the union of all collection content_types; not user-editable
- [x] Live preview matches what a visitor sees when they connect via iroh
- [x] "Unpublish" deletes the signed file; iroh server returns empty profile to peers

**Tests required:**
Unit: `profile_signed_with_correct_key`, `content_types_auto_computed`, `no_relay_call_on_save`
Integration: `save_profile_then_iroh_serve` (covered by T17 integration tests)
E2E: save profile → connect from second instance via iroh → profile data matches

**Verification steps:**
1. `cargo test -p hb-app -- profile`
2. Save profile; inspect `~/.hoardbook/identity/profile.signed.json` — valid signed envelope

**Definition of done:** All acceptance criteria checked; no relay interaction on profile save.

---

### CHECKPOINT 2 [ ]: Identity + Profile Operational

**Gate condition:** User generates a keypair (DPAPI-encrypted on Windows), completes or skips the wizard, and has a locally-signed profile ready to serve.

**Human review items:**
- Windows: generate key, verify `~/.hoardbook/identity/keypair.bin` is a binary (not plain JSON)
- Export backup → delete key file → reimport → profile still loadable
- Walk all three wizard paths: complete, skip step 2, skip step 3
- Save profile → `cat ~/.hoardbook/identity/profile.signed.json` → valid signed envelope

**Automated gate:** `cargo test -p hb-app && cargo test -p hb-core`

---

## PHASE 3 — COLLECTION FLOW

---

### TASK 15 [ ]: Collection Add, Scan, and Draft

**Depends on:** T12, T5  **Parallel with:** T14

**Scope:** Tauri command `collection_add(local_path, path_alias, description?, depth, exclude_patterns, content_types, tags) → Result<String>`. Walk filesystem up to `depth`, apply glob exclusions, build `Vec<DirectoryItem>` (no `sha256` field). Write draft to `~/.hoardbook/collections/<slug>.draft.json`. At least one content type required to enable publish — enforced at the command layer. Per-item notes: `DirectoryItem.note`, editable post-scan. Preview mode renders draft as visitors see it. `collection_regenerate_snapshot` re-scans and preserves per-item notes. Spec §Collection Manager.

**Acceptance criteria:**
- [ ] 1,000-file directory scans in under 5 seconds
- [ ] Exclude patterns remove matching items
- [ ] Depth limit respected
- [ ] At least one content type required before publish is enabled
- [ ] Per-item notes survive regenerate
- [ ] No `sha256` field in any `DirectoryItem` in the draft JSON

**Tests required:**
Unit: `depth_limit_enforced`, `exclude_glob_applied`, `item_count_accurate`, `regenerate_preserves_notes`, `no_sha256_in_draft`
Integration: `scan_writes_valid_draft`
E2E: add collection → preview → edit per-item note → regenerate → note present

**Verification steps:**
1. `cargo test -p hb-app -- collection_add`
2. `grep sha256 ~/.hoardbook/collections/*.draft.json` — zero results

**Definition of done:** All acceptance criteria checked; no sha256 in any output.

---

### TASK 16 [ ]: Collection Sign — Local Only

**Depends on:** T15, T14  **Parallel with:** none in this phase

**Scope:** Tauri command `collection_publish(slug: String) → Result<()>`. Loads draft, creates `SignedEnvelope` (doc_type=Collection), writes to `<slug>.signed.json`, calls `profile_save` to recompute `content_types`. **No relay push.** The signed file is served by the iroh node (T17). Publishing is always manual. Spec §Collection Manager — "Snapshot trigger".

**Acceptance criteria:**
- [ ] Signed envelope written to `~/.hoardbook/collections/<slug>.signed.json`
- [ ] No HTTP call to any relay on publish
- [ ] Profile `content_types` recomputed and profile re-signed after collection publish
- [ ] `envelope.verify()` returns `Ok(())` on the output file

**Tests required:**
Unit: `publish_signs_with_current_key`, `no_relay_call_on_publish`, `profile_content_types_updated`
Integration: `sign_then_iroh_serve` (covered by T17)
E2E: publish collection → connect from second instance via iroh → collection present

**Verification steps:**
1. `cargo test -p hb-app -- collection_publish`
2. `cargo run -p hb-app` (dev) → publish → verify signed.json on disk

**Definition of done:** All acceptance criteria checked; no relay interaction.

---

### CHECKPOINT 3 [ ]: Collection Flow Complete

**Gate condition:** User can add, configure, preview, and publish collections locally. Signed files on disk are verifiable. Profile content_types updated on collection publish.

**Human review items:**
- Add collection with depth=2 and `*.nfo` exclusion; verify preview
- Publish; `cat ~/.hoardbook/collections/<slug>.signed.json | jq .doc_type` → `"collection"`
- `grep sha256 ~/.hoardbook/collections/*.signed.json` → zero results

**Automated gate:** `cargo test --workspace`

---

## PHASE 4 — IROH NODE + BACKGROUND SERVICE

---

### TASK 17 [ ]: iroh Node Server — Serve Profile + Collections, Accept Direct DMs

**Depends on:** T12, T16  **Parallel with:** T18, T19

**Scope:** Start an iroh QUIC endpoint on app launch. Define a simple request/response wire protocol over iroh byte streams:

```
Request:  { "type": "get_profile" }
Response: { "profile": <signed envelope>|null, "collections": [<signed envelope>, ...] }

Request:  { "type": "send_dm", "envelope": <signed envelope> }
Response: { "ok": true } | { "ok": false, "error": "..." }
```

The server reads signed files from `~/.hoardbook/identity/profile.signed.json` and `~/.hoardbook/collections/*.signed.json` to respond to `get_profile` requests. For incoming `send_dm` requests: verify the envelope signature, validate the recipient matches own hb_id, queue the message for the inbox (in-memory queue, drained by `dm_fetch_inbox`). The iroh NodeAddr is stored in app state so the heartbeat task (T18) can include it.

**Acceptance criteria:**
- [ ] iroh endpoint starts on app launch and binds a local port
- [ ] `get_profile` request returns the current signed profile + all signed collections
- [ ] `get_profile` returns empty response when no profile is published
- [ ] `send_dm` request: verifies signature, queues message if recipient matches own key
- [ ] `send_dm` request: rejects if recipient hb_id does not match own key
- [ ] iroh NodeAddr is accessible to the heartbeat task for inclusion in heartbeat payload
- [ ] Server handles concurrent connections without blocking

**Tests required:**
Unit: `get_profile_returns_signed_files`, `send_dm_validates_recipient`, `send_dm_wrong_recipient_rejected`
Integration: `iroh_client_connects_and_fetches_profile` — spin up two iroh endpoints in the same test, assert profile data matches
E2E: two running app instances on same machine; instance A fetches instance B's profile via iroh directly

**Verification steps:**
1. `cargo test -p hb-app -- iroh_server`
2. Launch app; check logs for "iroh endpoint started on …"

**Definition of done:** All acceptance criteria checked; profile fetch and DM receive verified over iroh.

---

### TASK 18 [ ]: Heartbeat Background Task — Carries NodeAddr

**Depends on:** T12, T8  **Parallel with:** T17, T19

**Scope:** Tokio task spawned on Tauri setup hook. Every 5 minutes: build `HeartbeatBody { public_key, signed_at, node_addr: Some(iroh_node_addr) }`, sign, POST to all known relays. First heartbeat within 30 seconds of launch. Relay failure: log, continue. Shutdown: `CancellationToken`. The `node_addr` is the iroh NodeAddr from T17, base64-encoded. This is what allows peers to find and connect directly. Spec §Resolved Design Decisions — "Online status detection".

**Acceptance criteria:**
- [ ] First heartbeat within 30 seconds of launch
- [ ] Heartbeat includes `node_addr` (from T17 iroh endpoint)
- [ ] Relay failure does not stop the task
- [ ] After 11 minutes of running, relay returns `online: true`
- [ ] App shutdown cancels task cleanly

**Tests required:**
Unit: `heartbeat_includes_node_addr`, `heartbeat_signed_correctly`, `task_survives_relay_failure`
Integration: `heartbeat_sets_online_with_node_addr` — POST heartbeat with node_addr; GET /v1/peer; assert `online: true` and `node_addr` present

**Verification steps:**
1. `cargo test -p hb-app -- heartbeat`
2. Run app 6 min; `curl relay/v1/peer/:pubkey | jq .node_addr` — non-null

**Definition of done:** All acceptance criteria checked; NodeAddr present in relay heartbeat store.

---

### TASK 19 [ ]: System Tray + Background Service

**Depends on:** T12  **Parallel with:** T17, T18

**Scope:** Configure Tauri so that pressing the window close button hides the window rather than terminating the process. A system tray icon persists. Right-click tray menu: "Open Hoardbook" (shows window), "Quit" (terminates process and all background tasks). All background tasks (iroh server, heartbeat, DHT announce) keep running when the window is hidden. The tray icon shows a visual indicator when unread DMs are present (badge or icon change). On Windows: use the native system tray API via Tauri's tray support. On Linux: same via the system tray spec (most desktop environments).

**Acceptance criteria:**
- [ ] Pressing window X hides the window; process does not terminate
- [ ] System tray icon visible after window is hidden
- [ ] "Open Hoardbook" from tray shows the window
- [ ] "Quit" from tray terminates the process (all tasks stopped cleanly)
- [ ] iroh endpoint, heartbeat task, and DHT announce continue while window is hidden
- [ ] Tray icon shows unread DM indicator when new DMs are queued

**Tests required:**
Unit: `tray_quit_stops_all_tasks`
E2E (manual): close window → verify process still running (`tasklist` / `ps`); open from tray → window appears; quit from tray → process gone

**Verification steps:**
1. Close window; `tasklist | grep hb-app` (Windows) or `pgrep hb-app` (Linux) — process present
2. Quit from tray; repeat check — process absent

**Definition of done:** All acceptance criteria checked; background service verified on Windows and Linux.

---

### CHECKPOINT 4 [ ]: iroh Node + Background Service Operational

**Gate condition:** Two Hoardbook instances can exchange profile and collection data directly via iroh without any relay involvement. The app persists as a background service after the window is closed.

**Human review items:**
- Instance A publishes profile + collection; instance B fetches via iroh (no relay); data matches
- Close instance A's window; verify process still running; open from tray; quit from tray
- Check relay during iroh fetch — no profile/collection traffic to relay
- `curl relay/v1/peer/:pubkey | jq .node_addr` — non-null after heartbeat

**Automated gate:** `cargo test --workspace`

---

## PHASE 5 — BROWSE + FOLLOW

---

### TASK 20 [ ]: Browse — iroh Direct Connection, Local Cache Fallback

**Depends on:** T17, T18, T10  **Parallel with:** T21

**Scope:** Tauri command `browse_fetch_peer(hb_id: String) → Result<PeerData, BrowseError>`. Flow:
1. Validate hb_id (reject immediately with error before any network call if invalid)
2. Query relay `GET /v1/peer/:pubkey` → get `online` status and `node_addr`
3. If `online: true` and `node_addr` present: connect via iroh → send `get_profile` request → receive signed envelopes → verify each signature → return data
4. If `online: false` or iroh connection fails: load from local contact cache (`~/.hoardbook/contacts/<hash>.json`) if present — return with `stale: true` flag
5. If offline and no local cache: return `BrowseError::PeerOffline`

UI: skeleton card + "Connecting…" indicator; on success — profile card (all fields; absent optionals not shown); on stale cache — profile card with "Offline — last seen X days ago" badge; on no cache + offline — offline error card ("Save key anyway" / "Retry" / "Dismiss"). Two-pane directory viewer below the card. Search within listing. Export button (T26). Spec §Browse & Follow, §Profile Card View, §Directory Listing View.

**Acceptance criteria:**
- [ ] Invalid hb_id checksum rejected before any network call
- [ ] Online peer: data fetched via iroh; no relay involved for profile/collection data
- [ ] Offline peer with local cache: stale profile card shown with offline badge
- [ ] Offline peer with no cache: offline error card shown with all three buttons
- [ ] "Save key anyway" adds peer to contact list for future retry
- [ ] Tampered signed envelope (invalid signature) silently discarded; next relay tried / error shown
- [ ] Two-pane directory viewer: tree pane (lazy expansion) + contents pane (selected folder's children)
- [ ] Breadcrumb navigable; per-item notes shown as tooltips

**Tests required:**
Unit: `invalid_hb_id_no_network_call`, `offline_with_cache_shows_stale`, `offline_no_cache_shows_error`, `tampered_envelope_discarded`
Integration: `fetch_peer_via_iroh_matches_published_data`
E2E: paste online peer's hb_id → profile card via iroh; stop peer → paste again → stale cache shown

**Verification steps:**
1. `cargo test -p hb-app -- browse`
2. Two instances: B online, A fetches B → check no relay profile traffic; B quits, A fetches B again → stale badge

**Definition of done:** All acceptance criteria checked; iroh-first fetch confirmed in network logs.

---

### TASK 21 [ ]: Follow, Contact List, and Contact Groups

**Depends on:** T20  **Parallel with:** none in this phase

**Scope:** Tauri commands: `contact_follow(hb_id, initial_group_id?)`, `contact_list()`, `contact_refresh(hb_id)` (re-fetches via iroh or local cache), `contact_update_groups(hb_id, group_ids)`, `group_create(name)`, `group_rename(id, name)`, `group_delete(id)`. Storage: contacts at `~/.hoardbook/contacts/<sha256(pubkey)[0..16]>.json` (full cached profile + collections); groups at `~/.hoardbook/groups.json`. The contact cache is the local copy used for stale display in T20. Multi-group membership. Group delete → Ungrouped. Groups ordered by `modified_at` descending. UI: collapsible group sections, drag-and-drop reassignment, right-click "Add to group". Group picker on follow. Status badges: Online / Stale (snapshot >7 days) / Offline. Content type badges and profile tags on contact cards — read-only. Groups never sent to relay. Spec §Browse & Follow, §Contact Groups.

**Acceptance criteria:**
- [ ] Follow shows group picker; skip → Ungrouped
- [ ] Contact can belong to multiple groups
- [ ] Deleting a group moves contacts to Ungrouped; does not remove from contact list
- [ ] Groups ordered by most-recently-modified
- [ ] Drag to Ungrouped removes all group memberships
- [ ] `contact_refresh` updates the local cache file (used for offline stale display)
- [ ] Groups never transmitted to relay at any point

**Tests required:**
Unit: `follow_skip_ungrouped`, `multi_group_membership`, `delete_group_moves_to_ungrouped`, `stale_after_7_days`, `groups_not_in_relay_traffic`
Integration: `contact_refresh_updates_cache`
E2E: follow peers, assign groups, restart → contacts and groups persist

**Verification steps:**
1. `cargo test -p hb-app -- contacts groups`
2. Follow a peer; check `~/.hoardbook/groups.json`; stop relay entirely; reload app — contacts still show from local cache

**Definition of done:** All acceptance criteria checked; relay independence of contact data verified.

---

### CHECKPOINT 5 [ ]: Browse + Follow Operational

**Gate condition:** User can paste any Hoardbook ID, see a profile card fetched via direct iroh connection, follow with group assignment, and see stale local cache when the peer's node is offline.

**Human review items:**
- Fetch an online peer → verify no profile data goes through relay (network capture)
- Peer goes offline → fetch again → stale badge with last-seen time, profile data still shown
- Groups persist across restart; groups absent from all relay responses

**Automated gate:** `cargo test --workspace`

---

## PHASE 6 — DHT DISCOVERY

---

### TASK 22 [ ]: DHT Announce and Tag Search (Mainline DHT, BEP 5)

**Depends on:** T12, T11  **Parallel with:** T24

**Scope:** Integrate a mainline DHT (BEP 5) client (`mainline` crate). **(A) Announce:** for each opted-in tag/content_type, compute `SHA-1(tag_string)` as DHT key; announce with payload `(hb_id, relay_urls)` encoded as bencode. Refresh every 30 minutes while enabled. Stop immediately when disabled. **(B) Search:** `dht_search(tags, content_types) → Vec<PeerData>`. `get_peers` for each term in parallel; collect `(pubkey, relay_url)` pairs; deduplicate; query relay `GET /v1/peer/:pubkey` for NodeAddr; fetch profile via iroh; verify signatures; discard invalid results. AND logic for tags; OR logic for content types. At least one filter required. Spec §DHT Discovery.

**Acceptance criteria:**
- [ ] Announce enabled → discoverable by searching announced tags from another instance
- [ ] Announce disabled → zero BEP 5 traffic
- [ ] Invalid signature results silently discarded
- [ ] Tag AND logic; content type OR logic
- [ ] Empty filter rejected before DHT call
- [ ] Only explicitly opted-in tags announced

**Tests required:**
Unit: `sha1_tag_key`, `invalid_sig_discarded`, `and_logic_intersection`, `empty_filter_rejected`
Integration: `announce_and_find` (live DHT or mock)
E2E: enable announce with unique tag; search from second instance → own profile appears

**Verification steps:**
1. `cargo test -p hb-app -- dht`
2. Enable announce; Wireshark capture → `announce_peer` packets present; disable → traffic stops

**Definition of done:** All acceptance criteria checked; announce/search verified on live DHT.

---

### TASK 23 [ ]: Saved Tag Watches

**Depends on:** T22  **Parallel with:** none

**Scope:** Locally stored tag queries that fire in-app notifications when new matching peers are discovered via DHT search. Storage: `~/.hoardbook/watches.json` as `Vec<Watch>` with `{ id, name, tags, content_types, last_fired?, dismissed_keys[] }`. Commands: `watches_list()`, `watches_create(name, tags, content_types)`, `watches_delete(id)`. Evaluation: after each DHT search, compare results against contacts + dismissed sets per watch. Fire = Tauri frontend event `watch_fired { watch_name, count }`. Never transmitted to relay. Spec §Saved Tag Watches.

**Acceptance criteria:**
- [ ] Fires only for peers not in contact list and not previously dismissed from this watch
- [ ] Each watch fires independently
- [ ] Dismissing from one watch does not affect another
- [ ] Watches persist across restarts

**Tests required:**
Unit: `watch_fires_new_peer`, `watch_silent_known_contact`, `watch_silent_dismissed`, `watch_persists`
Integration: `watches_evaluated_after_dht_search`
E2E: create watch; DHT search matches; notification appears; dismiss; re-search; no repeat

**Verification steps:**
1. `cargo test -p hb-app -- watches`

**Definition of done:** All acceptance criteria checked.

---

### CHECKPOINT 6 [ ]: DHT Discovery Complete

**Gate condition:** DHT announce and search work on live mainline DHT. Watches fire correctly.

**Human review items:**
- Two-instance test: announce unique tag; search from second instance; discover peer
- Disable announce; verify no DHT traffic (Wireshark)
- Watch fire, dismiss, re-search — no repeat notification

**Automated gate:** `cargo test -p hb-app`

---

## PHASE 7 — DIRECT MESSAGES

---

### TASK 24 [ ]: DM Compose and Send — Direct iroh First, Relay Fallback

**Depends on:** T12, T17  **Parallel with:** T25

**Scope:** Tauri command `dm_send(to: HbId, content: String) → Result<()>`. Encryption: existing implementation — static X25519 derived from Ed25519 identity key, XChaCha20-Poly1305, wire format `base64(nonce[24] || ciphertext)`. Send flow:
1. Query relay `GET /v1/peer/:to` → check if recipient is online + has NodeAddr
2. If online: establish iroh connection → send `send_dm` request with signed envelope → await `{"ok":true}` response
3. If offline or iroh delivery fails: fall back to `POST /v1/publish` on relay (store-and-forward)
4. If relay also fails: return error to UI

UI: compose view — recipient hb_id input (or pick from contacts) and message textarea. Content limit: 4096 bytes before encryption.

**Acceptance criteria:**
- [ ] Online recipient: DM delivered via iroh (no relay involvement)
- [ ] Offline recipient: DM stored on relay via POST /v1/publish
- [ ] iroh delivery failure falls back to relay automatically (transparent to user)
- [ ] Content > 4096 bytes rejected before encryption
- [ ] `ChatMessage.encrypted = true` always set
- [ ] Relay-stored ciphertext is not readable as plaintext

**Tests required:**
Unit: `send_dm_online_uses_iroh`, `send_dm_offline_uses_relay`, `iroh_failure_falls_back_to_relay`, `max_content_enforced`
Integration: `dm_roundtrip_via_iroh` — two iroh endpoints; A sends to B directly; B receives via iroh server queue
Integration: `dm_roundtrip_via_relay` — B offline; A sends via relay; B comes online and fetches
E2E: A sends to online B — verify via iroh (no relay message traffic); A sends to offline B — verify relay stores it

**Verification steps:**
1. `cargo test -p hb-app -- dm_send`
2. Send to online peer; inspect relay — no new message row; relay DB messages table unchanged

**Definition of done:** All acceptance criteria checked; direct path verified in network logs.

---

### TASK 25 [ ]: DM Inbox — Receive Direct + Poll Relay

**Depends on:** T24, T10  **Parallel with:** none

**Scope:** Two inbound paths. **(A) Direct:** The iroh node server (T17) accepts `send_dm` requests and queues them in an in-memory channel. The UI subscribes to a Tauri event `dm_received` emitted when a message is queued. **(B) Relay poll:** `dm_fetch_inbox()` command queries `GET /v1/messages/:own_pubkey` from all known relays on app launch and on manual refresh. Deduplicates by `(from_key, sent_at)` across both sources and across relays. Decrypts using existing `decrypt_from`. Decryption failures → `"[Unable to decrypt]"` placeholder. No local persistence — inbox is always live (in-memory direct queue + relay fetch). UI: inbox grouped by sender (display name if in contacts, else hb_id), newest sender first, messages within sender chronological. No threads, no read receipts.

**Acceptance criteria:**
- [ ] Direct DMs appear in inbox immediately (real-time, no refresh needed)
- [ ] Relay DMs appear on launch and manual refresh
- [ ] Deduplication: same `(from_key, sent_at)` appears once regardless of how many sources
- [ ] Decryption failure shows placeholder without crashing
- [ ] Inbox grouped by sender; within group, chronological order
- [ ] Known contact shows display name

**Tests required:**
Unit: `dedup_across_sources`, `decryption_failure_placeholder`, `known_contact_display_name`
Integration: `direct_dm_appears_in_inbox` — send via iroh server; assert Tauri event emitted
Integration: `relay_dm_fetched_on_launch`
E2E: A sends direct DM to online B; appears immediately in B's inbox without refresh

**Verification steps:**
1. `cargo test -p hb-app -- dm_inbox`
2. Send DM while recipient is running — verify it appears without manual refresh

**Definition of done:** All acceptance criteria checked; real-time direct delivery and relay fallback both verified.

---

### CHECKPOINT 7 [ ]: DMs Operational

**Gate condition:** Direct iroh DM delivery works between two online instances. Relay fallback works for offline recipients. Inbox deduplicates across sources.

**Human review items:**
- A sends to online B; inspect relay DB — no new message row (direct path used)
- A sends to offline B; inspect relay DB — message row present; B comes online; inbox shows it after launch
- Inspect relay DB `messages.envelope` during relay-path send — content is base64 ciphertext, not plaintext

**Automated gate:** `cargo test --workspace`

---

## PHASE 8 — POLISH

---

### TASK 26 [ ]: Export Collection Listing

**Depends on:** T16  **Parallel with:** T27

**Scope:** `collection_export_listing(slug, format: PlainText | MarkdownChecklist) → Result<String>`. Plain text: 4-space indent per depth level. Markdown: `- [ ] Item name [Format, Size]`; folders as `- [ ] 📁 Folder name`. Renders from signed JSON, not live filesystem. "Copy to clipboard" or "Save to file". Spec §Directory Listing View.

**Acceptance criteria:**
- [ ] Plain text: root items at 0 indent; each depth level adds 4 spaces
- [ ] Markdown: `- [ ] Seven Samurai (1954) [MKV, 14.2GB]`; format/size omitted if absent
- [ ] Uses signed data, not re-scanned filesystem
- [ ] 5,000-item collection exports in under 2 seconds

**Tests required:**
Unit: `plain_text_indentation`, `markdown_with_metadata`, `markdown_missing_metadata`, `uses_signed_not_live`

**Verification steps:**
1. `cargo test -p hb-app -- export`
2. Export real collection in both formats; paste markdown into GitHub comment preview

**Definition of done:** Both formats verified in real markdown renderers.

---

### TASK 27 [ ]: Settings Page

**Depends on:** T12, T11, T22, T23  **Parallel with:** T26

**Scope:** SvelteKit route `/settings`. Sections: **(1) Connection:** Relay-only / Allow direct connections. First-enable shows one-time warning (spec text, §Privacy Model). Direct mode enables iroh direct connections for browsing (already default for serving via T17 — this toggle controls whether the client also makes outgoing direct connections to peers who are offline-relay-only). **(2) Publish toggle** — off means iroh server returns empty profile; heartbeat continues. **(3) DHT Announce** — toggle + tag/content-type selector. **(4) DM delivery** — informational: "Direct when online, relay when offline." **(5) Relay Preferences** — managed relay list; add custom HTTPS relay URL (validated). **(6) Snapshot schedule** — informational: "Manual only." **(7) Watches** — list, add, delete. **(8) Key management** — view hb_id (copy button), export backup. **No key rotation section.** Settings persisted in `~/.hoardbook/settings.json`.

**Acceptance criteria:**
- [ ] Direct connection warning shown exactly once
- [ ] Publish toggle off: iroh server returns empty; heartbeat continues
- [ ] DHT announce toggle off: zero BEP 5 traffic immediately
- [ ] Custom relay URL validated as HTTPS before saving
- [ ] No key rotation or succession UI anywhere in settings
- [ ] All settings persist across restarts

**Tests required:**
Unit: `direct_mode_warning_once`, `relay_url_https_required`, `publish_toggle_stops_iroh_serve`, `no_rotation_ui_present`
Integration: `settings_persist_across_restart`

**Verification steps:**
1. `cargo test -p hb-app -- settings`
2. Grep UI files for "rotation" and "succession" — zero results

**Definition of done:** All acceptance criteria checked; key rotation absent from all UI surfaces.

---

### CHECKPOINT 8 (FINAL) [ ]: Phase 1 MVP Complete

**Gate condition:** All 27 tasks complete. The following five user journeys execute end-to-end on Windows (primary) and Linux (secondary):

1. Fresh install → generate key → onboarding → publish profile + collection (locally signed) → share hb_id
2. Second user pastes hb_id → fetches profile via iroh directly → follows → assigns to group
3. User A sends DM to online User B → arrives immediately via iroh (no relay)
4. User A sends DM to offline User B → stored on relay → B comes online → DM in inbox
5. DHT announce tag → third user searches → discovers peer → connects via iroh

**Human review items:**
- All five journeys on Windows with two local instances
- Journeys 1–2 on Linux
- Relay DB inspection: no `documents` or `collections` tables exist
- Network capture: profile/collection fetch produces zero relay traffic (iroh only)
- `grep -r "rotation\|succession\|Succession" crates/hb-app/src crates/hb-app/ui` → zero results
- `grep sha256 ~/.hoardbook/collections/*.signed.json` → zero results
- Relay Docker image documented in `crates/hb-relay/README.md`

**Automated gate:**
- `cargo test --workspace` — all tests green
- `cargo clippy --workspace -- -D warnings` — zero warnings
- `npm run check` in `crates/hb-app/ui` — zero TypeScript errors

**Rollback plan:** Tag `v0.1.0-rc1` before Phase 8. Any Phase 8 failure reverts to Phase 7 tag; DMs and DHT features unaffected.

---

## PLAN SUMMARY FOR HUMAN REVIEW

**Total tasks:** 27

---

**Critical path:**

```
T1 → T2 → T3 → T4 → T5 → T6 → T12 → T14 → T15 → T16 → T17 → T20 → T21 → T24 → T25
```

15 tasks. All other tasks parallelise against this chain.

---

**Highest-risk tasks:**

1. **T17 — iroh Node Server:** This is the most architecturally novel component. Hoardbook now acts as its own server, requiring a custom request/response protocol over iroh QUIC streams, concurrent connection handling, and graceful shutdown. iroh's API surface for custom protocols is relatively new and documentation is thin. Get this right before building browse (T20) and DMs (T24), both of which depend on it.

2. **T22 — DHT Announce + Search (BEP 5):** The BEP 5 `announce_peer` payload format for carrying `(hb_id, relay_urls)` is non-standard. This format must be finalled before Phase 6 ships — it cannot be changed later without breaking cross-version DHT interoperability.

3. **T19 — System Tray + Background Service:** Platform-specific behaviour that is hard to test automatically. Windows tray integration via Tauri works but has edge cases (multiple monitors, taskbar icon behaviour, notification area overflow). Linux tray support varies by desktop environment (GNOME, KDE, XFCE all behave differently). Requires manual verification on multiple configurations.

---

**What changed from v1.0 of this plan:**

| Item | v1.0 | v2.0 |
|---|---|---|
| Relay profile/collection caching | Core feature | Removed |
| Profile/collection publish | Push to relay | Sign locally; serve via iroh |
| Browse data source | Relay HTTP fetch | iroh direct; local cache fallback |
| Key rotation + succession | Task T24 | Removed entirely |
| Private key storage | Credential Manager | DPAPI-encrypted file (Windows) / `chmod 600` file (Linux) |
| DM delivery | Relay store-and-forward only | iroh direct when online; relay fallback |
| App lifecycle | Window close = quit | Window close = minimize to tray |
| New tasks | — | T17 (iroh server), T18 (heartbeat with NodeAddr), T19 (system tray), T24/T25 (DM dual-path) |
| Task count | 26 | 27 |

---

**Features explicitly deferred (not in Phase 1 per spec):**

- Qurator integration panel (Phase 2)
- Tag autocomplete (Phase 2)
- macOS support (Phase 2)
- MessagePack for large collections (Phase 2 if needed)
- Community relay network expansion (Phase 3)
- Browser extension (Phase 3)
- CLI interface (Phase 3)
- Static HTML collection export (Phase 3)
- File transfer (explicitly out of scope per spec §What Hoardbook Is NOT)

**No open questions remain.** All decisions have been made.
