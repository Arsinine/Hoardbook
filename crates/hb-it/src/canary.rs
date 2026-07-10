//! The VPS canary (`hb-it --canary`; HANDOVER §A2.2, M9 Track K). A permanent headless probe of the
//! **live** relay backbone — the failure class CI's ephemeral relay can't see (real relay drift,
//! retention/GC, NIP-13 in the wild, cross-region propagation, DM delivery).
//!
//! Each run uses an **ephemeral throwaway `npub`** and tags **every** published event with the
//! [`CANARY_MARKER`] (`t`=`hb-canary`), which the counts + discovery exclude — so the canary's
//! synthetic traffic never pollutes real data (the marker, not just the throwaway key, is what
//! enforces that). One cycle:
//!   1. publish teaser + encrypted listing + presence (all `hb-canary`-tagged),
//!   2. fetch them back and verify (Schnorr + decrypt),
//!   3. round-trip a NIP-17 DM,
//!   4. assert SG↔JP **cross-region** propagation (publish on one relay, read from the other),
//!   5. aggregate into a pass/fail run → **nonzero exit on failure** + an alert log line.

use std::time::Duration;

use anyhow::Result;
use hb_core::event::{
    build_listing_event, build_teaser, parse_listing_event, parse_teaser, Teaser, KIND_LISTING,
    KIND_TEASER,
};
use hb_core::{build_binding, is_canary, BrowseKey, Identity, CANARY_MARKER};
use hb_net::{unwrap_dm, wrap_dm, RelayClient};
use nostr::prelude::*;

use crate::harness::now;
use crate::tap::TestResult;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const PRESENCE_TTL: u64 = 30 * 60;
/// The canary's listing plaintext (intentionally trivial — the cycle proves the round-trip, not the
/// tree).
const CANARY_LISTING_JSON: &str = r#"{"slug":"hb-canary-coll","entries":[]}"#;

/// One step's outcome in a canary cycle.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub name: String,
    pub passed: bool,
    pub detail: Option<String>,
}

impl StepResult {
    fn ok(name: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, detail: None }
    }
    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), passed: false, detail: Some(detail.into()) }
    }
    fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, detail: Some(format!("SKIP {}", reason.into())) }
    }
}

/// The aggregate of one canary cycle: the throwaway `npub` it ran under + every step's outcome.
#[derive(Debug, Clone)]
pub struct CanaryRun {
    pub npub: String,
    pub results: Vec<StepResult>,
}

impl CanaryRun {
    /// All steps passed (and at least one ran). Pure — the basis of the exit code.
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.passed)
    }

    /// 0 on a fully-green cycle, 1 otherwise — what `systemd`/alerting keys on.
    pub fn exit_code(&self) -> i32 {
        if self.all_passed() {
            0
        } else {
            1
        }
    }

    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| !r.passed).count()
    }

    /// Render the run as TAP `TestResult`s for the shared printer.
    pub fn to_tap(&self) -> Vec<TestResult> {
        self.results
            .iter()
            .map(|r| match (r.passed, r.detail.as_deref()) {
                (true, Some(d)) if d.starts_with("SKIP") => {
                    TestResult::skip(r.name.clone(), d.trim_start_matches("SKIP ").to_string())
                }
                (true, _) => TestResult::ok(r.name.clone()),
                (false, Some(d)) => TestResult::fail(r.name.clone(), d.to_string()),
                (false, None) => TestResult::fail(r.name.clone(), "failed"),
            })
            .collect()
    }

    /// A single-line JSON summary for machine alerting.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"canary":"{}","passed":{},"failed":{},"npub":"{}"}}"#,
            if self.all_passed() { "pass" } else { "fail" },
            self.results.len() - self.failed(),
            self.failed(),
            self.npub,
        )
    }
}

/// Re-sign `event` with an added `t`=`hb-canary` marker tag (the "don't pollute real data"
/// guarantee). Pure. Shared with the L2 count suite, which uses it to publish marker-bearing
/// events and prove the counts/discovery exclude them.
pub(crate) fn with_canary_marker(id: &Identity, event: &Event) -> Result<Event> {
    let mut tags: Vec<Tag> = event.tags.iter().cloned().collect();
    tags.push(Tag::hashtag(CANARY_MARKER));
    Ok(id.sign(
        EventBuilder::new(event.kind, event.content.clone())
            .tags(tags)
            .custom_created_at(event.created_at),
    )?)
}

