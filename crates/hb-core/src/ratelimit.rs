//! Relay-write pacing (ban-avoidance) — a **token bucket**, the pure arithmetic only.
//!
//! Public relays ban clients that flood them with writes. The single realistic flood Hoardbook can
//! produce is a large-collection publish: the M13 recursive split explodes one oversized listing
//! into many part events (`split.rs`), and a 100k-file catalogue publishes as hundreds of `publish`
//! calls back-to-back. Left ungoverned that reads as an attack to a relay → rate-limit → ban →
//! [[large_collection_intent_2026-07-11]]'s "relay citizenship" constraint is violated.
//!
//! **This is NOT the announce cooldown.** `topic::ANNOUNCE_MIN_INTERVAL_SECS` (1 hour) is a
//! per-channel *anti-spam* rule for one specific broadcast; applying an hour — or any fixed
//! min-interval — to *every* write would make the app unusable (owner ruling 2026-07-12). A token
//! bucket instead: a **generous burst** so ordinary interactive writes (publish your profile, send
//! a DM, publish one small listing, the 5-minute presence beacon) drain a single token and go out
//! **instantly**, and only a sustained flood — once the burst is spent — is paced down to a gentle
//! steady rate. Reads are never governed; this touches the write path only.
//!
//! **Pure, clock-injected.** Like [`crate::topic::announce_cooldown_remaining`], this crate supplies
//! only the refill/consume math (unit-tested without a clock); the monotonic clock + the async sleep
//! live in the caller (hb-net `RelayClient`, the un-bypassable publish chokepoint).

/// Burst capacity — the number of writes that go out with **zero pacing** from a full bucket.
/// Sized so any ordinary interactive action clears instantly (a profile/DM = 1 event; a small
/// listing = a handful of part events) and only a genuine flood ever touches the steady rate.
/// **OWNER-RATIFICATION DEFAULT** (2026-07-12): the *shape* (token bucket, not min-interval) is
/// ruled; the *value* may change before ship. Mirrors the `ANNOUNCE_MIN_INTERVAL_SECS` caveat.
pub const RELAY_WRITE_BURST: f64 = 24.0;

/// Steady-state refill — writes per second once the burst is drained. Gentle enough that a public
/// relay does not read a sustained large-collection publish as a flood, so a huge catalogue paces
/// smoothly instead of tripping a ban (a soft nudge toward the dedicated higher-cap relay that
/// [[large_collection_intent_2026-07-11]] plans for item 7). **OWNER-RATIFICATION DEFAULT** — value
/// may change before ship.
pub const RELAY_WRITE_REFILL_PER_SEC: f64 = 2.0;

/// A token-bucket rate limiter's pure state. `now` is **monotonic seconds** supplied by the caller
/// (e.g. `Instant::elapsed().as_secs_f64()`) — never wall-clock, so a system-clock jump cannot skew
/// pacing. Starts **full** (`tokens == capacity`) so a fresh client's first burst is unthrottled.
#[derive(Debug, Clone)]
pub struct RelayRateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    /// Monotonic seconds at the last refill. `None` until the first `try_acquire` anchors the clock,
    /// so the very first call refills by nothing (no free tokens from an arbitrary epoch offset).
    updated_at: Option<f64>,
}

