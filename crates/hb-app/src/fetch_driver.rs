//! QURATOR-164 item 3 — the BACKGROUND FETCH DRIVER (owner ruling 2026-09-04, option (b)).
//!
//! The ruling picked a background driver over a UI-driven ask wave: *"a background driver that
//! watches fingerprints and fetches unattended."* So nobody clicks. This loop notices that a
//! collection this node already holds has moved on to a new snapshot, and re-fetches it — asking
//! CARRIERS first and the author only as a last resort, which is the whole point of QURATOR-164
//! (spreading load off the author, since prefetch takes no caps).
//!
//! Four properties, each from a ruling and each with a pin below:
//!
//! 1. **The only trigger is a fingerprint change.** There is deliberately no manual retrigger, and
//!    *that absence IS the anti-nuisance-traffic control* — not a rate limit and not a cap. It
//!    self-heals: an offline peer leaves the inequality true, so the next poll simply tries again.
//! 2. **An unknown fingerprint is NOT a change.** If the author's published listing does not carry
//!    one (a pre-M16 listing, or a slug we hold but they no longer publish), the holding is left
//!    alone. Treating "unknown" as "stale" would make the driver re-ask forever against a peer
//!    that can never satisfy it — the exact nuisance traffic property 1 exists to prevent.
//! 3. **Carriers before the author.** Candidates are every contact EXCEPT the author, ordered by
//!    [`crate::peer_wave`], which asks 2-3 per wave, retries a peer 3 times with exponential
//!    backoff, and falls back to the author only once every carrier is exhausted.
//! 4. **Asks go through the production path.** Every ask this loop sends is
//!    [`request_manifest_from_inner`] — the same body the UI's ask calls. The WAN harness has three
//!    times re-implemented a command body and dropped one step from it; a driver with its own copy
//!    would be the fourth, and the dropped step would not surface until a live run.
//!
//! ⚠ **This is the BASELINE behaviour, not the opt-in one.** Refreshing something you already hold
//! is what "no manual retrigger" means, so it is not gated on `swarm_caching`. That switch governs
//! discovery-triggered auto-fetch of collections you have NEVER held, which is a separate slice.
//!
//! Shape: the decisions are pure functions ([`stale_holdings`], [`candidates_for`]) with the clock
//! and the network kept out of them; the loop is a thin shell that gathers inputs, calls them, and
//! sends. §5's integration half — a live two-machine run — is owed on QURATOR-164 and is not
//! discharged by any unit test here.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use crate::commands::browse::{contact_share_code, resolve_peer};
use crate::commands::chat::request_manifest_from_inner;
use crate::identity_state::SharedIdentity;
use crate::manifest_cache::CachedKey;
use crate::net::SharedRelay;
use crate::peer_wave::{next_action, Candidate, WaveAction};
use crate::store::DataStore;

/// How often the driver re-reads published fingerprints.
///
/// Minutes, not seconds: a snapshot changes when a human edits a collection, and every poll costs
/// one listing resolve per author held. The ask throttle paces what this produces, but the cheapest
/// relay traffic is the request never made.
const POLL_INTERVAL: Duration = Duration::from_secs(300);

/// A held collection whose author has published a newer snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleHolding {
    pub author_npub: String,
    pub slug: String,
    /// The fingerprint now published — what the ask names as seen, and what a reply must match.
    pub want_fingerprint: String,
}

/// Pure core — which of `held` have been superseded by `published`.
///
/// `published` is `(slug, current fingerprint)` as it arrives on the author's listing, so the
/// fingerprint is optional: a listing without the M16 marker carries none. **A missing fingerprint,
/// and a slug absent from the listing entirely, both mean NOT STALE** (property 2 in the module
/// doc) — the driver only ever acts on a fingerprint it can actually compare.
pub(crate) fn stale_holdings(held: &[CachedKey], published: &[(String, Option<String>)]) -> Vec<StaleHolding> {
    held.iter()
        .filter_map(|k| {
            let (_, current) = published.iter().find(|(slug, _)| *slug == k.slug)?;
            let current = current.as_ref()?;
            (current != &k.fingerprint).then(|| StaleHolding {
                author_npub: k.npub.clone(),
                slug: k.slug.clone(),
                want_fingerprint: current.clone(),
            })
        })
        .collect()
}

/// Pure core — who may be asked for `author`'s collection.
///
/// Every contact except the author themself (they are the fallback, held separately) and except
/// this node (asking yourself for what you already hold is a no-op that would still burn a wave
/// slot and a throttle slot).
pub(crate) fn candidates_for(contacts: &[String], author: &str, me: &str) -> Vec<Candidate> {
    contacts
        .iter()
        .filter(|n| n.as_str() != author && n.as_str() != me)
        .map(Candidate::fresh)
        .collect()
}

/// Per-collection ask state, in memory for the process lifetime.
struct AskState {
    peers: Vec<Candidate>,
    author: Candidate,
}

/// Record an attempt against `npub` in `state`, so the wave's backoff and 3-try cap advance.
fn note_attempt(state: &mut AskState, npub: &str, now: Instant) {
    let target = if state.author.npub == npub {
        Some(&mut state.author)
    } else {
        state.peers.iter_mut().find(|c| c.npub == npub)
    };
    if let Some(c) = target {
        c.attempts += 1;
        c.last_attempt = Some(now);
    }
}

