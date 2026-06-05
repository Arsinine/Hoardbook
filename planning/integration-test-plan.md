# Hoardbook Integration Test Plan
**Topology:** SG VPS = `relay-sg` (relay-1) · JP VPS = `relay-jp` (relay-2)

Unit tests cover logic in isolation. This plan targets the seams unit tests can't reach: real network latency, real clock skew, concurrent database writes, relay peering over the internet, iroh NAT traversal between geographic regions, and cascading failure modes.

---

## 1. Test Environment Setup

### 1.1 VPS Roles

| Host | Role | Env vars |
|------|------|----------|
| `relay-sg` (Singapore) | Primary relay, client Alice | `BIND_ADDR=0.0.0.0:443`, `DATABASE_URL=sqlite:///data/relay.db`, `PEER_RELAYS=https://relay-jp` |
| `relay-jp` (Japan) | Secondary relay, client Bob | `BIND_ADDR=0.0.0.0:443`, `DATABASE_URL=sqlite:///data/relay.db`, `PEER_RELAYS=https://relay-sg` |

### 1.2 Shared Test Harness

Build a small Rust binary `hb-it` (integration test runner) that:
- Accepts `--relay-sg <url>` and `--relay-jp <url>` flags
- Runs each test suite and emits TAP output
- Uses `hb_core` directly (no Tauri layer) — same keypair primitives as production

```
cargo build -p hb-it --release
# on local machine, targeting both VPS addresses:
./hb-it --relay-sg https://relay-sg.example.com --relay-jp https://relay-jp.example.com
```

### 1.3 Clock Skew Baseline

Before running: record actual clock delta between machines.
```bash
# run on local machine
ssh relay-sg 'date +%s' && date +%s  # compare
ssh relay-jp 'date +%s' && date +%s
```
Expected: < 2s delta with NTP. Any delta > 5s will cause timestamp freshness failures — flag it.

---

## 2. Suite A — Relay HTTP API (Cross-Network)

These tests run the same HTTP calls as unit tests but over the real internet, exercising TLS termination, actual TCP/QUIC handshake latency, and real SQLite write paths.

### A1 — Health endpoint reachability
**Why unit tests miss this:** Unit tests mock the state; this checks TLS cert validity, network routing, and the binary actually running.

```
GET https://relay-sg/v1/health  → 200, ok=true
GET https://relay-jp/v1/health  → 200, ok=true
```
Verify: `peers` array on relay-sg contains relay-jp URL and vice versa.

### A2 — Cross-relay peer advertisement (relay peering)
**Why unit tests miss this:** No relay-to-relay communication in unit tests at all.

1. Send heartbeat for Alice to `relay-sg`
2. Query `GET relay-jp/v1/peer/<alice_pubkey>` — expect it NOT to know Alice (relay-jp has no direct data)
3. Verify `relay-sg/v1/health` peers list shows relay-jp
4. _This test establishes the boundary: the app accumulates relays client-side; relays do NOT proxy peer lookups to each other. This should confirm relays are dumb pipes, not a mesh._

### A3 — Heartbeat across geographic distance
**Why unit tests miss this:** Timestamp freshness window (300s) is tested with mocked clocks. Real clocks between SG and JP can drift.

1. Alice generates keypair on local machine
2. Craft heartbeat signed with `signed_at = now()` on local machine
3. POST to `relay-sg` (70–100ms RTT) — must accept
4. POST same heartbeat body to `relay-jp` — must accept despite clock difference
5. **Edge case:** Craft heartbeat with `signed_at = now() - 290s` and confirm it passes both relays. Then try `now() - 310s` — must be rejected.

### A4 — Simultaneous heartbeat updates (concurrency)
**Why unit tests miss this:** SQLite `ON CONFLICT DO UPDATE` under concurrent writes from two different goroutines is in-process in unit tests. Real concurrent HTTP clients from two IPs are not tested.

1. Fire 20 heartbeat POSTs concurrently from `relay-sg` and 20 from `relay-jp` for the same `pubkey`
2. All should return 200
3. `GET /v1/peer/<pubkey>` must return `online=true`
4. `last_seen` must equal the most recent update's time (within 1s)

### A5 — Rate limiting from two distinct IPs
**Why unit tests miss this:** Unit tests use `127.0.0.1` for all requests. The rate limiter is IP-keyed; the sweep behaviour and actual IP separation are not tested.

1. From `relay-sg`, send 31 heartbeat POSTs in one burst to `relay-jp` (limit is 30/min default)
2. The 31st must return 429 or 400 with "rate limit exceeded"
3. From `relay-jp`, send a heartbeat to `relay-sg` — must succeed (different IP, separate bucket)
4. Wait for rate limit window to expire (~60s), resend from same IP — must succeed again

