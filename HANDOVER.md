# Session Handover

**Last updated: 2026-06-08 (DPAPI keypair-encryption bug found + fixed; see "Multi-platform test suite" below)**
**Branch:** `main`. The `scan_directory_inner` refactor + 3 regression tests are committed (defensive, kept). **The real fix — the DHT lazy-build change in `dht_service.rs` — is applied in the working tree (uncommitted), compiles, passes `hb-app` tests, clippy-clean on Linux. PENDING WINDOWS VERIFICATION (CPU check, see below).**

> Build note: `/mnt/c` (WSL2 9p mount) throws intermittent I/O errors (`os error 22`, `rustc-LLVM IO failure`) under heavy compile load, and the host C: drive runs near-full. Re-run on failure (artifacts persist). `CARGO_INCREMENTAL=0` reduces churn.

---

## ✅ Multi-platform test suite — REQUIRED (motivated by a latent Windows-only DPAPI bug)

**2026-06-08:** Found + fixed a critical bug present in every Windows build — the root cause of the 0-byte `keypair.bin` → "Identity file unreadable" recovery screen.

**Bug:** `crates/hb-dpapi/src/lib.rs::encrypt` passed flag `0x8` to `CryptProtectData`, with a comment misnaming it `CRYPTPROTECT_UI_FORBIDDEN`. `0x8` is actually `CRYPTPROTECT_CRED_SYNC` — a credential-sync op that returns `TRUE` **without encrypting**, leaving `pDataOut` null. The correct value is `0x1`. Every keypair "encryption" was therefore a no-op returning an empty blob → a 0-byte file. Debug builds abort on the resulting `slice::from_raw_parts(null,…)` (Rust UB check); release builds silently write the empty file → recovery screen on next launch. **Fixed:** flag `0x8` → `0x1`, plus a defensive null-blob guard in both `encrypt`/`decrypt`. `cargo test -p hb-dpapi` now passes on Windows (verified; keypair.bin writes 406 bytes in the real app).

**Why it was never caught:** the hb-dpapi tests are `#[cfg(all(test, target_os = "windows"))]`, and all dev + CI ran on **Linux/WSL** — so they **never executed**. Single-OS testing makes every `#[cfg(target_os = …)]` / FFI / DPAPI / keyring / path-handling branch invisible.

**Action required — multi-platform CI matrix** so platform-gated tests actually run:
- GitHub Actions matrix `{windows-latest, macos-latest, ubuntu-latest}` → `cargo test --workspace` (+ `cargo clippy`) on each. A green Linux run must no longer be treated as "tests pass".
- Guarantee every `#[cfg(target_os = "windows")]` / `"macos"` / `"linux"` test runs on its native runner. Treat the presence of such a gate as a hard requirement to include that OS in the matrix.
- Add a Windows-native end-to-end identity test (generate → `save_keypair` → reload via `load_keypair`/`get_identity` → decrypt) so save/load wiring is covered, not just the in-crate DPAPI round-trip.
- Audit all `#[cfg(target_os)]`-gated code so no platform branch is left untested.

---

## ⚠️ Add Collection — "Scanning…" infinite hang (Windows) — REAL CAUSE FOUND + FIXED (pending Windows verify)

**Status:** Previous "tokio time driver missing on Windows" theory is **disproven on Windows**. Real culprit is a **runaway `mainline::rpc::socket` UDP retry storm caused by two bugs in `dht_service::run_dht_announce_loop`**. The scan_directory refactor in HEAD is harmless but does not fix the hang. **The DHT fix is now applied (working tree) — see "Fix applied" below.**

### Symptom (still as reported on `Hoardbook_v0.4.1.exe`, Windows)
- Add Collection → pick any folder (incl. empty) → "Start scan" → button stuck on "Scanning…" forever, no error toast.
- Webview itself stays responsive (window drag, slider) — it's in a separate `msedgewebview2` process.

### What the Windows investigation actually showed (2026-06-07)