/// The driver loop. Spawned once at startup; runs for the process lifetime.
pub(crate) async fn run_fetch_driver_loop(store: DataStore, live_npub: SharedIdentity, relay: SharedRelay) {
    // Keyed (author_npub, slug). In memory, like the auto-approve loop's caps: a restart re-reads
    // fingerprints and starts its attempt counting over, which is correct — a fresh process has no
    // reason to believe a peer that was down an hour ago still is.
    let mut states: HashMap<(String, String), AskState> = HashMap::new();

    tracing::info!(
        poll_secs = POLL_INTERVAL.as_secs(),
        "fetch driver: loop started (refetch on fingerprint change; carriers before author)"
    );

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        // Per-poll identity read, for the same reason the auto-approve loop does it: a fresh
        // install has no identity when this loop starts, and a one-shot snapshot would leave the
        // driver dead for the whole session.
        let (identity, own_npub) = {
            let guard = live_npub.read().await;
            let Some(id) = guard.as_ref() else { continue };
            (id.identity.clone(), id.npub())
        };

        let held = crate::manifest_cache::list(&store.manifest_cache_dir());
        if held.is_empty() {
            continue;
        }
        let Ok(contacts) = store.list_contacts() else { continue };
        let contact_npubs: Vec<String> = contacts.iter().map(|c| c.npub.clone()).collect();

        // Group holdings by author so each author's listing is resolved once per poll, not once
        // per collection held.
        let mut by_author: HashMap<String, Vec<CachedKey>> = HashMap::new();
        for k in held {
            by_author.entry(k.npub.clone()).or_default().push(k);
        }

        for (author_npub, keys) in by_author {
            let Some(contact) = contacts.iter().find(|c| c.npub == author_npub) else {
                // We hold a manifest from someone who is not a contact, so there is no share code
                // to resolve their listing with. Nothing to compare against; leave it alone.
                continue;
            };
            let Ok(share_code) = contact_share_code(contact) else { continue };
            let published: Vec<(String, Option<String>)> =
                match resolve_peer(&share_code, &identity, &store, &relay).await {
                    Ok(peer) => peer
                        .collections
                        .into_iter()
                        .map(|c| (c.collection.slug, c.snapshot_fingerprint))
                        .collect(),
                    Err(e) => {
                        tracing::debug!(author = %truncate(&author_npub), error = %e, "fetch driver: listing resolve failed");
                        continue;
                    }
                };

            for stale in stale_holdings(&keys, &published) {
                let key = (stale.author_npub.clone(), stale.slug.clone());
                let state = states.entry(key).or_insert_with(|| AskState {
                    peers: candidates_for(&contact_npubs, &stale.author_npub, &own_npub),
                    author: Candidate::fresh(stale.author_npub.clone()),
                });

                let now = Instant::now();
                // The wave already carries the author when the author is ready (owner ruling
                // 2026-09-04: whoever has a free slot answers first), so there is no separate
                // fallback branch to take here.
                let targets = match next_action(&state.peers, &state.author, now) {
                    WaveAction::Ask(sources) => sources,
                    // Backing off, or every source exhausted. Both are handled by simply not
                    // asking this poll — the next one re-evaluates, which is what makes the
                    // give-up bound self-healing rather than terminal.
                    WaveAction::Wait(_) | WaveAction::GiveUp => continue,
                };

                for target in targets {
                    // The production ask body — never a second copy (property 4). It takes the
                    // shared 1/sec throttle slot itself, so a wide wave leaves slowly rather than
                    // bursting.
                    match request_manifest_from_inner(
                        &target,
                        &stale.author_npub,
                        &stale.slug,
                        &stale.want_fingerprint,
                        None,
                        &identity,
                        &store,
                        &relay,
                    )
                    .await
                    {
                        Ok(()) => tracing::info!(
                            asked = %truncate(&target),
                            slug = %stale.slug,
                            "fetch driver: asked for a refreshed manifest"
                        ),
                        Err(e) => tracing::debug!(asked = %truncate(&target), error = %e, "fetch driver: ask failed"),
                    }
                    note_attempt(state, &target, now);
                }
            }
        }
    }
}