### A6 — Mailbox fill race (concurrent publish to same recipient)
**Why unit tests miss this:** Unit tests fill the mailbox sequentially. Two real clients sending simultaneously can race on the `count_messages_for` check before either insert completes.

1. Alice and Bob each have keypairs
2. 10 goroutines on `relay-sg` + 10 goroutines on `relay-jp` simultaneously POST messages to Carol's mailbox
3. Insert 490 messages sequentially first to bring Carol near the cap (500)
4. Then fire the concurrent 20
5. Verify final count ≤ 500 (SQLite `UNIQUE(from_key, sent_at)` and count check prevent overflow)
6. Verify no messages above cap were accepted

### A7 — Per-sender cap (M6) cross-relay
**Why unit tests miss this:** M6 sender caps are counted per-relay — if Alice sends 200 messages to relay-sg and 200 to relay-jp, each relay allows it because they have separate DBs. This is expected behaviour but needs explicit documentation/test.

1. Send `MAX_MESSAGES_PER_SENDER=200` messages from Alice to various recipients on `relay-sg` — 201st must be rejected
2. Repeat on `relay-jp` — 201st on JP must also be rejected, independently
3. Confirm this is by design: each relay has its own quota

---

## 3. Suite B — End-to-End DM Flow

Tests the full message lifecycle: sign → publish → relay stores → authenticated fetch → decrypt.

### B1 — Happy path: Alice sends DM to Bob
**Participants:** Alice (local), Bob (local), relay = `relay-sg`