1. **Previous theory ruled out.** Reverted the body of `scan_directory_inner` to v0.4.1's raw `tokio::time::timeout(tokio::task::spawn_blocking(...))` block and ran `scan_completes_on_tauri_async_runtime` **on Windows MSVC** → **passed in 0.01s, no panic**. The construct does not require any runtime driver that's missing on Windows; the diagnosis in the prior session was wrong. Restored the new version; HEAD is unchanged.
2. **Live binary observation.** Launched the shipped `Hoardbook_v0.4.1.exe` (PID 47996, then 32204 on relaunch) and sampled CPU **with no user interaction**:
   - Steady-state **~280% CPU continuously from startup** — not climbing, not transient, ~14s CPU per 5s wall clock.
   - 28-44 threads, 3 in `Running` state at any time.
   - This is incompatible with the "silent panic, IPC never sent" model — the backend is *actively running*, very loudly.
3. **Identifying the runaway.** Wired a temporary `tracing-subscriber::fmt` init at the top of `pub fn run()` (debug builds already keep the console; `windows_subsystem` is `cfg_attr(not(debug_assertions), …)`), built `target/debug/hb-app.exe`, ran with `RUST_LOG=hb_app=debug,iroh=info,mainline=debug,reqwest=warn` and captured stderr to `.work/diag.log`. **1641 lines in 25 seconds, 1636 of them `mainline::rpc::socket`**:
   ```
   T+0.18s INFO  mainline::dht: Mainline DHT listening address=0.0.0.0:6881
   T+0.18s DEBUG mainline::rpc: Bootstrapping the routing table
   T+0.24s WARN  mainline::rpc::socket: IO error … (os error 10060)
   T+0.30s WARN  mainline::rpc::socket: IO error … (os error 10060)
   … ~16/sec, indefinitely …
   ```
   `os error 10060` = WSAETIMEDOUT. Windows firewall blocks outbound UDP to DHT bootstrap nodes (router.bittorrent.com / dht.libtorrent.org / …), `mainline` retries with no backoff. Diagnostic changes have been **reverted**; `git status` is clean. `.work/diag.log` is preserved (untracked, ~1 MB) for inspection.

### The two bugs (both in `crates/hb-app/src/dht_service.rs`)

