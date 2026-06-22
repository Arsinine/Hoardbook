//! Suite CANARY — the VPS canary cycle, exercised in CI against the ephemeral relay set (M9 Track K;
//! HANDOVER §A2.2). Proves the full cycle (publish → fetch → verify → DM → cross-region) passes, a
//! soak loop doesn't flake, and a deliberately-broken relay makes the canary exit nonzero (the alarm
//! fires). The pure aggregation / exit-code / marker guarantees are unit-tested in `canary`.

use anyhow::{ensure, Result};

use crate::canary::run_canary;
use crate::harness::{result, Ctx};
use crate::tap::TestResult;

/// How many cycles the soak runs (each is a full publish→fetch→DM→cross-region round-trip).
const SOAK_CYCLES: usize = 2;

pub async fn run(ctx: &Ctx) -> Vec<TestResult> {
    vec![
        result("CANARY1 full cycle (publish/fetch/verify/DM/cross-region)", full_cycle(ctx).await),
        result("CANARY2 soak (repeated cycles, no flake/leak)", soak(ctx).await),
        result("CANARY3 broken relay → nonzero exit (alarm fires)", broken_relay().await),
    ]
}

/// A full cycle against the real (ephemeral) relay set must be all-green — including cross-region
/// when ≥2 relays are configured (the `integration` CI job starts two).
async fn full_cycle(ctx: &Ctx) -> Result<()> {
    let run = run_canary(&ctx.relays).await;
    ensure!(run.all_passed(), "canary cycle had failures: {}", run.to_json());
    ensure!(run.exit_code() == 0, "a green cycle must exit 0");
    // With a 2nd relay, the cross-region step must have actually run (not skipped).
    if ctx.multi() {
        let xr = run
            .results
            .iter()
            .find(|r| r.name.contains("cross-region"))
            .expect("a cross-region step exists");
        ensure!(
            xr.passed && xr.detail.as_deref().map(|d| !d.starts_with("SKIP")).unwrap_or(true),
            "cross-region must run (not skip) with two relays"
        );
    }
    Ok(())
}

/// Repeated cycles must all pass — catches flakiness / a leak that only shows under repetition.
async fn soak(ctx: &Ctx) -> Result<()> {
    for i in 0..SOAK_CYCLES {
        let run = run_canary(&ctx.relays).await;
        ensure!(run.all_passed(), "soak cycle {i} failed: {}", run.to_json());
    }
    Ok(())
}

/// A canary pointed at an unreachable relay must fail (nonzero exit) — proof the alarm actually
/// fires, rather than a broken backbone silently passing.
async fn broken_relay() -> Result<()> {
    // Port 1 is privileged + unbound — connect is refused fast.
    let run = run_canary(&["ws://127.0.0.1:1".to_string()]).await;
    ensure!(!run.all_passed(), "a broken relay must NOT pass");
    ensure!(run.exit_code() != 0, "a broken relay must exit nonzero (so systemd/alerting catches it)");
    Ok(())
}
