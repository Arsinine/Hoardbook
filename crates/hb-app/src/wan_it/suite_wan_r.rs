//! WAN-R — relay-set behavior in the wild (M20 W6 §W6). Two rows that are the **live twins of the
//! `hb-it` L2 RELAY suite** plus the **default-relay policy watch** (the canary row):
//!
//! - **R1** — degraded set: one dead relay (unroutable TEST-NET-1, RFC 5737) in the pool alongside a
//!   live VPS relay. Reads stay bounded (no half-open drag), publishes succeed via the live relay.
//!   Adapts `hb-it/suite_relay::relay1` to the live VPS backbone.
//! - **R2** — **default-relay policy watch** (the canary row, LIGHT-TOUCH): for each public default
//!   (damus / nos.lol / primal), publish ONE event of each our-kinds class (teaser / kind-11111
//!   presence / a gift-wrap 1059 / a 1117 announce) from a throwaway identity and record accept/reject
//!   PER RELAY PER KIND as the evidence table. This is the early-warning row for policy changes that
//!   would otherwise present as Hoardbook bugs. Low-rate: one event per kind per relay per run, no
//!   retries beyond the standard publish path.
//!
//! **R2 is the ONLY thing touching the public defaults.** Relay citizenship: R1 uses the live VPS
//! strfry + a TEST-NET-1 dead address; R2 publishes one event per kind per public relay per run (the
//! minimum that yields a per-kind policy signal). The evidence table is the deliverable.
//!
//! **Honest red.** R1 is a binary pass/fail. R2 is a MEASUREMENT row: it ALWAYS passes (the evidence
//! table is the point, not a gate), because a relay rejecting one of our kinds is a finding, not a
//! Hoardbook bug. A rejection is recorded in the table and printed to stderr; the row fails only if
//! the harness could not reach the relay at all (a connect failure — that IS a finding worth gating,
//! because it means the relay is down for everyone, not just policy-rejecting our kind).

use std::time::{Duration, Instant};

use anyhow::Result;
use hb_core::binding::KIND_PRESENCE;
use hb_core::event::{build_teaser, Teaser};
use hb_core::topic::KIND_TOPIC_ANNOUNCE;
use hb_core::{build_binding, Identity};
use hb_net::{RelayClient};
use nostr::prelude::*;

use crate::wan_it::tap::Tap;

// ---------------------------------------------------------------------------
// Constants — timeouts, settle (match WAN-P / WAN-U conventions)
// ---------------------------------------------------------------------------

/// Relay handshake/fetch timeout (matches WAN-P / WAN-U).
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);

/// Settle between a publish and a read (lets the live relay index the event).
const SETTLE: Duration = Duration::from_secs(3);

/// A presence beacon TTL comfortably inside the freshness window.
const PRESENCE_TTL_SECS: u64 = 30 * 60;

/// An unroutable address (TEST-NET-1, RFC 5737 — `192.0.2.0/24` is reserved for documentation and
/// guaranteed not to route). The dead relay in the degraded set.
const DEAD_RELAY: &str = "ws://192.0.2.1:7777";

/// The bound for a degraded-set read (R1): the connect+publish+fetch must not be dragged by the dead
/// relay. The hb-it twin (suite_relay::relay1) uses 25s against a localhost dead relay (port 1); the
/// WAN version must account for real WAN connect latency on the live relay PLUS the dead-relay
/// handshake timeout. The cliff is ~3 × RELAY_TIMEOUT (45s) — "every op waited the full timeout on
/// the dead host" — so the bound sits under that with WAN headroom.
const DEGRADED_BOUND: Duration = Duration::from_secs(40);

// ---------------------------------------------------------------------------
// Probe input — built by run_probe_wan_r from the parsed args
// ---------------------------------------------------------------------------

/// The input the WAN-R probe needs. R1 uses the live VPS relay set; R2 uses the public defaults.
pub struct ProbeInput {
    /// The live VPS relay URLs R1 uses (the degraded-set row).
    pub relays: Vec<String>,
}

/// Build the WAN-R probe input from the parsed args.
pub async fn build_probe_input(relays: Vec<String>) -> Result<ProbeInput> {
    Ok(ProbeInput { relays })
}

