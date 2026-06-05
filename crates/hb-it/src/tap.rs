/// A single test outcome.
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

    pub fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, detail: Some(format!("SKIP {}", reason.into())) }
    }
}

pub fn print_results(results: &[TestResult]) {
    println!("TAP version 13");
    println!("1..{}", results.len());
    for (i, r) in results.iter().enumerate() {
        let status = if r.passed { "ok" } else { "not ok" };
        let suffix = match &r.detail {
            Some(d) if d.starts_with("SKIP") => format!(" # {d}"),
            Some(d) => format!("\n  ---\n  detail: {d}\n  ..."),
            None => String::new(),
        };
        println!("{} {} - {}{}", status, i + 1, r.name, suffix);
    }
    let failed: usize = results.iter().filter(|r| !r.passed).count();
    let skipped: usize = results.iter().filter(|r| r.detail.as_deref().map(|d| d.starts_with("SKIP")).unwrap_or(false)).count();
    eprintln!(
        "\n{} tests: {} passed, {} failed, {} skipped",
        results.len(),
        results.len() - failed - skipped,
        failed,
        skipped,
    );
}
