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
//! What it emits — stated precisely, because "read-only" is easy to overclaim (codex, 2026-08-13).
//! It publishes no Hoardbook event: no announce, no presence beacon, no DM, nothing durable. But it
//! is NOT silent on the wire. `RelayClient::connect` installs a signer and the SDK has NIP-42
//! auto-auth on, so **if a relay issues an AUTH challenge this will sign and send an AUTH event**,
//! and every fetch sends an observable REQ/CLOSE. A hostile relay can therefore log a signed
//! authentication and see which topic roots you asked about.
//!
//! That is acceptable here only because the key is a **throwaway `Identity::generate()`**, created
//! per run and never persisted — it is not your app identity, and no browse-key is anywhere near
//! this path. Do not "reuse" it by passing a real identity in.
//!
//! Not part of CI — run it by hand against whatever relay set you want to audit (the shipped
//! defaults live in `crates/hb-app/ui/src/lib/default_relays.json`).
//!
//! Usage: `cargo run -p hb-net --example topic_discover_probe -- <root_topic> <relay> [relay...]`

use std::collections::BTreeSet;
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
    let filter = topic_discover_filter(std::slice::from_ref(&root))?;
    println!("stage 1 — filter: {}", serde_json::to_string(&filter)?);
    let events = client.fetch(filter, timeout).await?;
    println!("stage 1 — RAW EVENTS RETURNED: {}\n", events.len());

    // ── Stage 2: the two PER-EVENT discards discover_public_topics performs ──────────────────────
    // NOT the whole filter. Production ALSO keeps only the newest event per `topic_id` (replaceable
    // semantics, hb-net/src/topic.rs). This stage is deliberately pre-dedupe, so re-announces of one
    // topic each count here — otherwise a drop between stage 2 and stage 3 would look like scoring
    // when it was only deduplication. `distinct topic_ids` below is the number to compare against
    // stage 3.
    let mut candidates = 0usize;
    let mut distinct: BTreeSet<String> = BTreeSet::new();
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
                    // `topic_id` is relay-controlled: parse_announce deserializes it as a plain
                    // String with no hex validation, so a signed hostile announce could put a
                    // multi-byte char here. Take CHARS, never bytes — byte-slicing would panic and
                    // kill the probe before stage 3.
                    truncate(&meta.topic_id, 12),
                    ident.map(|s| truncate(s, 12)),
                    meta.private,
                    if id_ok { "PASS" } else { "FAIL -> discarded" }
                );
                if id_ok {
                    candidates += 1;
                    distinct.insert(meta.topic_id.clone());
                }
            }
        }
    }
    println!("\nstage 2 — surviving events: {candidates}  (pre-dedupe)");
    println!("stage 2 — DISTINCT topic_ids: {}\n", distinct.len());

    // ── Stage 3: the whole production function, member_count scoring included ────────────────────
    let started = std::time::Instant::now();
    match discover_public_topics(&client, std::slice::from_ref(&root), timeout).await {
        Ok(found) => {
            println!("stage 3 — discover_public_topics OK in {:?}: {} topic(s)", started.elapsed(), found.len());
            for (meta, count) in &found {
                println!("    {:?}  members={count}", meta.name);
            }
            // The verdict must not overstate what two INDEPENDENT relay round-trips can establish:
            // stage 3 issues its own fetch, so an empty stage 3 next to a non-empty stage 1 is
            // suggestive, not proof — the second query genuinely may have seen something different.
            // Only claim a contradiction when stage 2 actually found a candidate.
            println!(
                "\nVERDICT: {}",
                match (found.is_empty(), distinct.is_empty()) {
                    (true, false) => "the discovery path looks BROKEN on this relay set — stage 2 found a valid candidate and production still returned EMPTY. (Stages 1 and 3 are separate fetches, so re-run before concluding.)",
                    (true, true) => "INCONCLUSIVE — production returned empty, but stage 2 had no valid candidate either, so there is nothing here for it to have lost. Check the announce exists on this relay set at all (stage 4).",
                    (false, _) => "the discovery path is HEALTHY on this relay set. If the app still shows nothing, the fault is client-side, not here (QURATOR-83 was exactly that: a cached empty root).",
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

/// First `n` CHARS of `s`. Never byte-slice a relay-controlled string: `topic_id` arrives from the
/// wire as an unvalidated `String` (`parse_announce` does no hex check), so a byte index can land
/// mid-codepoint and panic the probe on a signed hostile announce.
fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
