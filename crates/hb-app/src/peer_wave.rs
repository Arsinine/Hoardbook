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
//! 3. **The author rides IN the wave, not after it** (owner ruling 2026-09-04): *"if the author and
//!    a carrier are both online, fetch from whichever has a free download slot."* A free slot is
//!    discovered by asking, so the author is asked ALONGSIDE the carriers and the first answer
//!    wins. ⚠ An earlier cut of this module made the author a terminal fallback reached only once
//!    every carrier was exhausted — strictly more load-spreading, but NOT what was ruled, and it
//!    paid up to three backoffs per carrier in latency before trying the one source guaranteed to
//!    hold the collection. Do not reinstate that ordering.
//! 4. **Give-up is bounded and explicit.** Without a terminal state a fetch can hang across many
//!    dead peers. [`WaveAction::GiveUp`] is reached only after the author itself is exhausted.
//!
//! Shape: one pure, synchronous decision function ([`next_action`]) — no I/O, no globals, no
//! async, and **no clock read of its own**. `now` arrives as a parameter, which is what makes
//! every branch below testable without sleeping. The driver that calls this on a schedule, issues
//! the asks through [`crate::ask_throttle::acquire`] and watches fingerprints is a separate slice;
//! this module deliberately knows nothing about transports, stores or DMs.

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
    /// Ask these sources now: up to [`WAVE_MAX_PEERS`] carriers, plus the author when ready.
    /// Never empty. Whoever answers first wins — that is how a free download slot is discovered.
    Ask(Vec<String>),
    /// Every live source is inside its backoff; wait this long and decide again.
    Wait(Duration),
    /// Every source, the author included, is exhausted. Nothing left to try.
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
/// The wave is up to [`WAVE_MAX_PEERS`] ready carriers **plus the author when the author is ready**
/// (property 3). Carriers are listed first, which expresses the load-spreading preference as
/// ORDERING rather than as exclusion — but the author is in the same wave, so a carrier that is
/// merely slow can never stall a fetch the author could have served at once.
///
/// Never panics and never reads the clock.
pub(crate) fn next_action(peers: &[Candidate], author: &Candidate, now: Instant) -> WaveAction {
    let mut wave: Vec<String> = peers
        .iter()
        .filter(|c| c.is_ready(now))
        .take(WAVE_MAX_PEERS)
        .map(|c| c.npub.clone())
        .collect();
    if author.is_ready(now) {
        wave.push(author.npub.clone());
    }
    if !wave.is_empty() {
        return WaveAction::Ask(wave);
    }

    // Nothing is ready. If any source — carrier or author — is merely backing off, wait for the
    // soonest: that is when the picture next changes. Only when every source has spent its attempt
    // budget is there nothing left to try.
    match peers
        .iter()
        .chain(std::iter::once(author))
        .filter(|c| !c.is_exhausted())
        .map(|c| c.remaining_backoff(now))
        .min()
    {
        Some(wait) => WaveAction::Wait(wait),
        None => WaveAction::GiveUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A candidate asked `attempts` times, the last of them `ago` before `now`.
    fn tried(npub: &str, attempts: u32, ago: Duration, now: Instant) -> Candidate {
        Candidate { npub: npub.into(), attempts, last_attempt: Some(now - ago) }
    }

    /// A source that has spent its whole attempt budget, long enough ago that only the cap can be
    /// keeping it out of a wave.
    fn spent(npub: &str, now: Instant) -> Candidate {
        tried(npub, MAX_ATTEMPTS_PER_PEER, Duration::from_secs(600), now)
    }

    fn asked(action: &WaveAction) -> &[String] {
        match action {
            WaveAction::Ask(v) => v,
            other => panic!("expected an Ask, got {other:?}"),
        }
    }

    /// MUTATION (P-10) — the edit goes in the `MAX_ATTEMPTS_PER_PEER` const initializer (the `3`
    /// above the tests): change it to `4` → this test reds. (Anchored to the const's initializer by
    /// line, not by a text search — this file's prose also quotes the ruling's "3 attempts".)
    #[test]
    fn the_attempt_cap_is_the_ruled_three() {
        assert_eq!(MAX_ATTEMPTS_PER_PEER, 3);
    }

    /// ⚠ THIS TEST REPLACED `a_ready_carrier_is_preferred_over_the_author`, WHICH PINNED THE WRONG
    /// RULING. That test asserted the author was NOT asked while a carrier was ready — a stricter
    /// ordering than the owner ruled. The ruling is *"if the author and a carrier are both online,
    /// fetch from whichever has a free download slot"*, and a free slot is discovered by asking, so
    /// both go in the same wave. The inversion is deliberate; do not "restore" the old assertion.
    ///
    /// MUTATION (P-10) — in `next_action`, delete the `if author.is_ready(now) { wave.push(...) }`
    /// block → this test reds (the author would be dropped from the wave and only reached once
    /// every carrier had exhausted, which is exactly the behaviour this replaced).
    #[test]
    fn the_author_is_asked_alongside_carriers_not_after_them() {
        let now = Instant::now();
        let peers = vec![Candidate::fresh("carrier")];
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(
            asked(&action),
            ["carrier", "author"],
            "both are asked in one wave; whoever has a free slot answers first"
        );
    }

    /// MUTATION (P-10) — the edit goes in the `WAVE_MAX_PEERS` const initializer: change `3` to
    /// `5` → this test reds (all five carriers would be asked at once).
    #[test]
    fn a_wave_is_capped_at_three_carriers_plus_the_author() {
        let now = Instant::now();
        let peers: Vec<Candidate> = (0..5).map(|i| Candidate::fresh(format!("npub{i}"))).collect();
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(
            asked(&action),
            ["npub0", "npub1", "npub2", "author"],
            "the CAP is on carriers; the author rides along and is not counted against it"
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the `.take(WAVE_MAX_PEERS)` on the carrier
    /// iterator to `.take(1)` → this test reds (both ready carriers must be asked; a wave of one
    /// carrier is the concentration this module exists to avoid).
    #[test]
    fn a_wave_takes_every_ready_carrier_when_fewer_than_the_cap() {
        let now = Instant::now();
        let peers = vec![Candidate::fresh("a"), Candidate::fresh("b")];
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(asked(&action), ["a", "b", "author"]);
    }

    /// MUTATION (P-10) — in `Candidate::is_exhausted`, change `>=` to `>` → this test reds (a
    /// carrier sitting exactly on the cap would be asked again, giving it a fourth attempt).
    #[test]
    fn a_carrier_is_exhausted_after_the_third_attempt() {
        let now = Instant::now();
        let peers = vec![spent("spent", now)];
        let action = next_action(&peers, &Candidate::fresh("author"), now);
        assert_eq!(asked(&action), ["author"], "the spent carrier drops out; the author remains");
    }

    /// MUTATION (P-10) — in `backoff_for`, replace the shift expression `1u32 << doublings` with
    /// the constant `1` → this test reds (backoff becomes a flat 2s instead of doubling).
    #[test]
    fn backoff_doubles_with_each_attempt() {
        assert_eq!(backoff_for(1), BACKOFF_BASE);
        assert_eq!(backoff_for(2), BACKOFF_BASE * 2);
        assert_eq!(backoff_for(3), BACKOFF_BASE * 4);
        assert!(backoff_for(2) > backoff_for(1));
        assert!(backoff_for(3) > backoff_for(2));
    }

    /// MUTATION (P-10) — in `backoff_for`, delete the `attempts == 0` early return (let it fall
    /// through to the shift) → this test reds, because `0 - 1` underflows in debug and the call
    /// panics rather than returning zero.
    #[test]
    fn a_source_never_asked_has_no_backoff() {
        assert_eq!(backoff_for(0), Duration::ZERO);
    }

    /// MUTATION (P-10) — in `Candidate::remaining_backoff`, swap the subtraction operands to
    /// `now.saturating_duration_since(last).saturating_sub(backoff_for(self.attempts))` → this
    /// test reds (the remainder collapses to zero and the carrier is asked early).
    ///
    /// The author is spent here on purpose: with a ready author the wave would be non-empty and
    /// this test would measure nothing about the carrier's backoff.
    #[test]
    fn a_source_inside_its_backoff_is_waited_on_not_asked() {
        let now = Instant::now();
        // One attempt 500ms ago ⇒ 2s backoff, 1.5s left to run.
        let peers = vec![tried("cooling", 1, Duration::from_millis(500), now)];
        assert_eq!(
            next_action(&peers, &spent("author", now), now),
            WaveAction::Wait(Duration::from_millis(1500))
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the wait combinator from `.min()` to `.max()` →
    /// this test reds (it would idle 3.5s for the slowest source instead of 1.5s for the soonest,
    /// while a source was already askable).
    #[test]
    fn the_wait_is_the_soonest_source_not_the_latest() {
        let now = Instant::now();
        let peers = vec![
            tried("slow", 2, Duration::from_millis(500), now), // 4s backoff, 3.5s left
            tried("soon", 1, Duration::from_millis(500), now), // 2s backoff, 1.5s left
        ];
        assert_eq!(
            next_action(&peers, &spent("author", now), now),
            WaveAction::Wait(Duration::from_millis(1500))
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the wave's `.filter(|c| c.is_ready(now))` to
    /// `.filter(|c| !c.is_exhausted())` → this test reds (the cooling carrier would be asked
    /// immediately, defeating the backoff entirely).
    #[test]
    fn an_exhausted_carrier_and_a_cooling_one_are_distinguished() {
        let now = Instant::now();
        let peers = vec![spent("spent", now), tried("cooling", 1, Duration::from_millis(500), now)];
        assert_eq!(
            next_action(&peers, &spent("author", now), now),
            WaveAction::Wait(Duration::from_millis(1500)),
            "the cooling carrier is still live, so we wait for it rather than giving up"
        );
    }

    /// MUTATION (P-10) — in `next_action`, drop the `.chain(std::iter::once(author))` from the
    /// wait/give-up scan → this test reds on the second assert: with no carriers left, the live
    /// author would be invisible to the scan and the driver would give up while the one source
    /// guaranteed to hold the collection was merely backing off.
    #[test]
    fn give_up_only_once_every_source_including_the_author_is_exhausted() {
        let now = Instant::now();
        let peers = vec![spent("a", now)];
        assert_eq!(next_action(&peers, &spent("author", now), now), WaveAction::GiveUp);

        // Same carriers, but the author is merely cooling: there is still something to wait for.
        let cooling_author = tried("author", 1, Duration::from_millis(500), now);
        assert_eq!(
            next_action(&peers, &cooling_author, now),
            WaveAction::Wait(Duration::from_millis(1500)),
            "a backing-off author is not an exhausted one"
        );
    }

    /// MUTATION (P-10) — in `next_action`, change the `if !wave.is_empty()` guard to
    /// `if wave.is_empty()` → this test reds (with no carriers the author-only wave would be
    /// discarded and the driver would fall through to the wait/give-up scan).
    #[test]
    fn no_carriers_at_all_still_asks_the_author() {
        let now = Instant::now();
        let action = next_action(&[], &Candidate::fresh("author"), now);
        assert_eq!(asked(&action), ["author"]);
    }
}