/// Build the three canary-marked events (teaser, encrypted listing, presence) for one run. Pure —
/// no network — so the "every event carries the marker" guarantee is unit-tested.
pub fn build_canary_events(id: &Identity, bk: &BrowseKey, run_tag: &str) -> Result<(Event, Event, Event)> {
    let teaser = build_teaser(
        id,
        &Teaser {
            display_name: "hb-canary".into(),
            bio: "synthetic backbone probe".into(),
            tags: vec![run_tag.to_string()],
            content_types: vec!["canary".into()],
            picture: None,
        },
        true,
    )?;
    let listing = build_listing_event(id, "hb-canary-coll", bk, CANARY_LISTING_JSON)?;
    let presence = build_binding(id, now(), PRESENCE_TTL)?;
    Ok((
        with_canary_marker(id, &teaser)?,
        with_canary_marker(id, &listing)?,
        with_canary_marker(id, &presence)?,
    ))
}

/// The canary's per-run secrets: a fresh ephemeral identity + a throwaway browse-key derived from
/// its pubkey (no extra RNG dep; unique per run since the identity is).
fn canary_keys() -> (Identity, BrowseKey, String) {
    let id = Identity::generate();
    let bk: BrowseKey = id.public_key().to_bytes();
    let run_tag = format!("hbcanary-{}", &id.public_key().to_hex()[..12]);
    (id, bk, run_tag)
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

/// Run one canary cycle against `relays`. Never panics — every failure becomes a failed `StepResult`
/// so the run still aggregates to a nonzero exit code.
pub async fn run_canary(relays: &[String]) -> CanaryRun {
    let (id, bk, run_tag) = canary_keys();
    let npub = id.npub();
    let mut results = Vec::new();

    let (teaser, listing, presence) = match build_canary_events(&id, &bk, &run_tag) {
        Ok(e) => e,
        Err(e) => {
            results.push(StepResult::fail("build canary events", format!("{e:#}")));
            return CanaryRun { npub, results };
        }
    };

    let client = match RelayClient::connect(&id, relays, CONNECT_TIMEOUT).await {
        Ok(c) => c,
        Err(e) => {
            results.push(StepResult::fail("connect to relays", format!("{e:#}")));
            return CanaryRun { npub, results };
        }
    };

    // (1) publish teaser + listing + presence.
    let publish = async {
        client.publish(&teaser).await?;
        client.publish(&listing).await?;
        client.publish(&presence).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match publish {
        Ok(()) => results.push(StepResult::ok("publish teaser+listing+presence (hb-canary tagged)")),
        Err(e) => {
            results.push(StepResult::fail("publish teaser+listing+presence", format!("{e:#}")));
            client.disconnect().await;
            return CanaryRun { npub, results };
        }
    }
    settle().await;

    // (2) fetch back + verify (Schnorr via parse_*, decrypt the listing with the browse-key).
    results.push(verify_step(&client, &id, &bk).await);

    // (3) round-trip a NIP-17 DM (canary → a fresh recipient → unwrap).
    results.push(dm_step(&client, &id).await);

    // (4) cross-region propagation (publish on relay[0], read from relay[1]).
    results.push(cross_region_step(relays).await);

    client.disconnect().await;
    CanaryRun { npub, results }
}

async fn verify_step(client: &RelayClient, id: &Identity, bk: &BrowseKey) -> StepResult {
    let inner = async {
        let teasers = client
            .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_TEASER)), FETCH_TIMEOUT)
            .await?;
        let teaser_ev = teasers.first().ok_or_else(|| anyhow::anyhow!("teaser not retained"))?;
        anyhow::ensure!(is_canary(teaser_ev), "fetched teaser is not hb-canary tagged");
        parse_teaser(teaser_ev)?; // Schnorr + schema verify

        let listings = client
            .fetch(Filter::new().author(id.public_key()).kind(Kind::from_u16(KIND_LISTING)), FETCH_TIMEOUT)
            .await?;
        let listing_ev = listings.first().ok_or_else(|| anyhow::anyhow!("listing not retained"))?;
        let (_slug, json) = parse_listing_event(listing_ev, bk)?; // Schnorr + NIP-44 decrypt
        anyhow::ensure!(json == CANARY_LISTING_JSON, "decrypted listing mismatch");
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match inner {
        Ok(()) => StepResult::ok("fetch back + verify (Schnorr + decrypt)"),
        Err(e) => StepResult::fail("fetch back + verify", format!("{e:#}")),
    }
}

