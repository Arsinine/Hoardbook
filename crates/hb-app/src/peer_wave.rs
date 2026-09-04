//! QURATOR-164 build item 3 — PEER WAVE SELECTION, the pure core (owner rulings 2026-09-04).
//!
//! Which peer do we ask next for a manifest, and when do we stop asking? The ruled policy:
//! *"If the author and a carrier are both online, fetch from whichever has a free download slot;
//! if the author is offline and a carrier is up, fetch from the carrier. Exponential backoff,
//! 3 attempts against the same peer before moving on."*
//!
//! Four load-bearing properties, all from the rulings:
//!
//! 1. **A free slot is DISCOVERED BY ASKING, never advertised.** Nothing carries load in presence
//!    and adding it would be a new public signal about node state. So this module never reads a
//!    "capacity" field — it emits a wave of asks and lets the first answer win. A busy peer simply
//!    does not answer, and the backoff below handles it.
//! 2. **The wave is BOUNDED, the candidate set is NOT.** Ask 2-3 peers per wave, widen only if
//!    nobody answers. Nobody is excluded from the candidate set, they are merely asked later —
//!    which is what keeps this consistent with the no-caps ruling. [`WAVE_MAX_PEERS`] is the
//!    enforced knob; the lower bound of the ruled "2-3" is emergent rather than separately
//!    enforced (a wave takes every ready peer up to the cap, so it is only smaller than 2 when
//!    fewer than 2 peers are actually ready — waiting to batch a bigger wave would add delay for
//!    no benefit).
//! 3. **The author is the TERMINAL fallback, not the first choice.** Spreading load off the author
//!    is the entire reason this module exists, so the author is consulted only once every carrier
//!    is exhausted. It is still a real destination — [`WaveAction::FallBackToAuthor`] — because a
//!    fetch that gave up while the author was reachable would be a worse outcome than the load.
//! 4. **Give-up is bounded and explicit.** Without a terminal state a fetch can hang across many
//!    dead peers. [`WaveAction::GiveUp`] is reached only after the author itself is exhausted.
//!
//! Shape: one pure, synchronous decision function ([`next_action`]) — no I/O, no globals, no
//! async, and **no clock read of its own**. `now` arrives as a parameter, which is what makes
//! every branch below testable without sleeping. The driver that calls this on a schedule, issues
//! the asks through [`crate::ask_throttle::acquire`] and watches fingerprints is a separate slice;
//! this module deliberately knows nothing about transports, stores or DMs.

// QURATOR-164 item 3, slice 1 of 3: the policy core lands before the driver that calls it, so
// every item below is built-but-uncalled until slice 2 wires it into the fetch loop. REMOVE this
// allow in that slice — an allow that outlives its reason is how dead UI has shipped here before.
#![allow(dead_code)]

use std::time::Duration;

use tokio::time::Instant;

/// Attempts against ONE peer before it is exhausted and the policy moves on (owner ruling
/// 2026-09-04: *"3 attempts against the same peer before moving on"*).
const MAX_ATTEMPTS_PER_PEER: u32 = 3;

/// Upper bound on a single wave (the ruled "2-3 peers"). The candidate set itself is never
/// truncated — see property 2 in the module doc.
const WAVE_MAX_PEERS: usize = 3;

/// First backoff interval; each further attempt against the same peer doubles it.
const BACKOFF_BASE: Duration = Duration::from_secs(2);

/// One candidate source and everything the policy needs to know about it.
///
/// `attempts` counts asks already sent to this peer; `last_attempt` is when the most recent one
/// went out (`None` when it has never been asked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub npub: String,
    pub attempts: u32,
    pub last_attempt: Option<Instant>,
}

impl Candidate {
    /// A candidate that has never been asked.
    pub fn fresh(npub: impl Into<String>) -> Self {
        Self { npub: npub.into(), attempts: 0, last_attempt: None }
    }

    /// Has this candidate used up its attempt budget?
    fn is_exhausted(&self) -> bool {
        self.attempts >= MAX_ATTEMPTS_PER_PEER
    }

    /// How much longer this candidate must wait before it may be asked again.
    fn remaining_backoff(&self, now: Instant) -> Duration {
        let Some(last) = self.last_attempt else {
            return Duration::ZERO;
        };
        backoff_for(self.attempts).saturating_sub(now.saturating_duration_since(last))
    }

    /// Not exhausted, and its backoff has elapsed.
    fn is_ready(&self, now: Instant) -> bool {
        !self.is_exhausted() && self.remaining_backoff(now).is_zero()
    }
}

/// What the driver should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WaveAction {
    /// Ask these peers now — between 1 and [`WAVE_MAX_PEERS`] npubs, never empty.
    Ask(Vec<String>),
    /// Every live candidate is inside its backoff; wait this long and decide again.
    Wait(Duration),
    /// Every carrier is exhausted — go to the author.
    FallBackToAuthor,
    /// The author is exhausted too. Nothing left to try.
    GiveUp,
}

