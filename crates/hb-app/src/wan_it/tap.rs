//! Tiny TAP (Test Anything Protocol, version 13) emitter — a local copy of the `hb-it/src/tap.rs`
//! pattern. The WAN harness lives in-crate in `hb-app` (to reach `pub(crate)` production code) and
//! deliberately does NOT share the module across crates (project rule: three similar lines beat a
//! premature abstraction). Emits one `ok`/`not ok` row per check, carries a diagnostic block on
//! failure, and finishes with a summary line on stderr. The process exits 0 on all-pass, 1 if any
//! row failed — red rows stay honest (`# TODO`/skip is forbidden for expected-red WAN-P rows).
//! A skipped row is a THIRD outcome, deliberately constructed at the call site (`Tap::skip`): it
//! renders the TAP 13 directive `ok <n> - <name> # SKIP <reason>` and does not fail the run. A
//! skip can never come out of `check` — every `Err` maps to `not ok` — so no row can skip itself
//! by rephrasing its error text, and WAN-P's expected-red rows stay red.

/// One test outcome: a name, a pass/fail flag, a skip flag, and an optional diagnostic
/// (failure detail, or the skip reason on a skipped row).
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub skipped: bool,
    pub detail: Option<String>,
}

impl TestResult {
    pub fn ok(name: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, skipped: false, detail: None }
    }

    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), passed: false, skipped: false, detail: Some(detail.into()) }
    }

    /// A deliberately skipped row. `passed: true` so it cannot fail the run (TAP 13: a skip is
    /// `ok`); `skipped: true` so it renders the `# SKIP <reason>` directive instead of a plain
    /// pass. The only construction path is an explicit `Tap::skip` call — never derived from
    /// error wording.
    pub fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, skipped: true, detail: Some(reason.into()) }
    }
}

/// A small accumulator so the probe can collect rows from several suites and emit them in order.
pub struct Tap {
    rows: Vec<TestResult>,
}

impl Tap {
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    /// Record a row from a `Result<(), String>` (Ok ⇒ pass, Err(detail) ⇒ fail with diagnostic).
    pub fn check(&mut self, name: impl Into<String>, r: Result<(), String>) {
        match r {
            Ok(()) => self.rows.push(TestResult::ok(name)),
            Err(detail) => self.rows.push(TestResult::fail(name, detail)),
        }
    }

    /// Record a deliberately skipped row: `ok <n> - <name> # SKIP <reason>`, excluded from the
    /// exit code. The only skip entry point — `check` cannot produce one, so an `Err` can never
    /// render as a skip.
    pub fn skip(&mut self, name: impl Into<String>, reason: impl Into<String>) {
        self.rows.push(TestResult::skip(name, reason));
    }

    /// Print the TAP stream and return the process exit code (0 = all pass, 1 = any fail).
    /// Skipped rows do not fail the run — a skip carries `passed: true`.
    pub fn finish(self) -> std::process::ExitCode {
        print_results(&self.rows);
        if self.rows.iter().any(|r| !r.passed) {
            std::process::ExitCode::FAILURE
        } else {
            std::process::ExitCode::SUCCESS
        }
    }
}

/// Emit a TAP 13 stream for `results` (stdout), followed by a summary line on stderr.
pub fn print_results(results: &[TestResult]) {
    println!("TAP version 13");
    println!("1..{}", results.len());
    for (i, r) in results.iter().enumerate() {
        println!("{}", render_row(i, r));
    }
    let failed: usize = results.iter().filter(|r| !r.passed).count();
    let skipped: usize = results.iter().filter(|r| r.skipped).count();
    let passed: usize = results.iter().filter(|r| r.passed && !r.skipped).count();
    // The skipped clause appears only when something skipped, so a skip-free run's summary — like
    // every skip-free row — is byte-identical to what the emitter printed before skips existed.
    let skip_note = if skipped > 0 { format!(", {skipped} skipped") } else { String::new() };
    eprintln!("\n{} tests: {} passed, {} failed{}", results.len(), passed, failed, skip_note);
}

