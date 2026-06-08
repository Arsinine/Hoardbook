# Regression-Test Authoring Prompt

A reusable prompt for an agent to write **regression tests** covering the bug classes
recorded in `HANDOVER.md`. The throughline of those bugs: each lived in a code path the
existing suite never executed — platform-gated FFI (`#[cfg(target_os = "windows")]` tests
that never ran on Linux CI), an untyped Svelte template that violated a serde contract,
and an async loop that only misbehaves under Windows network conditions. So every test
below must (1) assert the real invariant the bug broke, and (2) actually execute on the
runner where the bug lives. A test that "passes" by never running is the failure mode
that caused this.

## How to use
Hand the prompt block to a capable coding agent from the repo root. It pairs cleanly with
Chorus's `red-green` template (implementer blind to tests, deterministic verify loop).
Keep `HANDOVER.md` as the source of truth so the scenario list can't go stale.

---

## Prompt

```text
You are writing **regression tests** for the Hoardbook repo. The goal: for each
already-diagnosed bug recorded in HANDOVER.md, add a test that would have caught it —
one that FAILS on the buggy code and PASSES on the fix (classic red-green). The
recurring root cause was that these bugs lived in code paths the existing suite never
executed (platform-gated FFI, untyped Svelte templates, OS-specific async behavior),
so a test that "passes" by never running is a FAILURE here.

## Source of truth
1. Read HANDOVER.md end-to-end. Each "⚠️"/"✅" section documents a bug, its root cause,
   and the fix. Treat that list as the scenario inventory — do not invent bugs.
2. Read the fix commits on branch `fix/windows-dpapi-and-collection-render` and the
   relevant files before writing anything.

## Hard requirements for every test
- **Red-green proof.** State, for each test, the exact invariant it asserts and how it
  fails against the pre-fix code. Where practical, verify by checking out the parent
  commit (or reverting the one-line fix) and showing the test fails, then restoring.
- **It must actually run on the OS where the bug lives.** If a test is
  `#[cfg(target_os = "windows")]`, the CI matrix MUST run it on `windows-latest`. A
  green Linux run is not "tests pass." If the matrix doesn't cover that OS, add it
  (`{windows-latest, macos-latest, ubuntu-latest}` → `cargo test --workspace`).
- **Assert the observable invariant, not "it compiles."** E.g. "ciphertext is non-empty
  and != plaintext", not "encrypt() returned Ok".
- Match the existing test style/module conventions in each crate. No new test
  frameworks, no abstractions for single-use helpers (see CLAUDE.md: surgical, simple).
- If a scenario is genuinely not unit-testable as-is, say so explicitly and propose the
  smallest refactor (e.g. extract an inner fn) that makes the invariant testable —
  don't silently skip it.

## Scenarios to cover (cross-check against HANDOVER.md; add any I missed)
1. **DPAPI flag / null-blob (`crates/hb-dpapi/src/lib.rs`).** Round-trip: `decrypt(encrypt(x)) == x`;
   ciphertext is non-empty and != plaintext (this is what `0x8`=CRED_SYNC silently broke);
   `decrypt(&[])` and `decrypt(b"\x00\x01\x02")` return `Err`, never panic/UB.
   These are `#[cfg(target_os="windows")]` — ensure the matrix runs them on Windows.
2. **0-byte keypair.bin recovery (`load_keypair`, store.rs).** An empty/corrupt identity
   file must NOT dead-end on the "Identity file unreadable" recovery screen forever —
   assert the loader treats an empty file as "absent" (Ok(None)/regenerates), matching
   whatever the fix decides. Include the empty-file case explicitly.
3. **End-to-end identity wiring (Windows).** generate → save_keypair → reload via
   load_keypair/get_identity → decrypt. Covers the save/load glue, not just the in-crate
   DPAPI round-trip.
4. **Collection content_types contract (frontend).** The backend field is plural
   `content_types: Vec<String>`; the UI read singular `content_type` and threw at render.
   Add a type/render-level guard so a singular-vs-plural mismatch fails the build: wire
   `svelte-check` into CI, and/or a component test that renders a Collection with
   `content_types` populated and asserts the badges appear (and that a missing field
   doesn't throw).
5. **DHT lazy-build (`dht_service::run_dht_announce_loop`).** Assert the two invariants the
   runaway violated: (a) with `dht_announce_enabled = false`, `mainline::Dht::builder().build()`
   is never called; (b) a freshly-subscribed `cancel_rx` does not let the loop skip
   INITIAL_DELAY on first poll. If the loop isn't unit-testable, refactor out a testable
   decision fn rather than skipping.

## Deliverable
- The new tests, each with a one-line comment naming the HANDOVER scenario it locks in.
- Any CI matrix changes needed so the platform-gated tests actually execute.
- A short summary table: scenario → test name → file → which OS runner executes it →
  red-green status.
```
