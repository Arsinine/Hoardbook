//! Manual diagnostic: verify topic discovery works end-to-end against arbitrary user-supplied
//! relays — and specifically against the app's SHIPPED DEFAULTS, which no automated suite touches.
//!
//! Why this exists. Every topic integration row (hb-it TOPIC1-13, WAN-T 1-5) runs against the
//! private VPS strfry backbone, which indexes everything. A public relay that silently declines to
//! index `#t` on kind 31117 would be INVISIBLE to that entire suite while breaking every user. This
//! probe is the only tool that looks at that gap: it runs the REAL `discover_public_topics` against
//! relays you name on the command line and prints, stage by stage, where a known-good announce is
//! lost (raw fetch → per-event discard → full production function with member-count scoring).
//!
//! Read-only: fetches and parses, publishes nothing. Not part of CI — run it by hand against
//! whatever relay set you want to audit (the shipped defaults live in
//! `crates/hb-app/ui/src/lib/default_relays.json`).
//!
//! Usage: `cargo run -p hb-net --example topic_discover_probe -- <root_topic> <relay> [relay...]`

use std::time::Duration;

use hb_core::topic::{parse_announce, KIND_TOPIC_ANNOUNCE};
use hb_core::Identity;
use hb_net::topic::{discover_public_topics, topic_discover_filter};
use hb_net::RelayClient;
use nostr::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (root, relays) = match args.split_first() {
        Some((r, rest)) if !rest.is_empty() => (r.clone(), rest.to_vec()),
        _ => {
            eprintln!(
                "Usage: cargo run -p hb-net --example topic_discover_probe -- <root_topic> <relay> [relay...]"
            );
            eprintln!(
                "  <root_topic>   topic root to search for (e.g. `video`).\n  <relay>...     one or more relay URLs to probe (e.g. the shipped defaults in crates/hb-app/ui/src/lib/default_relays.json).\nNo built-in defaults: the relay set is taken from the command line so this example never carries a second copy of it."
            );
            std::process::exit(2);
        }
    };
    println!("root = {root:?}\nrelays = {relays:?}\n");

    let ephemeral = Identity::generate();
    let client = RelayClient::connect(&ephemeral, &relays, Duration::from_secs(10)).await?;
    let timeout = Duration::from_secs(10);

    // ── Stage 1: the raw fetch, using production's own filter builder ────────────────────────────
    let filter = topic_discover_filter(&[root.clone()])?;
    println!("stage 1 — filter: {}", serde_json::to_string(&filter)?);
    let events = client.fetch(filter, timeout).await?;
    println!("stage 1 — RAW EVENTS RETURNED: {}\n", events.len());

    // ── Stage 2: per-event, exactly the two discards discover_public_topics performs ─────────────
    let mut candidates = 0usize;
    for ev in &events {
        let ident = ev.tags.identifier();
        match parse_announce(ev) {
            Err(e) => println!("  DISCARD parse_announce failed: {e}  (id={})", ev.id),
            Ok(meta) => {
                let id_ok = ident == Some(meta.topic_id.as_str());
                println!(
                    "  kind={} name={:?} topic_id={} d={:?} private={} identity_check={}",
                    ev.kind.as_u16(),
                    meta.name,
                    &meta.topic_id[..12.min(meta.topic_id.len())],
                    ident.map(|s| &s[..12.min(s.len())]),
                    meta.private,
                    if id_ok { "PASS" } else { "FAIL -> discarded" }
                );
                if id_ok {
                    candidates += 1;
                }
            }
        }
    }
    println!("\nstage 2 — SURVIVING CANDIDATES: {candidates}\n");

    // ── Stage 3: the whole production function, member_count scoring included ────────────────────
    let started = std::time::Instant::now();
    match discover_public_topics(&client, &[root.clone()], timeout).await {
        Ok(found) => {
            println!("stage 3 — discover_public_topics OK in {:?}: {} topic(s)", started.elapsed(), found.len());
            for (meta, count) in &found {
                println!("    {:?}  members={count}", meta.name);
            }
            println!(
                "\nVERDICT: {}",
                if found.is_empty() {
                    "the discovery path is BROKEN on this relay set — production returns EMPTY despite stage 1 seeing the announce."
                } else {
                    "the discovery path is HEALTHY on this relay set. If the app still shows nothing, the fault is client-side, not here (QURATOR-83 was exactly that: a cached empty root)."
                }
            );
        }
        Err(e) => println!("stage 3 — discover_public_topics ERRORED in {:?}: {e}\n\nVERDICT: REPRODUCED as an ERROR (v0.13.0 renders this as the confident negative).", started.elapsed()),
    }

    // ── Stage 4: sanity — is the kind reachable at all on this relay set? ────────────────────────
    let any = Filter::new().kind(Kind::from_u16(KIND_TOPIC_ANNOUNCE)).limit(5);
    let all = client.fetch(any, timeout).await?;
    println!("\nstage 4 — untagged kind-{KIND_TOPIC_ANNOUNCE} fetch (limit 5): {} events", all.len());
    Ok(())
}