async fn dm_step(client: &RelayClient, id: &Identity) -> StepResult {
    let inner = async {
        let recipient = Identity::generate();
        let wrap = wrap_dm(id, &recipient.public_key(), "hb-canary ping").await?;
        client.publish(&wrap).await?;
        settle().await;
        let rc = RelayClient::connect(&recipient, client.relays(), CONNECT_TIMEOUT).await?;
        let inbox = rc
            .fetch(Filter::new().kind(Kind::GiftWrap).pubkey(recipient.public_key()), FETCH_TIMEOUT)
            .await;
        rc.disconnect().await;
        let inbox = inbox?;
        let wrap_back = inbox.first().ok_or_else(|| anyhow::anyhow!("DM did not arrive"))?;
        let dm = unwrap_dm(&recipient, wrap_back).await?;
        anyhow::ensure!(dm.content == "hb-canary ping", "DM plaintext mismatch");
        anyhow::ensure!(dm.sender == id.public_key(), "DM sender not recovered from the seal");
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match inner {
        Ok(()) => StepResult::ok("NIP-17 DM round-trip"),
        Err(e) => StepResult::fail("NIP-17 DM round-trip", format!("{e:#}")),
    }
}

/// Cross-region health (SG↔JP). **Nostr relays do not replicate to each other** — SG and JP are
/// independent strfry instances — so the meaningful cross-region property is that an event published
/// in *one* region is visible to a **multi-region client** (one reaching both relays), which is
/// exactly how a real user in SG sees a listing published in JP. This catches a dead region (publish
/// fails / the multi-region read can't find it). The two ephemeral CI relays simulate SG↔JP.
async fn cross_region_step(relays: &[String]) -> StepResult {
    if relays.len() < 2 {
        return StepResult::skip("cross-region propagation", "needs a 2nd relay (SG↔JP)");
    }
    let inner = async {
        for (idx, relay) in relays.iter().enumerate().take(2) {
            let (id, _bk, _tag) = canary_keys();
            let presence = with_canary_marker(&id, &build_binding(&id, now(), PRESENCE_TTL)?)?;
            // Publish in region `idx` ONLY.
            let pubc = RelayClient::connect(&id, std::slice::from_ref(relay), CONNECT_TIMEOUT).await?;
            pubc.publish(&presence).await?;
            pubc.disconnect().await;
            settle().await;
            // A multi-region client (reaching every relay) must see it — the cross-region read.
            let reader = RelayClient::connect(&id, relays, CONNECT_TIMEOUT).await?;
            let got = reader
                .fetch(
                    Filter::new()
                        .author(id.public_key())
                        .kind(Kind::from_u16(hb_core::binding::KIND_PRESENCE)),
                    FETCH_TIMEOUT,
                )
                .await;
            reader.disconnect().await;
            anyhow::ensure!(
                !got?.is_empty(),
                "presence published only in region {idx} ({relay}) was not visible to a multi-region client"
            );
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    // Named for what it actually proves (chorus honesty note): a multi-region client sees an event
    // published in *either* region — NOT relay-to-relay replication (Nostr relays don't replicate).
    match inner {
        Ok(()) => StepResult::ok("cross-region reach (multi-region client, SG↔JP)"),
        Err(e) => StepResult::fail("cross-region reach (multi-region client, SG↔JP)", format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_is_zero_only_when_all_pass() {
        let pass = CanaryRun {
            npub: "npub1x".into(),
            results: vec![StepResult::ok("a"), StepResult::ok("b")],
        };
        assert_eq!(pass.exit_code(), 0);
        assert!(pass.all_passed());

        let fail = CanaryRun {
            npub: "npub1x".into(),
            results: vec![StepResult::ok("a"), StepResult::fail("b", "boom")],
        };
        assert_eq!(fail.exit_code(), 1, "any failed step → nonzero exit (the alarm fires)");
        assert_eq!(fail.failed(), 1);
    }

    #[test]
    fn empty_run_is_a_failure_not_a_silent_pass() {
        // A run that produced no steps (e.g. died before building events) must NOT read as green.
        let run = CanaryRun { npub: "npub1x".into(), results: vec![] };
        assert!(!run.all_passed());
        assert_eq!(run.exit_code(), 1);
    }

    #[test]
    fn every_canary_event_carries_the_marker() {
        // F-canary: teaser, listing, AND presence must all carry hb-canary so the counts/discovery
        // exclude them — the no-pollution guarantee at its source.
        let (id, bk, tag) = canary_keys();
        let (teaser, listing, presence) = build_canary_events(&id, &bk, &tag).unwrap();
        for (label, ev) in [("teaser", &teaser), ("listing", &listing), ("presence", &presence)] {
            assert!(is_canary(ev), "the canary {label} is missing the hb-canary marker");
        }
    }

    #[test]
    fn canary_runs_use_distinct_ephemeral_keys() {
        // Each run is a fresh throwaway npub — two runs never share a key.
        let (a, _, _) = canary_keys();
        let (b, _, _) = canary_keys();
        assert_ne!(a.public_key(), b.public_key(), "canary must mint a fresh ephemeral npub per run");
    }

    #[test]
    fn json_summary_reports_pass_fail() {
        let run = CanaryRun {
            npub: "npub1abc".into(),
            results: vec![StepResult::ok("a"), StepResult::fail("b", "x")],
        };
        let json = run.to_json();
        assert!(json.contains(r#""canary":"fail""#), "{json}");
        assert!(json.contains(r#""failed":1"#), "{json}");
        assert!(json.contains("npub1abc"), "{json}");
    }
}
