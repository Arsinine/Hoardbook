//! Tiny TAP (Test Anything Protocol, version 13) emitter — a local copy of the `hb-it/src/tap.rs`
//! pattern. The WAN harness lives in-crate in `hb-app` (to reach `pub(crate)` production code) and
//! deliberately does NOT share the module across crates (project rule: three similar lines beat a
//! premature abstraction). Emits one `ok`/`not ok` row per check, carries a diagnostic block on
//! failure, and finishes with a summary line on stderr. The process exits 0 on all-pass, 1 if any
//! row failed — red rows stay honest (`# TODO`/skip is forbidden for expected-red WAN-P rows).

/// One test outcome: a name, a pass/fail flag, and an optional diagnostic (failure detail).
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub detail: Option<String>,
}

impl TestResult {
    pub fn ok(name: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, detail: None }
    }

    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), passed: false, detail: Some(detail.into()) }
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

    /// Print the TAP stream and return the process exit code (0 = all pass, 1 = any fail).
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
        let status = if r.passed { "ok" } else { "not ok" };
        match &r.detail {
            Some(d) => println!("{} {} - {}\n  ---\n  detail: {d}\n  ...", status, i + 1, r.name),
            None => println!("{} {} - {}", status, i + 1, r.name),
        }
    }
    let failed: usize = results.iter().filter(|r| !r.passed).count();
    eprintln!(
        "\n{} tests: {} passed, {} failed",
        results.len(),
        results.len() - failed,
        failed,
    );
}