/// Run the WAN-R rows (R1–R2). Each row is an honest TAP check.
pub async fn run(tap: &mut Tap, probe: &ProbeInput) {
    tap.check(
        "R1: degraded set (one dead relay) — reads bounded, publishes succeed via the rest",
        r1_degraded_set(probe).await,
    );

    // R2 is the policy-watch measurement row. It records the per-relay-per-kind evidence table; the
    // row passes when every relay was REACHABLE (a connect failure is a real finding worth gating),
    // and the accept/reject split is the recorded evidence, not a pass/fail gate.
    tap.check(
        "R2: default-relay policy watch — per-relay per-kind accept/reject table recorded (canary row)",
        r2_default_relay_policy_watch().await,
    );
}

// ---------------------------------------------------------------------------
// R1 — degraded set (adapts hb-it/suite_relay::relay1 to the live VPS backbone)
//
// A set of [live VPS, dead TEST-NET-1]: connect succeeds (one relay came up), and a publish+fetch
// returns the live relay's result BOUNDED — it must not block on the dead relay for the full timeout
// on every call (the rate-limit/half-open drag the devtest described).
// ---------------------------------------------------------------------------

async fn r1_degraded_set(probe: &ProbeInput) -> Result<(), String> {
    if probe.relays.is_empty() {
        return Err("R1 needs at least one live --relay (the VPS strfry set)".to_string());
    }
    let live = probe.relays[0].clone();
    let set = vec![live.clone(), DEAD_RELAY.to_string()];
    let alice = Identity::generate();

    let started = Instant::now();
    let client = RelayClient::connect(&alice, &set, RELAY_TIMEOUT)
        .await
        .map_err(|e| format!("R1 connect (degraded set [live, dead]): {e}"))?;
    let beacon = build_binding(&alice, unix_now(), PRESENCE_TTL_SECS).map_err(|e| format!("R1 beacon: {e}"))?;
    client.publish(&beacon).await.map_err(|e| format!("R1 publish (accepted by the live relay): {e}"))?;
    settle().await;

    let got = client
        .fetch(
            Filter::new().author(alice.public_key()).kind(Kind::from_u16(KIND_PRESENCE)),
            RELAY_TIMEOUT,
        )
        .await
        .map_err(|e| format!("R1 fetch: {e}"))?;
    let elapsed = started.elapsed();
    client.disconnect().await;

    if got.is_empty() {
        return Err(format!(
            "R1 the live relay's presence was not returned despite a dead relay in the set (set: {:?})",
            set
        ));
    }
    if elapsed >= DEGRADED_BOUND {
        return Err(format!(
            "R1 a dead relay dragged the read to {elapsed:?} (>= {DEGRADED_BOUND:?}) — not bounded. \
             The half-open connection to {DEAD_RELAY} stalled every op."
        ));
    }
    eprintln!("   R1 degraded-set connect+publish+fetch completed in {:.2}s (bounded)", elapsed.as_secs_f64());
    Ok(())
}

// ---------------------------------------------------------------------------
// R2 — default-relay policy watch (the canary row, LIGHT-TOUCH)
//
// For each public default, publish ONE event of each our-kinds class from a throwaway identity and
// record accept/reject PER RELAY PER KIND as the evidence table. This is the early-warning row for
// policy changes that would otherwise present as Hoardbook bugs.
//
// Low-rate: one event per kind per relay per run. The publish path is the standard RelayClient path
// (no retries beyond the outcome relay publish returns).
// ---------------------------------------------------------------------------

/// The public default relays R2 watches (the production DEFAULT_RELAYS set).
const DEFAULTS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
];

/// The per-kind policy entry: relay × kind → accept/reject.
#[derive(Debug, Clone)]
struct PolicyEntry {
    relay: String,
    kind: &'static str,
    accepted: bool,
    note: String,
}