/// Exponential backoff for a peer that has already been asked `attempts` times.
///
/// `attempts == 0` means never asked, so there is nothing to back off from. Otherwise the interval
/// doubles per attempt: 2s, 4s, 8s… The shift is clamped so an out-of-range `attempts` (which the
/// cap should prevent, but this function does not get to assume) can never overflow.
fn backoff_for(attempts: u32) -> Duration {
    if attempts == 0 {
        return Duration::ZERO;
    }
    let doublings = (attempts - 1).min(16);
    BACKOFF_BASE.saturating_mul(1u32 << doublings)
}

/// Pure core — pick the next action, given the carriers, the author, and the current instant.
///
/// Order of preference is property 3 in the module doc: ready carriers first, then wait on a
/// backing-off carrier, then the author, then give up. Never panics and never reads the clock.
fn next_action(peers: &[Candidate], author: &Candidate, now: Instant) -> WaveAction {
    let wave: Vec<String> = peers
        .iter()
        .filter(|c| c.is_ready(now))
        .take(WAVE_MAX_PEERS)
        .map(|c| c.npub.clone())
        .collect();
    if !wave.is_empty() {
        return WaveAction::Ask(wave);
    }

    // No carrier is ready. If any is merely backing off, wait for the soonest rather than
    // burdening the author — the shortest remaining backoff is when the picture next changes.
    let soonest = peers
        .iter()
        .filter(|c| !c.is_exhausted())
        .map(|c| c.remaining_backoff(now))
        .min();
    if let Some(wait) = soonest {
        return WaveAction::Wait(wait);
    }

    // Every carrier is exhausted.
    if author.is_exhausted() {
        return WaveAction::GiveUp;
    }
    let author_wait = author.remaining_backoff(now);
    if author_wait.is_zero() {
        WaveAction::FallBackToAuthor
    } else {
        WaveAction::Wait(author_wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A candidate asked `attempts` times, the last of them `ago` before `now`.
    fn tried(npub: &str, attempts: u32, ago: Duration, now: Instant) -> Candidate {
        Candidate { npub: npub.into(), attempts, last_attempt: Some(now - ago) }
    }

    fn asked(action: &WaveAction) -> &[String] {
        match action {
            WaveAction::Ask(v) => v,
            other => panic!("expected an Ask, got {other:?}"),
        }
    }

    /// MUTATION (P-10) — the edit goes in the `MAX_ATTEMPTS_PER_PEER` const initializer (the
    /// `3` above the tests): change it to `4` → this test reds. (Anchored to the const's
    /// initializer by line, not by a text search — this file's prose also quotes the ruling's
    /// "3 attempts".)
    #[test]
    fn the_attempt_cap_is_the_ruled_three() {
        assert_eq!(MAX_ATTEMPTS_PER_PEER, 3);
    }

    /// MUTATION (P-10) — the edit goes in the `WAVE_MAX_PEERS` const initializer: change `3` to
    /// `5` → this test reds (all five ready peers would be asked at once).
    #[test]
    fn a_wave_is_capped_at_three_peers() {
        let now = Instant::now();
        let peers: Vec<Candidate> =
            (0..5).map(|i| Candidate::fresh(format!("npub{i}"))).collect();
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(asked(&action).len(), WAVE_MAX_PEERS);
        assert_eq!(asked(&action), ["npub0", "npub1", "npub2"]);
    }

    /// MUTATION (P-10) — in `next_action`, change the `.take(WAVE_MAX_PEERS)` on the wave
    /// iterator to `.take(1)` → this test reds (two ready peers must both be asked; a wave of one
    /// is the concentration this module exists to avoid).
    #[test]
    fn a_wave_takes_every_ready_peer_when_fewer_than_the_cap() {
        let now = Instant::now();
        let peers = vec![Candidate::fresh("a"), Candidate::fresh("b")];
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(asked(&action), ["a", "b"]);
    }

    /// MUTATION (P-10) — in `Candidate::is_exhausted`, change `>=` to `>` → this test reds (a peer
    /// on exactly the cap would still be asked, giving it a fourth attempt).
    #[test]
    fn a_peer_is_exhausted_after_the_third_attempt() {
        let now = Instant::now();
        // Well past any backoff, so only the attempt cap can be keeping it out of the wave.
        let peers = vec![tried("spent", MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now)];
        assert_eq!(
            next_action(&peers, &Candidate::fresh("author"), now),
            WaveAction::FallBackToAuthor
        );
    }

    /// MUTATION (P-10) — in `backoff_for`, replace the shift expression `1u32 << doublings` with
    /// the constant `1` → this test reds (backoff becomes flat 2s instead of doubling).
    #[test]
    fn backoff_doubles_with_each_attempt() {
        assert_eq!(backoff_for(1), BACKOFF_BASE);
        assert_eq!(backoff_for(2), BACKOFF_BASE * 2);
        assert_eq!(backoff_for(3), BACKOFF_BASE * 4);
        // Strictly increasing is the property that matters; the exact values above are the knob.
        assert!(backoff_for(2) > backoff_for(1));
        assert!(backoff_for(3) > backoff_for(2));
    }

    /// MUTATION (P-10) — in `backoff_for`, delete the `attempts == 0` early return (let it fall
    /// through to the shift) → this test reds, because `0 - 1` underflows in debug and the call
    /// panics rather than returning zero.
    #[test]
    fn a_peer_never_asked_has_no_backoff() {
        assert_eq!(backoff_for(0), Duration::ZERO);
    }

    /// MUTATION (P-10) — in `Candidate::remaining_backoff`, swap the subtraction operands to
    /// `now.saturating_duration_since(last).saturating_sub(backoff_for(self.attempts))` → this
    /// test reds (the remainder collapses to zero and the peer is asked early).
    #[test]
    fn a_peer_inside_its_backoff_is_waited_on_not_asked() {
        let now = Instant::now();
        // One attempt 500ms ago ⇒ 2s backoff, 1.5s left to run.
        let peers = vec![tried("cooling", 1, Duration::from_millis(500), now)];
        assert_eq!(
            next_action(&peers, &Candidate::fresh("author"), now),
            WaveAction::Wait(Duration::from_millis(1500))
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the `soonest` combinator from `.min()` to
    /// `.max()` → this test reds (it would wait 3.5s for the slowest peer instead of 1.5s for the
    /// soonest, sitting idle while a peer was already askable).
    #[test]
    fn the_wait_is_the_soonest_peer_not_the_latest() {
        let now = Instant::now();
        let peers = vec![
            tried("slow", 2, Duration::from_millis(500), now), // 4s backoff, 3.5s left
            tried("soon", 1, Duration::from_millis(500), now), // 2s backoff, 1.5s left
        ];
        assert_eq!(
            next_action(&peers, &Candidate::fresh("author"), now),
            WaveAction::Wait(Duration::from_millis(1500))
        );
    }

    /// MUTATION (P-10) — in `next_action`, move the author block above the wave block (return
    /// `WaveAction::FallBackToAuthor` before computing `wave`) → this test reds. This is the pin
    /// on property 3: the author is the LAST resort, and going to them while a carrier is ready is
    /// exactly the load concentration QURATOR-164 exists to prevent.
    #[test]
    fn a_ready_carrier_is_preferred_over_the_author() {
        let now = Instant::now();
        let peers = vec![Candidate::fresh("carrier")];
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(asked(&action), ["carrier"]);
    }

    /// MUTATION (P-10) — in `next_action`, change the no-carriers author branch to return
    /// `WaveAction::GiveUp` unconditionally (drop the `author.is_exhausted()` test) → this test
    /// reds (a live author would be abandoned).
    #[test]
    fn the_author_is_the_fallback_once_every_carrier_is_exhausted() {
        let now = Instant::now();
        let peers = vec![
            tried("a", MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now),
            tried("b", MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now),
        ];
        assert_eq!(
            next_action(&peers, &Candidate::fresh("author"), now),
            WaveAction::FallBackToAuthor
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the author-exhausted branch to return
    /// `WaveAction::FallBackToAuthor` instead of `WaveAction::GiveUp` → this test reds (the fetch
    /// would loop on an author that has already refused three times, with no terminal state).
    #[test]
    fn give_up_only_once_the_author_is_exhausted_too() {
        let now = Instant::now();
        let peers = vec![tried("a", MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now)];
        let author = tried("author", MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now);
        assert_eq!(next_action(&peers, &author, now), WaveAction::GiveUp);
    }

    /// MUTATION (P-10) — in `next_action`, change the wave's `.filter(|c| c.is_ready(now))` to
    /// `.filter(|c| !c.is_exhausted())` → this test reds (the backing-off peer would be asked
    /// immediately, defeating the backoff entirely).
    #[test]
    fn an_exhausted_peer_and_a_cooling_peer_are_distinguished() {
        let now = Instant::now();
        let peers = vec![
            tried("spent", MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now),
            tried("cooling", 1, Duration::from_millis(500), now),
        ];
        // The cooling peer is still live, so we wait for it rather than falling to the author.
        assert_eq!(
            next_action(&peers, &Candidate::fresh("author"), now),
            WaveAction::Wait(Duration::from_millis(1500))
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the `if !wave.is_empty()` guard to
    /// `if wave.is_empty()` → this test reds (an empty candidate list would return
    /// `Ask([])`, telling the driver to send a wave to nobody).
    #[test]
    fn no_carriers_at_all_goes_straight_to_the_author() {
        let now = Instant::now();
        assert_eq!(
            next_action(&[], &Candidate::fresh("author"), now),
            WaveAction::FallBackToAuthor
        );
    }
}
