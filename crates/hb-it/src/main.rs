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

use anyhow::{bail, Result};

use harness::Ctx;

mod harness;
mod suite_ab;
mod suite_disc;
mod suite_dm;
mod suite_id;
mod suite_n;
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

    tap::print_results(&results);

    if results.iter().any(|r| !r.passed) {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Ctx> {
    let mut relays: Vec<String> = Vec::new();
    let mut pow: u8 = 0;
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
            other => bail!(
                "unknown argument: {other}\nusage: hb-it --relay <ws-url> [--relay <2nd>] [--pow <bits>]"
            ),
        }
        i += 1;
    }
    if relays.is_empty() {
        bail!("--relay <ws-url> is required (e.g. --relay ws://127.0.0.1:7777)");
    }
    // A fresh key's hex is a convenient per-run-unique token for namespacing discovery tags.
    let run_id = hb_core::Identity::generate().public_key().to_hex()[..16].to_string();
    Ok(Ctx { relays, pow, run_id })
}