/// Run the default-relay policy watch. Returns the evidence table (printed to stderr regardless of
/// pass/fail). The row PASSES when every relay was reachable (a connect failure is a finding worth
/// gating — it means the relay is down for everyone). The accept/reject split is recorded evidence,
/// not a gate: a relay rejecting kind-11111 is a finding, not a Hoardbook bug.
pub(crate) async fn r2_default_relay_policy_watch() -> Result<(), String> {
    let mut table: Vec<PolicyEntry> = Vec::new();
    let mut connect_failures: Vec<String> = Vec::new();

    for relay_url in DEFAULTS {
        let relay = relay_url.to_string();
        let author = Identity::generate();
        let one = std::slice::from_ref(&relay);

        // Connect per relay. A connect failure is a real finding (the relay is down for everyone).
        let client = match RelayClient::connect(&author, one, RELAY_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                connect_failures.push(format!("{relay}: {e}"));
                // Record a connect-failure row for every kind so the table is complete.
                for kind in &["teaser", "presence", "giftwrap", "announce"] {
                    table.push(PolicyEntry {
                        relay: relay.clone(),
                        kind,
                        accepted: false,
                        note: format!("CONNECT FAILED: {e}"),
                    });
                }
                continue;
            }
        };

        // (1) Teaser (kind = KIND_TEASER). The production publish_profile / publish_collection path.
        let kind_label = "teaser";
        let token = token();
        let teaser_ev = match build_teaser(
            &author,
            &Teaser {
                display_name: format!("wan-r-r2-{token}"),
                bio: "hoards".into(),
                tags: vec![format!("wan-r-r2-{token}")],
                content_types: vec!["video".into()],
                picture: None,
            },
            true,
        ) {
            Ok(ev) => ev,
            Err(e) => {
                table.push(PolicyEntry {
                    relay: relay.clone(),
                    kind: kind_label,
                    accepted: false,
                    note: format!("BUILD FAILED: {e}"),
                });
                continue;
            }
        };
        record_outcome(&mut table, &relay, kind_label, client.publish(&teaser_ev).await);

        // (2) Presence (kind 11111). The production publish_presence path.
        let kind_label = "presence";
        let beacon = match build_binding(&author, unix_now(), PRESENCE_TTL_SECS) {
            Ok(b) => b,
            Err(e) => {
                table.push(PolicyEntry {
                    relay: relay.clone(),
                    kind: kind_label,
                    accepted: false,
                    note: format!("BUILD FAILED: {e}"),
                });
                continue;
            }
        };
        record_outcome(&mut table, &relay, kind_label, client.publish(&beacon).await);

        // (3) Gift-wrap (kind 1059). A self-addressed gift wrap (the outer wrap the production DM
        //     path publishes). Build a minimal gift wrap addressed to ourselves.
        let kind_label = "giftwrap";
        let wrap = match build_self_giftwrap(&author).await {
            Ok(w) => w,
            Err(e) => {
                table.push(PolicyEntry {
                    relay: relay.clone(),
                    kind: kind_label,
                    accepted: false,
                    note: format!("BUILD FAILED: {e}"),
                });
                continue;
            }
        };
        record_outcome(&mut table, &relay, kind_label, client.publish(&wrap).await);

        // (4) Announce (kind 1117). The production topic-announce path.
        let kind_label = "announce";
        let announce = match build_announce(&author, &token).await {
            Ok(a) => a,
            Err(e) => {
                table.push(PolicyEntry {
                    relay: relay.clone(),
                    kind: kind_label,
                    accepted: false,
                    note: format!("BUILD FAILED: {e}"),
                });
                continue;
            }
        };
        record_outcome(&mut table, &relay, kind_label, client.publish(&announce).await);

        client.disconnect().await;

        // One-event-per-kind-per-relay-per-run cadence: a small gap so the rate limiter on a shared
        // relay does not collapse the four publishes into a single reject burst.
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // The evidence table — printed to stderr (the TAP diagnostic stream) in full. This is the
    // high-value deliverable: the per-relay per-kind policy snapshot.
    eprintln!("   R2 default-relay policy table:");
    eprintln!("   R2   {:<28} {:<10} {:<8} note", "relay", "kind", "accept");
    for e in &table {
        eprintln!(
            "   R2   {:<28} {:<10} {:<8} {}",
            e.relay, e.kind, e.accepted, e.note
        );
    }

    // The row fails only on connect failures (a relay down for everyone is a real finding). A
    // kind-level reject is recorded evidence, not a failure.
    if !connect_failures.is_empty() {
        return Err(format!(
            "R2 connect failures (these relays were unreachable — a real finding, not a policy \
             reject): {}",
            connect_failures.join("; ")
        ));
    }
    Ok(())
}