**Bug #1 — `INITIAL_DELAY` is bypassed (lines 74–79).**
```rust
tokio::select! {
    _ = tokio::time::sleep(INITIAL_DELAY) => {}
    _ = cancel_rx.changed() => {              // returns IMMEDIATELY
        if *cancel_rx.borrow() { return; }    // borrow == false → falls through
    }
}
let dht = match mainline::Dht::builder().build() { … };   // runs at app launch
```
A freshly `.subscribe()`d `tokio::sync::watch::Receiver` has its initial value marked **unseen** (`subscribe()`'s documented behaviour), so `changed().await` returns on first poll. The cancel-value check (`false`) lets execution fall through to `mainline::Dht::builder().build()` — **the DHT is constructed within ~200 ms of process start**, not 30 s later. Trace timestamps in `.work/diag.log` confirm this.

**Bug #2 — DHT is constructed even when announce is disabled (lines 81–124).**
The `if settings.dht_announce_enabled` check sits **inside** the loop, *after* `mainline::Dht::builder().build()`. So users who never opted in to DHT discovery (i.e. nearly everyone — it's an opt-in feature shipped in T22) still pay the full bootstrap cost on every launch. The setting only gates whether to *announce*, not whether to *exist*.

### Why this manifests as "Scanning… forever"
~280% sustained CPU from `mainline::rpc::socket` retries either (a) starves Tauri's tokio worker pool so the `spawn_blocking(scan_directory)` task never gets picked up, or (b) creates so much process-wide scheduler pressure that the IPC response back to WebView2 is delayed past any user patience. Either explanation cleanly accounts for: empty folder hangs (no scan work, but no IPC reply either), webview stays responsive (separate process), and the bug never reproduces on Linux (outbound UDP for DHT bootstrap usually succeeds → bootstrap finishes in seconds → no retry storm → no CPU burn).

### Fix applied — `crates/hb-app/src/dht_service.rs::run_dht_announce_loop`

Implemented exactly the lazy-build approach. The DHT node is now `Option<AsyncDht>`, built **only** once `dht_announce_enabled` is set, and reused across cycles:
- **Bug #1:** `cancel_rx.mark_unchanged()` at the top, so the first `changed().await` no longer returns immediately (the receiver comes from `dht_cancel.subscribe()`, lib.rs:245).
- **Bug #2:** when `!dht_announce_enabled` the loop idles on `cancel_rx.changed().await` (returns on shutdown, `continue`s on re-check) and **never calls `mainline::Dht::builder().build()`**. The build + `INITIAL_DELAY` only run on the first enabled cycle; a build failure now backs off `ANNOUNCE_INTERVAL` before retrying instead of returning.
- The toggle path still works: `dht_start_announce`/`dht_stop_announce` send `dht_cancel.send(false)`, which wakes the idle `changed()` → re-reads settings → builds on enable.

Verified on Linux: compiles, `cargo test -p hb-app --lib dht_service::tests` green, `cargo clippy -p hb-app` clean. The announce logic itself is unchanged.

Notes / follow-ups (not done):
- **Tear-down on disable** is still a follow-up — once built, `mainline` exposes no clean shutdown, so disabling after enabling leaves the node alive until app exit. Acceptable for MVP (the default never-enabled user is fully protected).
- A **frontend safety net** (≈35 s client-side timeout in `ScanDialog.svelte::handleScan`) is still worth adding so the dialog can never trap itself again, independent of this root cause.
- The loop is not cleanly unit-testable (it builds a real DHT / runs forever), so no automated regression test was added — verification is the Windows CPU check below.

### Verifying the fix
On Windows MSVC:
1. `cargo build -p hb-app --bin hb-app` (debug — has console).
2. Run from PowerShell, watch `Get-Process hb-app | %{ $_.CPU }` over ~30 s — should sit near-idle (a few % CPU), not climb at ~3 s/s. (Optionally re-add the tracing-subscriber init temporarily and confirm `.work/diag.log` no longer has `mainline::rpc::socket` spam.)
3. Click Add Collection → empty folder → Start scan. Should complete in <1 s.

### State checklist after this session
- HEAD: includes the `scan_directory_inner` refactor + 3 regression tests. All 19 `commands::collection::tests` pass on Windows MSVC (0.11 s).
- `scan_directory` change is defensive (still correct, no time-driver dependency) — leave it in.
- **Working tree: the DHT fix in `dht_service.rs` is applied but UNCOMMITTED**, plus this HANDOVER edit. Commit it, then rebuild on Windows to verify (CPU check above). `.work/diag.log` preserved (untracked) as evidence.
- `Hoardbook_v0.4.1.exe` in repo root is the unfixed shipped binary; will continue to hang until a new build ships with the DHT fix.

---

## ⚠️ Second profiling test (2026-06-07, post-DHT-fix) — DHT was NOT the cause; runaway is a host-side GUI-loop spin

Re-profiled the **release** `hb-app.exe` that already contains the committed DHT fix (`641f7ba`), then captured an instrumented **debug** build (temporary `tracing-subscriber` init in `run()` — now reverted, tree clean). The CPU runaway persists, but every network/DHT/iroh theory is **disproven**. Method + evidence:

**Ruled OUT (each with direct evidence):**
- **Not network / DHT / iroh.** `Get-NetUDPEndpoint`/`Get-NetTCPConnection` on the live PID show **zero UDP sockets** — iroh and mainline DHT *both* always bind UDP, so neither is running. The previous session's `mainline::rpc::socket` storm is gone; the DHT fix worked. iroh additionally never starts here because `%APPDATA%\net.hoardbook\identity\keypair.bin` is **0 bytes** → `load_keypair` fails with DPAPI `0x57` (confirmed in the trace) → `start_iroh_endpoint` is skipped.
- **Not the tokio background tasks.** With no identity, the heartbeat loop no-ops, the DHT loop idles correctly on `cancel_rx.changed().await`, and the identity server blocks on `accept()`. The instrumented trace is **completely silent** after the two startup lines despite ~1 core burning.
- **Not the webview renderer / CSS.** During the spin the **host `hb-app.exe` burns ~1 core while the `msedgewebview2.exe` process is idle (<1 CPU-s)**. Neutralising every `animation: … infinite` in the built CSS and rebuilding **did not** reduce the spin. No `setInterval`/`requestAnimationFrame` flood and no self-retriggering Svelte `$:` reactive block in the mounted routes.

**What it IS (narrowed):**
- A **host-process native busy-loop**: **2 threads pegged in `Running`** (~0.6 + ~0.45 core), burning ~1 core on the debug build / ~1.8 cores on release. tao/wry/WebView2 use the `log` crate (not `tracing`), so they emit nothing under our subscriber — consistent with the spin living in that GUI/event-loop layer.
- **Foreground-dependent.** It only spins while the Hoardbook window is the foreground/visible window (a hidden-window launch stayed flat at 10 threads / 0.1 CPU; backgrounding it makes the per-thread deltas drop to ~0). This is the classic Tauri-on-Windows idle-CPU signature (continuous compositor/redraw wakeups of the in-proc WebView2 controller + tao message loop).

**Next diagnostic step (needs heavier tooling than this box has):** symbolicate the two hot threads with WPA/xperf (only in-box `wpr` + `tracerpt` are present; no `xperf`/WPA, so a 428 MB ETL couldn't be resolved to functions). Likely remediations to try once the frame is known: bump `tauri`/`wry`/`tao`, or pass WebView2 `additionalBrowserArgs` to disable the feature driving continuous composition. **The `keypair.bin` being 0 bytes is itself a bug to fix** (a failed/partial keypair write left an empty file → permanent "Identity file unreadable" recovery screen) and is a clean repro of the foreground spin.

**Amplifier — process stacking.** Found a leftover instance (PID 55936) holding **10 042 CPU-seconds**: repeated launches of the windowed app each spawn a full process + window + backend that keeps spinning, so CPU compounds across stale copies. This is what drove the original "~280–640% from startup" readings, and a stale instance owning ports/state while a fresh window starts is a plausible contributor to the "Scanning… forever" report. Fixed by the singleton work below.

> Tooling note: traces/evidence preserved under `.work/` (`diag5.log` = the instrumented startup trace; the 428 MB `hbcpu.etl` was deleted to reclaim disk). The temporary `tracing-subscriber` instrumentation has been reverted — `git status` shows only this HANDOVER edit.

## TODO: Make Hoardbook a singleton (single-instance) — only one running at a time

Per user request and the stacking evidence above. The Tauri builder currently registers no single-instance guard, so every launch (or every double-click) starts a brand-new process + window + backend tokio runtime, and stale copies keep burning CPU.

**Fix:** add [`tauri-plugin-single-instance`](https://github.com/tauri-apps/plugins-workspace/tree/v2/plugins/single-instance) (v2):
- `cargo add tauri-plugin-single-instance` in `crates/hb-app`.
- Register it **first** in `run()` (it must be the first plugin):
  ```rust
  .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
      // a second launch was attempted — focus the existing window instead
      if let Some(w) = app.get_webview_window("main") {
          let _ = w.show();
          let _ = w.set_focus();
      }
  }))
  ```
- This makes the second process exit immediately and hand off to the running one, so there can only ever be one backend.

Acceptance: launch hb-app twice → second invocation does not create a new process; the existing window gains focus; `Get-Process hb-app` shows exactly one PID. This also prevents future zombie-stacking from masking/inflating CPU during profiling.

> Note: the singleton guard fixes the *amplifier* (stacked zombie processes), not the underlying single-instance host-side GUI-loop spin documented above — both need addressing. The busy-loop's exact native frame still needs WPA/xperf symbolication (not available on this box).

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