/// npubs are truncated in logs — a full one identifies a person, and this loop names every peer it
/// asks. Same treatment `send_dm_inner` gives a recipient.
fn truncate(npub: &str) -> String {
    npub.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(npub: &str, slug: &str, fp: &str) -> CachedKey {
        CachedKey { npub: npub.into(), slug: slug.into(), fingerprint: fp.into() }
    }

    /// MUTATION (P-10) — in `stale_holdings`, invert the comparison in the `.then(...)` guard to
    /// `(current == &k.fingerprint)` → this test reds (an unchanged holding would be refetched and
    /// a changed one ignored).
    #[test]
    fn a_changed_fingerprint_is_stale_and_an_unchanged_one_is_not() {
        let h = vec![held("npubA", "films", "fp-old"), held("npubA", "music", "fp-same")];
        let published = vec![
            ("films".to_string(), Some("fp-new".to_string())),
            ("music".to_string(), Some("fp-same".to_string())),
        ];
        assert_eq!(
            stale_holdings(&h, &published),
            vec![StaleHolding {
                author_npub: "npubA".into(),
                slug: "films".into(),
                want_fingerprint: "fp-new".into(),
            }],
            "only the collection whose snapshot moved on is refetched"
        );
    }

    /// MUTATION (P-10) — in `stale_holdings`, replace the `let current = current.as_ref()?;` line
    /// with `let current = current.as_ref().map(|s| s.as_str()).unwrap_or("").to_string(); let
    /// current = &current;` → an absent fingerprint compares unequal to anything and the holding
    /// reds as stale.
    ///
    /// This is property 2, and it is the difference between a quiet driver and one that re-asks a
    /// peer forever over a listing that can never satisfy it.
    #[test]
    fn a_listing_without_a_fingerprint_is_not_treated_as_a_change() {
        let h = vec![held("npubA", "films", "fp-old")];
        let published = vec![("films".to_string(), None)];
        assert!(stale_holdings(&h, &published).is_empty(), "unknown is not stale");
    }

    /// MUTATION (P-10) — in `stale_holdings`, change the slug lookup `.find(|(slug, _)| *slug ==
    /// k.slug)?` to `.next()?` → this test reds (a held slug would be compared against whichever
    /// collection happened to come first, and refetched under the wrong name).
    ///
    /// ⚠ NOT `.first()?` — that is a slice method and `published.iter()` is an iterator, so it does
    /// not compile, and a mutation that does not compile is NON-EVIDENCE rather than a proof.
    #[test]
    fn a_slug_the_author_no_longer_publishes_is_left_alone() {
        let h = vec![held("npubA", "retired", "fp-old")];
        let published = vec![("films".to_string(), Some("fp-new".to_string()))];
        assert!(stale_holdings(&h, &published).is_empty(), "nothing to compare against");
    }

    /// MUTATION (P-10) — in `candidates_for`, drop the `&& n.as_str() != me` clause of the filter
    /// → this test reds (the node would ask itself for what it already holds, burning a wave slot
    /// and a throttle slot on a guaranteed no-op).
    #[test]
    fn candidates_exclude_the_author_and_this_node() {
        let contacts = vec!["npubA".to_string(), "npubC".to_string(), "npubME".to_string()];
        let got = candidates_for(&contacts, "npubA", "npubME");
        assert_eq!(
            got.iter().map(|c| c.npub.as_str()).collect::<Vec<_>>(),
            ["npubC"],
            "the author is the fallback, not a carrier; and we never ask ourselves"
        );
    }

    /// MUTATION (P-10) — in `note_attempt`, delete the `c.attempts += 1;` line → this test reds
    /// (no peer would ever reach the 3-try cap, so the wave could never move on or fall back to
    /// the author).
    #[test]
    fn noting_an_attempt_advances_that_peer_only() {
        let now = Instant::now();
        let mut state = AskState {
            peers: vec![Candidate::fresh("npubC"), Candidate::fresh("npubD")],
            author: Candidate::fresh("npubA"),
        };
        note_attempt(&mut state, "npubC", now);
        assert_eq!(state.peers[0].attempts, 1, "the asked peer advances");
        assert_eq!(state.peers[1].attempts, 0, "its sibling does not");
        assert_eq!(state.author.attempts, 0, "and neither does the author");
    }

    /// MUTATION (P-10) — in `note_attempt`, change the author branch condition to
    /// `state.author.npub != npub` → this test reds (an author fallback attempt would be recorded
    /// against a carrier, or nowhere, so the author could never exhaust and `GiveUp` would be
    /// unreachable).
    #[test]
    fn an_author_fallback_attempt_is_recorded_against_the_author() {
        let now = Instant::now();
        let mut state = AskState {
            peers: vec![Candidate::fresh("npubC")],
            author: Candidate::fresh("npubA"),
        };
        note_attempt(&mut state, "npubA", now);
        assert_eq!(state.author.attempts, 1);
        assert_eq!(state.peers[0].attempts, 0);
    }

    /// MUTATION (P-10) — in the `POLL_INTERVAL` const initializer, change
    /// `Duration::from_secs(300)` to `Duration::from_secs(1)` → this test reds. A one-second
    /// fingerprint poll would resolve every held author's listing every second, which is the relay
    /// citizenship problem the ask throttle exists to avoid, arriving by a different door.
    #[test]
    fn the_poll_interval_is_minutes_not_seconds() {
        assert!(
            POLL_INTERVAL >= Duration::from_secs(60),
            "a fingerprint changes when a human edits a collection; polling faster only costs relays"
        );
    }

    /// MUTATION (P-10) — in `truncate`, change `.take(12)` to `.take(200)` → this test reds. A full
    /// npub in a log line identifies a person, and this loop logs every peer it asks.
    #[test]
    fn logged_npubs_are_truncated() {
        let full = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        assert_eq!(truncate(full).len(), 12);
        assert!(truncate(full).len() < full.len());
    }
}
