//! hb-it — Hoardbook integration test runner.
//!
//! Usage:
//!   hb-it --relay-sg <url> --relay-jp <url> [OPTIONS]
//!
//! Options:
//!   --rate-limit-max <N>    RATE_LIMIT_MAX on both relays (default: 30). Used by A5.
//!   --slow                  Also run A6, A7, H6–H10 (slow; needs configured VPS).
//!   --dht-sg-addr <ip:port> TCP identity server port on SG VPS (e.g. 1.2.3.4:6882).
//!                           Enables H3. Required for H6–H10 under --slow.
//!   --dht-jp-addr <ip:port> Same for JP VPS.
//!
//! Output: TAP 13 to stdout. Exit 0 if all tests pass, 1 if any fail.
//!
//! DHT slow-test setup (on each VPS before running with --slow):
//!   # Start hb-app in background mode with DHT enabled:
//!   # SG: announce tags=["anime","vhs"], content_types=["video"]
//!   # JP: announce tags=["anime","documentary"], content_types=["audio"]
//!   # Wait ~90s for DHT propagation, then run:
//!   #   hb-it ... --slow --dht-sg-addr <sg_ip>:6882 --dht-jp-addr <jp_ip>:6882

use anyhow::{bail, Result};
use reqwest::ClientBuilder;
use std::time::Duration;

mod helpers;
mod tap;
mod suite_a;
mod suite_b;
mod suite_d;
mod suite_h;

#[derive(Clone)]
pub struct Config {
    pub relay_sg: String,
    pub relay_jp: String,
    pub client: reqwest::Client,
    pub rate_limit_max: u32,
    pub slow: bool,
    /// Optional TCP identity server address on SG VPS (enables H3, H6-H10).
    pub dht_sg_addr: Option<String>,
    /// Optional TCP identity server address on JP VPS.
    pub dht_jp_addr: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = parse_args(&args)?;

    let mut results = Vec::new();
    eprintln!("-- Suite A: Relay HTTP API --");
    results.extend(suite_a::run(&cfg).await);
    eprintln!("-- Suite B: DM Flow --");
    results.extend(suite_b::run(&cfg).await);
    eprintln!("-- Suite D: Identity & Signing --");
    results.extend(suite_d::run(&cfg).await);
    eprintln!("-- Suite H: DHT Discovery --");
    results.extend(suite_h::run(&cfg).await);

    tap::print_results(&results);

    let failed = results.iter().filter(|r| !r.passed).count();
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Config> {
    let mut relay_sg = None;
    let mut relay_jp = None;
    let mut rate_limit_max: u32 = 30;
    let mut slow = false;
    let mut dht_sg_addr = None;
    let mut dht_jp_addr = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--relay-sg" => {
                i += 1;
                relay_sg = Some(args.get(i).ok_or_else(|| anyhow::anyhow!("--relay-sg requires a value"))?.trim_end_matches('/').to_string());
            }
            "--relay-jp" => {
                i += 1;
                relay_jp = Some(args.get(i).ok_or_else(|| anyhow::anyhow!("--relay-jp requires a value"))?.trim_end_matches('/').to_string());
            }
            "--rate-limit-max" => {
                i += 1;
                rate_limit_max = args.get(i)
                    .ok_or_else(|| anyhow::anyhow!("--rate-limit-max requires a value"))?
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--rate-limit-max must be a positive integer"))?;
            }
            "--slow" => slow = true,
            "--dht-sg-addr" => {
                i += 1;
                dht_sg_addr = Some(args.get(i).ok_or_else(|| anyhow::anyhow!("--dht-sg-addr requires a value"))?.clone());
            }
            "--dht-jp-addr" => {
                i += 1;
                dht_jp_addr = Some(args.get(i).ok_or_else(|| anyhow::anyhow!("--dht-jp-addr requires a value"))?.clone());
            }
            other => bail!("unknown argument: {other}"),
        }
        i += 1;
    }
    let relay_sg = relay_sg.ok_or_else(|| anyhow::anyhow!("--relay-sg is required"))?;
    let relay_jp = relay_jp.ok_or_else(|| anyhow::anyhow!("--relay-jp is required"))?;
    let client = ClientBuilder::new()
        .timeout(Duration::from_secs(15))
        .build()?;
    Ok(Config { relay_sg, relay_jp, client, rate_limit_max, slow, dht_sg_addr, dht_jp_addr })
}