/// Record a publish outcome into the evidence table.
fn record_outcome(
    table: &mut Vec<PolicyEntry>,
    relay: &str,
    kind: &'static str,
    result: Result<hb_net::PublishOutcome, hb_net::NetError>,
) {
    match result {
        Ok(o) => {
            let accepted = !o.accepted.is_empty();
            let note = if accepted {
                format!("accepted: {}", o.accepted.join(", "))
            } else {
                format!("all rejected: {:?}", o.rejected)
            };
            table.push(PolicyEntry { relay: relay.to_string(), kind, accepted, note });
        }
        Err(e) => table.push(PolicyEntry {
            relay: relay.to_string(),
            kind,
            accepted: false,
            note: format!("PUBLISH ERROR: {e}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Event builders for the gift-wrap + announce classes
// ---------------------------------------------------------------------------

/// Build a self-addressed gift wrap (kind 1059) — the outer event the production DM path publishes.
/// Uses nostr-sdk's `EventBuilder::gift_wrap` (the same path `wrap_dm` takes internally). Minimal: a
/// wrap addressed to ourselves carrying a tiny DM rumor, so the relay sees a well-formed 1059.
async fn build_self_giftwrap(author: &Identity) -> Result<Event, String> {
    // Self-addressed (author is both sender and recipient) so no second identity is needed. The
    // gift_wrap builder signs the seal with the author and the outer wrap with a fresh ephemeral.
    // The rumor is a PrivateDirectMessage (kind 14) — the inner kind the production DM path uses.
    let rumor = EventBuilder::private_msg_rumor(author.public_key(), "wan-r-r2")
        .build(author.public_key());
    EventBuilder::gift_wrap(author.keys(), &author.public_key(), rumor, None)
        .await
        .map_err(|e| format!("gift_wrap build: {e}"))
}

/// Build a kind-31117 topic-announce event (the production `build_announce` path). Minimal: a single
/// `d`-tag identifier so it is a well-formed parameterized-replaceable announce. Signed by the author
/// via the same `Identity::sign` path `build_announce` uses.
async fn build_announce(author: &Identity, token: &str) -> Result<Event, String> {
    let builder = EventBuilder::new(Kind::from_u16(KIND_TOPIC_ANNOUNCE), "wan-r-r2")
        .tag(Tag::identifier(format!("wan-r-r2-{token}")));
    builder
        .sign(author.keys())
        .await
        .map_err(|e| format!("announce sign: {e}"))
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A small settle after a publish before a read (lets the live relay index the event).
async fn settle() {
    tokio::time::sleep(SETTLE).await;
}

/// A short hex token to namespace this run's events (so they are distinct from prior runs).
fn token() -> String {
    let bytes: [u8; 4] = rand::random();
    hex::encode(bytes)
}

// ---------------------------------------------------------------------------
// Unit tests for the pure helpers (no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_outcome_records_an_accept() {
        let mut table = Vec::new();
        let outcome = hb_net::PublishOutcome {
            accepted: vec!["wss://x".to_string()],
            rejected: vec![],
        };
        record_outcome(&mut table, "wss://x", "teaser", Ok(outcome));
        assert_eq!(table.len(), 1);
        assert!(table[0].accepted);
        assert_eq!(table[0].kind, "teaser");
    }

    #[test]
    fn record_outcome_records_a_reject() {
        let mut table = Vec::new();
        let outcome = hb_net::PublishOutcome {
            accepted: vec![],
            rejected: vec![("wss://x".to_string(), "blocked".to_string())],
        };
        record_outcome(&mut table, "wss://x", "presence", Ok(outcome));
        assert!(!table[0].accepted);
        assert!(table[0].note.contains("rejected"));
    }

    #[test]
    fn record_outcome_records_an_error() {
        let mut table = Vec::new();
        let err = hb_net::NetError::PublishRejected("all rejected".into());
        record_outcome(&mut table, "wss://x", "giftwrap", Err(err));
        assert!(!table[0].accepted);
        assert!(table[0].note.contains("PUBLISH ERROR"));
    }
}
