//! Minimal TAP 13 emitter for the L2 suites — a deliberate non-abstraction (the WAN sibling is
//! hb-app/src/wan_it/tap.rs). A skipped row is a THIRD outcome, constructed only by the explicit
//! [`TestResult::skip`]: it renders `ok <n> - <name> # SKIP <reason>` and does not fail the run.
//! The `# SKIP` directive and the skipped counter key on the `skipped` flag, never on detail text
//! — `ok`/`fail` (and every `harness::result` arm) cannot produce a skip, so no row can skip
//! itself by rephrasing its error.

/// One test outcome: a name, a pass/fail flag, a skip flag, and an optional diagnostic (failure
/// detail, or the skip reason on a skipped row).
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

    /// A deliberately skipped row: `passed: true` so it cannot fail the run (TAP 13: a skip is
    /// `ok`), `skipped: true` so it renders the `# SKIP <reason>` directive instead of a plain
    /// pass. The ONLY construction path for a skipped row — a skip is never derived from error
    /// wording.
    pub fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, skipped: true, detail: Some(reason.into()) }
    }
}

pub fn print_results(results: &[TestResult]) {
    println!("TAP version 13");
    println!("1..{}", results.len());
    for (i, r) in results.iter().enumerate() {
        println!("{}", render_row(i, r));
    }
    let (failed, skipped, passed) = counts(results);
    eprintln!("\n{} tests: {} passed, {} failed, {} skipped", results.len(), passed, failed, skipped);
}

/// Render row `i` (0-based) as its TAP 13 line. The one emission path for `print_results` and the
/// tests below — the tests pin what production actually prints, not a re-implementation.
fn render_row(i: usize, r: &TestResult) -> String {
    let n = i + 1;
    if r.skipped {
        let reason = r.detail.as_deref().unwrap_or("no reason given");
        format!("ok {n} - {} # SKIP {reason}", r.name)
    } else {
        let status = if r.passed { "ok" } else { "not ok" };
        let suffix = match &r.detail {
            Some(d) => format!("\n  ---\n  detail: {d}\n  ..."),
            None => String::new(),
        };
        format!("{status} {n} - {}{}", r.name, suffix)
    }
}

/// `(failed, skipped, passed)`, each from its own filter — never by subtraction. The old counter
/// sniffed "SKIP" out of the detail text, so a FAILED row whose error happened to start with
/// "SKIP" landed in both `failed` and `skipped`, and `len - failed - skipped` could underflow
/// `usize` (panic in debug, wrap in release). With the flag a row is counted exactly once: a skip
/// carries `passed: true`, so `!passed` and `skipped` are disjoint by construction.
fn counts(results: &[TestResult]) -> (usize, usize, usize) {
    let failed = results.iter().filter(|r| !r.passed).count();
    let skipped = results.iter().filter(|r| r.skipped).count();
    let passed = results.iter().filter(|r| r.passed && !r.skipped).count();
    (failed, skipped, passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direction 1 — a deliberate skip renders the TAP 13 `ok … # SKIP <reason>` directive, stays
    /// out of the failure count, and cannot fail the run: `main.rs` keys the process exit code on
    /// exactly `any(|r| !r.passed)` (main.rs:80/147), and a skip carries `passed: true`.
    ///
    /// MUTATION (P-10): in `TestResult::skip`, change `skipped: true` to `skipped: false` — one
    /// line, still compiles — and both asserts red (the row renders `ok` with a diagnostic block
    /// instead of the directive, and the skipped count drops to 0).
    #[test]
    fn a_skip_renders_the_directive_and_does_not_fail_the_run() {
        let row = TestResult::skip(
            "BR4 NIP-65 outbox: browse reaches a peer-only relay",
            "needs a 2nd --relay",
        );
        assert_eq!(
            render_row(0, &row),
            "ok 1 - BR4 NIP-65 outbox: browse reaches a peer-only relay # SKIP needs a 2nd --relay"
        );
        assert!(row.passed, "a skip must carry passed: true — that field is what fails the run");
        assert_eq!(counts(&[TestResult::ok("an ordinary pass"), row]), (0, 1, 1));
    }

    /// Direction 2 — a fail renders exactly what the emitter printed before `skipped` was a flag:
    /// `not ok`, diagnostic block, no directive. Byte-pinned because this change restructures
    /// emission (println-with-args into `render_row`), and an attribute-pinning suite is blind to
    /// a shape change.
    ///
    /// MUTATION (P-10): in `TestResult::fail`, change `passed: false` to `passed: true` — one
    /// line, still compiles — and both asserts red (the row renders `ok` with a diagnostic block,
    /// and the failure count drops to 0).
    #[test]
    fn a_fail_renders_not_ok_and_fails_the_run() {
        let row = TestResult::fail("DM4: sealed DM round-trip", "connection closed before hello");
        assert_eq!(
            render_row(0, &row),
            "not ok 1 - DM4: sealed DM round-trip\n  ---\n  detail: connection closed before \
             hello\n  ..."
        );
        assert_eq!(counts(std::slice::from_ref(&row)), (1, 0, 0));
    }

    /// The honesty prohibition, pinned: an `Err` whose detail merely SAYS "SKIP" still renders
    /// `not ok` with no directive. Fed through `harness::result`, the production Ok/Err mapping
    /// every suite uses — this is exactly the hole the emitter had (`detail.starts_with("SKIP")`
    /// decided the directive): a skip inferred from wording lets any row skip itself by rephrasing
    /// its error.
    ///
    /// MUTATION (P-10): in `render_row`, change `if r.skipped {` to
    /// `if r.skipped || r.detail.as_deref().is_some_and(|d| d.starts_with("SKIP")) {` — one line,
    /// still compiles — reintroducing the wording sniff, and both asserts red.
    #[test]
    fn an_err_never_renders_as_a_skip_regardless_of_wording() {
        let row = crate::harness::result(
            "ID4: keygrant on the second relay",
            Err(anyhow::anyhow!("SKIP needs a 2nd --relay")),
        );
        let rendered = render_row(0, &row);
        assert!(
            rendered.starts_with("not ok 1 - "),
            "an Err must render `not ok` even when its wording says SKIP — a wording-sniffed skip \
             is how a row skips itself by rephrasing its error. Got: {rendered}"
        );
        assert!(!rendered.contains("# SKIP"));
    }

    /// The underflow, pinned. The old summary counted skips by the same text prefix it matched
    /// failures' details against, so this exact one-row run computed `1 - 1 - 1` on `usize` —
    /// panic in a debug build, wrap in release. Each count now comes from its own filter, and the
    /// sum is pinned to `len` so a future double-count cannot come back silently.
    ///
    /// MUTATION (P-10): in `counts`, change the skipped filter's `|r| r.skipped` to
    /// `|r| r.detail.as_deref().map(|d| d.starts_with("SKIP")).unwrap_or(false)` — one
    /// expression, still compiles — and the failed row is counted as a skip again: `(1, 1, 0)`,
    /// sum 2, both asserts red.
    #[test]
    fn a_failed_row_saying_skip_is_counted_exactly_once() {
        let results = vec![TestResult::fail("M4: probe", "SKIP needs a 2nd --relay")];
        let (failed, skipped, passed) = counts(&results);
        assert_eq!(
            (failed, skipped, passed),
            (1, 0, 0),
            "a failed row whose detail says SKIP is a failure, never a skip"
        );
        assert_eq!(failed + skipped + passed, results.len(), "each row is counted exactly once");
    }
}
