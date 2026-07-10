//! hb-it — Hoardbook L2 integration runner (Nostr relay/protocol).
//!
//! A **headless Nostr client** (no `hb-app`) that drives `hb-net`'s `RelayClient` against a real
//! ephemeral relay, covering everything reachable over the relay protocol — Suites N / DM / DISC
//! / ID + the L2 Suite AB rows. Replaces the retired custom-HTTP `hb-relay` runner.
//!
//! Usage:
//!   hb-it --relay <ws-url> [--relay <2nd-url>] [--pow <bits>]
//!
//! A 2nd `--relay` enables the multi-relay cases (DM4 dedup, ID4 cross-relay, AB8 withhold).
//! `--pow <bits>` exercises the DISC5 NIP-13 path. Output: TAP 13 to stdout; exit 0 if all pass,
//! 1 if any fail.

use std::time::Duration;

use anyhow::{bail, Result};

use harness::Ctx;

mod canary;
mod harness;
mod same_nat;
mod suite_ab;
mod suite_browse;
mod suite_canary;
mod suite_count;
mod suite_disc;
mod suite_dm;
mod suite_id;
mod suite_n;
mod suite_priv;
mod suite_relay;
mod suite_topic;
mod survey;
mod tap;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ctx = parse_args(&args)?;
    eprintln!(
        "hb-it L2 — relays: {:?}  multi-relay: {}  pow: {}",
        ctx.relays,
        ctx.multi(),
        ctx.pow
    );

    // Survey mode (out-of-CI): per-relay acceptance probe instead of the cooperative L2 suites.
    if ctx.survey {
        eprintln!("\n-- Survey: per-relay acceptance probe (kinds / new-key / retention / PoW) --");
        let results = survey::run(&ctx.relays).await;
        tap::print_results(&results);
        return Ok(()); // informational — a rejection is a finding, not a build failure
    }

    // Same-NAT mode (devtest #9, 2026-07-10): diagnosis-only — run against the LIVE relays to prove
    // (or disprove) that two identities publishing presence from one source IP both count as online.
    // No fix lives behind this flag; a shortfall just names which relay rejected which identity.
    if ctx.same_nat {
        eprintln!("\n-- Same-NAT: same-source-IP presence diagnosis (devtest #9) --");
        let results = same_nat::run(&ctx).await;
        tap::print_results(&results);
        if results.iter().any(|r| !r.passed) {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Canary mode (HANDOVER §A2.2): the live-backbone probe. With --interval it loops forever (the
    // systemd daemon form, logging an alert on each failure); without, it runs one cycle and exits
    // with its code (the systemd oneshot+timer form). Every event is hb-canary-tagged, so it never
    // pollutes real counts/discovery.
    if ctx.canary {
        if let Some(interval) = ctx.interval {
            eprintln!("hb-canary daemon: probing every {interval}s");
            loop {
                let run = canary::run_canary(&ctx.relays).await;
                tap::print_results(&run.to_tap());
                println!("{}", run.to_json());
                if !run.all_passed() {
                    eprintln!("[ALERT] hb-canary FAILED — {}", run.to_json());
                }
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
        } else {
            let run = canary::run_canary(&ctx.relays).await;
            tap::print_results(&run.to_tap());
            println!("{}", run.to_json());
            if !run.all_passed() {
                eprintln!("[ALERT] hb-canary FAILED — {}", run.to_json());
            }
            std::process::exit(run.exit_code());
        }
    }

    let mut results = Vec::new();
    eprintln!("\n-- Suite N: Nostr events (publish / fetch / replace / delete) --");
    results.extend(suite_n::run(&ctx).await);
    eprintln!("-- Suite DM: NIP-17 gift-wrapped DMs --");
    results.extend(suite_dm::run(&ctx).await);
    eprintln!("-- Suite DISC: discovery --");
    results.extend(suite_disc::run(&ctx).await);
    eprintln!("-- Suite ID: identity & signing --");
    results.extend(suite_id::run(&ctx).await);
    eprintln!("-- Suite AB: adversarial (L2 rows) --");
    results.extend(suite_ab::run(&ctx).await);
    eprintln!("-- Suite BROWSE: M3 value loop (publish / discover / browse / re-key) --");
    results.extend(suite_browse::run(&ctx).await);
    eprintln!("-- Suite PRIV: Private Collections (per-recipient gift-wrapped listings, M10) --");
    results.extend(suite_priv::run(&ctx).await);
    eprintln!("-- Suite TOPIC: Topics (announce / membership / channel / invite, M11) --");
    results.extend(suite_topic::run(&ctx).await);
    eprintln!("-- Suite COUNT: relay-derived count + canary exclusion (M9) --");
    results.extend(suite_count::run(&ctx).await);
    eprintln!("-- Suite RELAY: relay-set resilience + cross-client presence (M12 W1) --");
    results.extend(suite_relay::run(&ctx).await);
    eprintln!("-- Suite CANARY: live-backbone probe cycle + cross-region + soak (M9) --");
    results.extend(suite_canary::run(&ctx).await);

    tap::print_results(&results);

    if results.iter().any(|r| !r.passed) {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Ctx> {
    let mut relays: Vec<String> = Vec::new();
    let mut pow: u8 = 0;
    let mut survey = false;
    let mut canary = false;
    let mut same_nat = false;
    let mut interval: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--relay" => {
                i += 1;
                let url = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--relay requires a ws:// url"))?
                    .clone();
                relays.push(url);
            }
            "--pow" => {
                i += 1;
                pow = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--pow requires a bit count"))?
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--pow must be an integer 0-255"))?;
            }
            "--survey" => survey = true,
            "--canary" => canary = true,
            "--same-nat" => same_nat = true,
            "--interval" => {
                i += 1;
                interval = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--interval requires a seconds count"))?
                        .parse()
                        .map_err(|_| anyhow::anyhow!("--interval must be a positive integer (seconds)"))?,
                );
            }
            other => bail!(
                "unknown argument: {other}\nusage: hb-it --relay <ws-url> [--relay <2nd>] [--pow <bits>] [--survey] [--canary [--interval <secs>]] [--same-nat]"
            ),
        }
        i += 1;
    }
    if relays.is_empty() {
        bail!("--relay <ws-url> is required (e.g. --relay ws://127.0.0.1:7777)");
    }
    // A fresh key's hex is a convenient per-run-unique token for namespacing discovery tags.
    let run_id = hb_core::Identity::generate().public_key().to_hex()[..16].to_string();
    Ok(Ctx { relays, pow, run_id, survey, canary, interval, same_nat })
}