1. Alice and Bob generate keypairs (`HoardbookKeypair::generate()`)
2. Bob registers online: POST heartbeat to `relay-sg`
3. Alice sends DM: `ChatMessage { to: bob.hb_id(), content: "hello", encrypted: false }`
4. Alice wraps in `SignedEnvelope::create(&alice_kp, DocType::Message, &msg)`
5. POST `{type: "message", document: envelope}` to `relay-sg/v1/publish` → 200
6. Bob reads mailbox: GET `relay-sg/v1/messages/<bob_pubkey>?signed_at=...&signature=...` → 200, messages = [Alice's message]
7. Bob verifies envelope signature locally
8. Assert: message content matches, sender matches Alice's pubkey

### B2 — Bob reads his mailbox from the other relay
**Why this matters:** Bob may have only added `relay-jp` to his bootstrap list.

1. Same setup as B1, message stored on `relay-sg`
2. Bob queries `relay-jp/v1/messages/<bob_pubkey>` — must return empty (relays don't sync)
3. This confirms the user flow: you must share the same relay as your contacts, or use a relay both know

### B3 — Mailbox authentication: wrong key cannot read
**Why unit tests miss this:** Unit tests use mocked in-process state; the auth header path over HTTP is not tested.

1. Eve (attacker) generates her own keypair
2. Eve tries to GET Bob's mailbox: constructs `MailboxAuthQuery` signed by Eve's key
3. Relay must return 400/401 (signed key ≠ path pubkey)
4. Test with: forged signature, stale timestamp, correct signature but wrong mailbox path

### B4 — Message deduplication across retries
**Why unit tests miss this:** Retry logic over a real network with packet loss.

1. Simulate a retry: POST the same signed envelope twice (same `from_key` + `sent_at`)
2. `relay-sg` must store exactly one copy (`INSERT OR IGNORE`)
3. Bob's GET must return 1 message, not 2

### B5 — Stale message timestamp rejected over real network
**Why unit tests miss this:** Clock skew between sender and relay under real conditions.

1. Alice crafts a message with `sent_at = now() - 290s` (just inside the 300s window)
2. POST to `relay-sg` — must accept (add buffer for SG→local round trip latency of ~100ms)
3. Alice crafts a message with `sent_at = now() - 305s` — must be rejected
4. **Edge case specific to distributed setup:** if the test runner's clock lags the relay's NTP clock by 2s, a 298s-old message might appear 300s old to the relay. This catches any clock synchronisation issues.

### B6 — E2E encrypted message (encrypted=true)
1. Alice performs X25519 DH with Bob's public key, encrypts "secret" with ChaCha20-Poly1305
2. Sends `ChatMessage { encrypted: true, content: <ciphertext> }` to relay
3. Relay stores opaque bytes — must not inspect content
4. Bob decrypts and gets "secret"
5. Assert relay stored count is exactly 1

---

## 4. Suite C — iroh P2P Layer

These tests require both VPS nodes to run the iroh endpoint (direct P2P, not just relay HTTP).

### C1 — iroh endpoint reachability SG ↔ JP
**Why unit tests miss this:** QUIC over the real internet, PMTUD, firewall rules.

1. Start `hb-app` background service on both VPS (headless, CLI mode)
2. Alice (SG) sends heartbeat with her iroh `NodeAddr` to `relay-sg`
3. Bob (JP) fetches Alice's peer record: `GET relay-sg/v1/peer/<alice>` → `node_addr` present
4. Bob establishes iroh connection to Alice's `NodeAddr`
5. Assert connection established within 10s
6. Assert round-trip ping ≤ 150ms (SG↔JP typical RTT is 70–90ms)

### C2 — NAT traversal: relay-assisted hole punch
**Why unit tests miss this:** Both VPS have static IPs, so this test is more relevant to clients behind NAT. However, you can simulate it by:

1. Deploy a third lightweight client (local machine behind home NAT, or a Docker container with iptables masquerade)
2. Client C (behind NAT) connects to relay-sg for discovery
3. Alice (SG VPS, direct IP) connects to C using iroh's QUIC relay fallback path
4. Verify connection succeeds via relay transport (QUIC relay, not direct)
5. Enable direct mode in Alice's settings → iroh switches to direct path once hole-punch succeeds

### C3 — Direct connection IP warning UX path
**Why unit tests miss this:** The Tauri command layer + settings interaction.

1. Alice's settings: `allow_direct = false` (default)
2. Bob pastes Alice's key → app calls `paste_key` command → fetches peer from relay
3. Bob enables direct in settings → app shows IP exposure warning modal
4. After confirmation, iroh tries direct connection → verify `node_addr` used
5. Verify `relay_only` mode falls back to relay transport if direct fails

### C4 — iroh file transfer across regions
**Why unit tests miss this:** Real QUIC stream over 70ms+ latency with backpressure.

1. Alice shares a collection directory (`/data/test-collection/`) with 100 files, 10MB each (1GB total)
2. Bob requests download via `request_download` Tauri command
3. Measure: time to first byte, throughput, completion
4. Verify: all 100 files arrive intact (SHA-256 checksums)
5. Verify: `DownloadSlotGuard` RAII slot is released after completion
6. **Edge case:** Kill Alice's iroh connection mid-transfer — Bob should get an error, not a silent partial file

### C5 — Concurrent downloads (slot exhaustion)
**Why unit tests miss this:** The download slot limiter is untested under concurrent pressure.

1. Start 5 download requests from Bob simultaneously (each to different files)
2. If max slots = 3 (check `sharing.rs`), verify 2 are queued/rejected, 3 proceed
3. As slots free up, queued requests start
4. No slot leak: after all complete, available slots == initial max

---

## 5. Suite D — Identity & Signing Correctness

### D1 — Key rotation (succession document) round-trip
**Why unit tests miss this:** Succession requires relay + two-step publish + follower update.

1. Alice publishes profile on `relay-sg` with old key
2. Bob follows Alice, stores old key
3. Alice generates new keypair, creates succession document: `{old_key → new_key}`, signed by old key
4. Alice publishes succession to relay
5. Bob calls `refresh_contact(alice_old_id)` → app should detect succession and update contact record to new key
6. Verify Bob's stored contact now references Alice's new HbId
7. Verify old HbId can no longer be used to sign valid messages (old key retired)

### D2 — Tampered envelope rejected at relay
**Why unit tests miss this:** The HTTP transport layer; unit tests call handlers in-process.

1. Alice creates valid `SignedEnvelope`
2. Serialise to JSON, mutate `payload.content` via HTTP body manipulation
3. POST to relay — must return 400
4. Verify relay stores nothing

### D3 — HbId checksum rejection
1. Take a valid HbId (`hb1_...`)
2. Change the last character (corrupts 4-byte checksum)
3. POST heartbeat with this malformed `public_key` field to relay → 400
4. GET `/v1/peer/<malformed_id>` → 400
5. Verify the 4-byte checksum is the only difference between a valid and invalid ID

### D4 — Cross-platform key compatibility
**Why unit tests miss this:** Ed25519 keys generated on the SG VPS (Linux) must verify on JP VPS (Linux) and on Windows dev machine.

1. Generate keypair on SG VPS
2. Export via `export_keypair` command, transfer to JP
3. Import via `import_keypair` on JP
4. Sign a message on JP, verify on SG
5. Sign a message on SG, verify on JP

---

## 6. Suite E — Resilience & Failure Modes

### E1 — Relay unreachable: app degrades gracefully
1. Alice has contacts cached locally (`DataStore`)
2. `relay-sg` is stopped (simulate: `systemctl stop hb-relay`)
3. Alice's app heartbeat background task — verify it retries with backoff, does not crash
4. Alice checks contacts — should see cached (stale) data, with "Stale" badge
5. Relay comes back up → heartbeat succeeds, contact refreshes

### E2 — Relay restarts with empty DB (data loss)
1. Seed relay-sg with 10 heartbeats and 50 messages
2. Stop relay, delete `relay.db`, restart
3. All 10 peers show `online=false` (no heartbeat records)
4. All 50 messages gone (recipients get empty mailbox)
5. Clients gradually restore state via their normal heartbeat cycle

### E3 — Heartbeat online/offline transition over real time
**Why unit tests miss this:** The 600s online threshold requires real time to pass (or direct DB manipulation which doesn't test the scheduler).

1. Alice sends heartbeat to `relay-sg`
2. Immediately query `GET /v1/peer/<alice>` → `online=true`
3. Wait 601s without any heartbeat (use a test-mode env var to control `ONLINE_THRESHOLD_SECS` = 10s for this test)
4. Query again → `online=false`, `last_seen_at` present, `node_addr` absent
5. Send another heartbeat → `online=true` again

> **Implementation note:** Add `ONLINE_THRESHOLD_SECS` as an env var override for testing. Prod default = 600.

### E4 — Rate limit window expiry (real time)
1. Exhaust rate limit for IP X
2. Wait for full window (1 min) — **do not mock**
3. First request after window expires must succeed
4. Verifies `RateLimiter::sweep` and window reset logic in production

### E5 — DB connection pool exhaustion
1. Fire 200 concurrent HTTP requests to `relay-sg`
2. All should complete (SQLite connection pool queues internally)
3. None should return 500 from pool exhaustion
4. Verify median latency doesn't degrade past 500ms under load

### E6 — Large payload rejection
1. Construct a message envelope where `payload` is padded to 6.1KB (above `6 * 1024` limit)
2. POST to `relay-sg` → must return 413 or 400 with "too large"
3. Verify 6.0KB exactly is accepted

---

## 7. Suite F — Settings & Configuration Integration

### F1 — Relay URL validation blocks non-HTTPS
**Why unit tests miss this:** `is_acceptable_relay_url` is tested in unit tests, but the Tauri command path `settings_check_relay` is not tested against a running relay.

1. Set relay URL to `http://relay-sg` (not HTTPS) in settings
2. Call `check_relay` Tauri command → must return error ("HTTPS required")
3. Set to `https://relay-sg` → call `check_relay` → returns 200 from `/v1/health`
4. Verify relay URL is only saved if the check passes

### F2 — Direct connection settings round-trip
1. Default: `allow_direct = false`
2. `save_settings` with `allow_direct = true` → persists to `settings.json`
3. Restart app (fresh process) → `get_settings` returns `allow_direct = true`
4. iroh endpoint switches to direct mode for next connection attempt

---

## 8. Suite G — Performance Baselines

Run these to establish baselines before optimisation work. Not pass/fail, but alert if > 2x baseline.

| Metric | Expected | Alert threshold |
|--------|----------|-----------------|
| `POST /v1/heartbeat` p99 (SG→SG) | < 20ms | > 100ms |
| `POST /v1/heartbeat` p99 (JP→SG) | < 150ms | > 400ms |
| `GET /v1/peer/:pubkey` p99 (JP→SG) | < 150ms | > 400ms |
| `POST /v1/publish` (DM, 1KB) p99 | < 200ms | > 500ms |
| `GET /v1/messages/:pubkey` (100 msgs) p99 | < 200ms | > 500ms |
| iroh file transfer throughput (SG↔JP) | > 20MB/s | < 5MB/s |
| Heartbeat burst: 100 req/s sustained 30s | 0 errors | any 5xx |

---

## 9. Test Execution Order

```
1. Suite A (relay HTTP)       — no iroh required, fast feedback
2. Suite B (DM flow)          — depends on A passing
3. Suite D (identity/signing) — independent, run in parallel with B
4. Suite F (settings)         — depends on A passing
5. Suite C (iroh P2P)         — requires background service running on both VPS
6. Suite E (resilience)       — runs last; some tests intentionally disrupt services
7. Suite G (performance)      — run after all functional suites pass
```

---

## 10. Known Gaps (Not Covered Here)

- **Qurator key reuse** — deferred to Phase 2 per spec
- **DHT announce/search** (Task 22) — not yet implemented
- **Windows Credential Manager (DPAPI)** — requires a Windows runner; VPS are Linux
- **Auto-updater end-to-end** — requires a staging update server; out of scope for VPS testing
- **Multi-relay bootstrap accumulation** — the app accumulating up to 20 relays from peer gossip; requires more than 2 relay nodes

---

## 11. CI Integration

Once the `hb-it` binary exists, add a GitHub Actions workflow:

```yaml
# .github/workflows/integration.yml
on:
  workflow_dispatch:
  schedule:
    - cron: '0 6 * * *'   # daily at 06:00 UTC

jobs:
  integration:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo build -p hb-it --release
      - run: |
          ./target/release/hb-it \
            --relay-sg ${{ secrets.RELAY_SG_URL }} \
            --relay-jp ${{ secrets.RELAY_JP_URL }}
```

Suites C and E (iroh, resilience) require SSH access to VPS and should be gated behind a manual trigger.