impl RelayRateLimiter {
    /// A limiter with an explicit `capacity` (max burst) and `refill_per_sec` (steady rate), starting
    /// full. `capacity` is clamped to at least 1 token and `refill_per_sec` to a positive floor so a
    /// mis-set constant can never wedge the acquire loop (a zero refill would wait forever).
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        let capacity = capacity.max(1.0);
        Self {
            capacity,
            refill_per_sec: refill_per_sec.max(f64::MIN_POSITIVE),
            tokens: capacity,
            updated_at: None,
        }
    }

    /// The production limiter: [`RELAY_WRITE_BURST`] tokens refilling at [`RELAY_WRITE_REFILL_PER_SEC`].
    pub fn relay_writes() -> Self {
        Self::new(RELAY_WRITE_BURST, RELAY_WRITE_REFILL_PER_SEC)
    }

    /// Refill up to `now`, then **either** consume one token (returns `None` — send now) **or**, if
    /// under a full token, return `Some(seconds)` the caller must wait before retrying (nothing is
    /// consumed on a wait). The caller loops: sleep, call again — a token will have accrued.
    ///
    /// **Monotonic-safe:** elapsed time is clamped at `0`, so a `now` earlier than the last refill
    /// (a monotonic clock should never go backwards, but defensively) neither over-refills nor
    /// underflows — it simply adds no tokens, exactly like the saturating `announce_cooldown_remaining`.
    pub fn try_acquire(&mut self, now: f64) -> Option<f64> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            // Seconds to accrue the fractional shortfall back up to one whole token.
            Some((1.0 - self.tokens) / self.refill_per_sec)
        }
    }

    fn refill(&mut self, now: f64) {
        let elapsed = match self.updated_at {
            Some(prev) => (now - prev).max(0.0),
            None => 0.0, // first call only anchors the clock; no free accrual from the epoch offset
        };
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        // Advance the anchor forward only. On a rollback (`now` < the previous anchor) we must NOT
        // move it back to `now`: doing so would make the *next* forward tick measure elapsed from the
        // rolled-back point and over-credit the bucket for time already accounted (Chorus codex). The
        // production clock (`Instant::elapsed`) is monotonic so this only bites an injected clock, but
        // keeping the anchor monotonic is what makes the pure type genuinely clock-injection-robust.
        self.updated_at = Some(match self.updated_at {
            Some(prev) => prev.max(now),
            None => now,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_bucket_lets_the_whole_burst_through_instantly() {
        // The usability floor (owner ruling 2026-07-12): no interactive action is paced. A full
        // bucket admits `capacity` writes with zero wait — a small listing's handful of part events,
        // a profile publish, a DM all clear immediately.
        let mut rl = RelayRateLimiter::new(24.0, 2.0);
        for i in 0..24 {
            assert_eq!(rl.try_acquire(0.0), None, "burst token {i} must pass without pacing");
        }
    }

    #[test]
    fn the_write_after_the_burst_is_paced_not_dropped() {
        // Only once the burst is spent does pacing engage — and it *waits*, never rejects (a large
        // publish still completes, just spread out).
        let mut rl = RelayRateLimiter::new(2.0, 2.0);
        assert_eq!(rl.try_acquire(0.0), None);
        assert_eq!(rl.try_acquire(0.0), None);
        // Burst drained → the 3rd write must wait ~ 1 token / 2 per sec = 0.5s.
        let wait = rl.try_acquire(0.0).expect("must pace once the burst is gone");
        assert!((wait - 0.5).abs() < 1e-9, "expected ~0.5s, got {wait}");
    }

    #[test]
    fn tokens_refill_at_the_steady_rate_after_the_burst() {
        let mut rl = RelayRateLimiter::new(1.0, 2.0);
        assert_eq!(rl.try_acquire(0.0), None); // spend the only token
        assert!(rl.try_acquire(0.0).is_some(), "empty right after");
        // After 0.5s at 2/sec exactly one token has accrued → passes again.
        assert_eq!(rl.try_acquire(0.5), None, "one token back after 0.5s");
        assert!(rl.try_acquire(0.5).is_some(), "and empty again immediately");
    }

    #[test]
    fn refill_never_exceeds_capacity_no_matter_how_long_idle() {
        // A long idle must not bank unlimited tokens (which would let a later flood burst huge).
        let mut rl = RelayRateLimiter::new(3.0, 2.0);
        for _ in 0..3 {
            assert_eq!(rl.try_acquire(0.0), None);
        }
        // Idle an hour, then only `capacity` writes may burst — not 3600*2.
        for i in 0..3 {
            assert_eq!(rl.try_acquire(3600.0), None, "capped-burst token {i}");
        }
        assert!(rl.try_acquire(3600.0).is_some(), "the bucket refills to capacity, never above");
    }

    #[test]
    fn a_clock_that_goes_backwards_adds_no_tokens() {
        // Monotonic-safe (mirrors announce_cooldown_remaining's saturating rollback handling): a
        // `now` before the last refill neither over-refills nor panics — it just accrues nothing.
        let mut rl = RelayRateLimiter::new(1.0, 2.0);
        assert_eq!(rl.try_acquire(100.0), None); // anchor at t=100, spend the token
        assert!(rl.try_acquire(50.0).is_some(), "a rollback grants no free token");
        // Forward progress from the later anchor still refills normally.
        assert_eq!(rl.try_acquire(100.5), None, "0.5s past the t=100 anchor refills one token");
    }

    #[test]
    fn a_rollback_does_not_over_credit_the_next_forward_tick() {
        // Chorus (codex): if a rollback moved the anchor *backward*, the next forward `now` would
        // measure elapsed from the rolled-back point and over-credit — refilling the whole burst for
        // time already accounted. Capacity 24 (not 1) so the saturating cap can't mask the bug.
        let mut rl = RelayRateLimiter::new(24.0, 2.0);
        for _ in 0..24 {
            assert_eq!(rl.try_acquire(100.0), None); // drain the burst, anchored at t=100
        }
        assert!(rl.try_acquire(10.0).is_some(), "rolled back to t=10: still empty, no free tokens");
        // Only 0.5s of *real* forward progress from the t=100 anchor ⇒ ~1 token — NOT the 90s×2 a
        // rolled-back anchor would have granted (which would wrongly refill the entire burst).
        assert_eq!(rl.try_acquire(100.5), None, "the one genuinely-earned token");
        assert!(rl.try_acquire(100.5).is_some(), "and empty again — the burst was NOT over-credited");
    }

    #[test]
    fn degenerate_config_cannot_wedge_the_acquire_loop() {
        // A zero/negative refill or sub-1 capacity is floored so `try_acquire` always returns a
        // FINITE wait — the caller's sleep-and-retry loop can never hang on a bad constant.
        let mut rl = RelayRateLimiter::new(0.0, 0.0);
        assert_eq!(rl.try_acquire(0.0), None, "capacity floored to >=1 token");
        let wait = rl.try_acquire(0.0).expect("now empty");
        assert!(wait.is_finite(), "refill floored positive → finite wait, never an infinite sleep");
    }

    #[test]
    fn the_production_limiter_uses_the_ratified_defaults() {
        let mut rl = RelayRateLimiter::relay_writes();
        for _ in 0..(RELAY_WRITE_BURST as usize) {
            assert_eq!(rl.try_acquire(0.0), None);
        }
        assert!(rl.try_acquire(0.0).is_some(), "paces exactly after RELAY_WRITE_BURST writes");
    }
}