/// Render row `i` (0-based) as its TAP 13 line(s). The one emission path for `print_results` and
/// the tests below — what the tests pin is what production prints, not a re-implementation.
fn render_row(i: usize, r: &TestResult) -> String {
    let n = i + 1;
    if r.skipped {
        let reason = r.detail.as_deref().unwrap_or("no reason given");
        format!("ok {n} - {} # SKIP {reason}", r.name)
    } else {
        let status = if r.passed { "ok" } else { "not ok" };
        match &r.detail {
            Some(d) => format!("{status} {n} - {}\n  ---\n  detail: {d}\n  ...", r.name),
            None => format!("{status} {n} - {}", r.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direction 1 — a deliberate skip renders the TAP 13 `ok … # SKIP <reason>` directive and
    /// does NOT fail the run. This is the D3-not-armed contract: an unarmed D3 must read as `ok`,
    /// stay out of the failure count, and leave the process exit code 0.
    ///
    /// MUTATION (P-10): in `TestResult::skip`, change `passed: true` to `passed: false` — one
    /// line, still compiles — and the `finish()` assert reds (a skip would fail the run, D3's
    /// unarmed state would inflate wan-d's failure count again).
    #[test]
    fn a_skip_renders_the_directive_and_does_not_fail_the_run() {
        let row = TestResult::skip("D3: search-eviction", "not armed: pass --flood-relay");
        assert_eq!(
            render_row(0, &row),
            "ok 1 - D3: search-eviction # SKIP not armed: pass --flood-relay"
        );

        let mut tap = Tap::new();
        tap.check("an ordinary pass", Ok(()));
        tap.skip("D3: search-eviction", "not armed");
        assert_eq!(tap.finish(), std::process::ExitCode::SUCCESS);
    }

    /// Direction 2 — a fail still renders `not ok` with its diagnostic block and still produces a
    /// failing exit code. Every pre-existing `Err` row (including D3's citizenship refusal) keeps
    /// this exact rendering — byte-identical to what the emitter printed before skips existed.
    ///
    /// MUTATION (P-10): in `TestResult::fail`, change `passed: false` to `passed: true` — one
    /// line, still compiles — and both asserts here red (the row renders `ok` with a diagnostic
    /// block, and the run exits SUCCESS).
    #[test]
    fn a_fail_renders_not_ok_and_fails_the_run() {
        let row =
            TestResult::fail("D3", "REFUSED (relay citizenship): ws://example.test not allowed");
        let rendered = render_row(0, &row);
        assert_eq!(
            rendered,
            "not ok 1 - D3\n  ---\n  detail: REFUSED (relay citizenship): \
             ws://example.test not allowed\n  ..."
        );
        assert!(!rendered.contains("# SKIP"));

        let mut tap = Tap::new();
        tap.check("D1", Ok(()));
        tap.check("D3", Err("REFUSED (relay citizenship)".to_string()));
        assert_eq!(tap.finish(), std::process::ExitCode::FAILURE);
    }

    /// The honesty prohibition, pinned: no `Err` can render as a skip, no matter its wording. If
    /// skips could be sniffed out of error text, any row — including WAN-P's expected-red rows —
    /// could skip itself by rephrasing its error, which is exactly what tap.rs's header forbids
    /// and what the `hb-it` sibling emitter does today (`detail.starts_with("SKIP")`,
    /// hb-it/src/tap.rs:28). This test feeds `check` the exact old unarmed-D3 wording — a string
    /// that BEGS to be sniffed — and asserts it still comes out `not ok` and still fails the run.
    ///
    /// MUTATION (P-10): in `render_row`, change `if r.skipped {` to
    /// `if r.skipped || r.detail.as_deref().is_some_and(|d| d.contains("SKIPPED")) {` — one line,
    /// still compiles — reintroducing the wording-sniff at the render layer, and this test reds
    /// on the `starts_with("not ok")` assert.
    #[test]
    fn an_err_never_renders_as_a_skip_regardless_of_wording() {
        let mut tap = Tap::new();
        tap.check(
            "P2: expected-red WAN-P row",
            Err("D3 SKIPPED (not armed): pass --flood-relay".to_string()),
        );
        let rendered = render_row(0, &tap.rows[0]);
        assert!(
            rendered.starts_with("not ok 1 - "),
            "an Err must render `not ok` even when its wording says SKIPPED — wording-sniffed \
             skips are how a row skips itself and how WAN-P's expected-red rows would go quiet. \
             Got: {rendered}"
        );
        assert!(!rendered.contains("# SKIP"));
        assert_eq!(tap.finish(), std::process::ExitCode::FAILURE);
    }
}
