//! QURATOR-164 build item 2 — the ASK THROTTLE (owner ruling 2026-09-04).
//!
//! *"at most 1 manifest ask/request per second, globally … That way we can stretch things over
//! minutes if we have to."* The burst being paced is the ask fan-out per fetch: "if you have 40
//! people who have it, sending 40 bursty messages on a relay is quite bad."
//!
//! Three load-bearing properties, all from the ruling:
//!
//! 1. **It DELAYS, it never DISCARDS.** A queue, not a drop — `acquire()` returns `()`, has no
//!    failure mode, and never refuses. This is what keeps it consistent with the no-caps ruling:
//!    nothing is skipped, it merely leaves slower.
//! 2. **One shared limiter, hoisted to MODULE scope.** A per-function static is never shared across
//!    callers (CLAUDE.md §7) and would silently give every call site its own budget. There is
//!    exactly one `ASK_THROTTLE` cell below; both ask commands go through it.
//! 3. **Chat/DM is NOT throttled** — 1/sec would make chat feel broken. Scope is manifest asks
//!    (`request_manifest` / `request_manifest_from` in `commands/chat.rs`) AND fetch requests
//!    (`redeem_manifest_ticket_with_progress` in `commands/fulfil.rs`) — the ruling's own wording;
//!    `send_message` and every other DM path never touch this module. Pinned by
//!    `chat_and_dm_paths_are_not_throttled` and `the_fetch_request_path_takes_a_slot_before_dialing`.
//!    On the fetch side the slot paces the DIAL, not the transfer: `acquire()` releases its lock
//!    before returning, so concurrent redemptions still overlap and only their initiations are
//!    spaced. Serialising multi-second transfers behind a 1/sec gate would be different, and
//!    unruled, behaviour.
//!
//! Shape: a pure, synchronous core ([`delay_until_slot`]) that computes the wait — no I/O, no
//! globals, no async — plus a thin shared wrapper ([`acquire`]) that holds ONE lock across the
//! sleep. Holding the lock across the sleep is what serializes concurrent callers: everyone who
//! arrives during a wait queues on the mutex and recomputes their delay against the grant the
//! caller before them just recorded. Release-then-sleep would let every waiter compute the same
//! zero delay and fire together — the entire feature lives in that distinction.

use std::sync::LazyLock;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// The global ask budget: **at most one manifest ask per second** (owner ruling 2026-09-04). This
/// is the only interval knob; per-call-site intervals would defeat the "one shared limiter" rule.
const ASK_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The ONE shared limiter (module scope — see the module doc). `LazyLock` because tokio's
/// `Mutex::new` is not `const` (same pattern as `FAILED_WRAPS` in `commands/chat.rs`).
static ASK_THROTTLE: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Pure core — how long a caller must wait, given the last grant and the current instant.
///
/// Returns `Duration::ZERO` when a full interval has already elapsed (or when there is no previous
/// grant), and the exact remainder otherwise. Never refuses, never panics: saturating arithmetic
/// throughout, so a `now` that is not later than `last_grant` (clock skew / paused-clock edge)
/// yields the FULL interval, never an underflow.
fn delay_until_slot(last_grant: Option<Instant>, now: Instant) -> Duration {
    let Some(last) = last_grant else {
        return Duration::ZERO;
    };
    let elapsed = now.saturating_duration_since(last);
    ASK_MIN_INTERVAL.saturating_sub(elapsed)
}

/// Take the next ask slot: wait out the remainder of the current interval, then record the grant.
///
/// **Never discards.** Returns `()` — there is no refusal path, no error, no drop; a caller that
/// arrives during a burst simply leaves one interval after its predecessor. The lock is held across
/// the sleep deliberately: that is what turns concurrent callers into a serialized queue.
pub(crate) async fn acquire() {
    let mut last_grant = ASK_THROTTLE.lock().await;
    let wait = delay_until_slot(*last_grant, Instant::now());
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    *last_grant = Some(Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MUTATION (P-10) — the edit goes in the `ASK_MIN_INTERVAL` const definition (the
    /// `Duration::from_secs(1)` initializer, above the tests): change it to
    /// `Duration::from_millis(500)` → this test reds. (Anchored to the const's initializer, not a
    /// text search — this file's own comments quote the ruling's "1 per second".)
    #[test]
    fn interval_is_one_second_per_the_owner_ruling() {
        // "at most 1 manifest ask/request per second, globally" — the ruling's number, pinned so a
        // tuning edit to the const is a visible decision, not a silent drift.
        assert_eq!(ASK_MIN_INTERVAL, Duration::from_secs(1));
    }

    /// MUTATION (P-10) — containing function `delay_until_slot` (resolve by function, not by text):
    /// change the `None` arm to `return ASK_MIN_INTERVAL` → this test reds (first call would wait
    /// a full second instead of firing immediately).
    #[test]
    fn first_ask_has_no_wait() {
        let now = Instant::now();
        assert_eq!(delay_until_slot(None, now), Duration::ZERO);
    }

    /// MUTATION (P-10) — in `delay_until_slot`, return `ASK_MIN_INTERVAL` unconditionally (ignore
    /// the elapsed time) → this test reds (2s elapsed must mean no wait).
    #[test]
    fn full_interval_elapsed_has_no_wait() {
        let now = Instant::now();
        assert_eq!(
            delay_until_slot(Some(now - Duration::from_secs(2)), now),
            Duration::ZERO
        );
    }

    /// MUTATION (P-10) — in `delay_until_slot`, subtract the elapsed time from a different interval
    /// (`Duration::from_secs(2).saturating_sub(elapsed)`) → this test reds (1700ms ≠ 700ms).
    #[test]
    fn partial_interval_waits_the_exact_remainder() {
        let now = Instant::now();
        assert_eq!(
            delay_until_slot(Some(now - Duration::from_millis(300)), now),
            Duration::from_millis(700)
        );
    }

    /// MUTATION (P-10) — in `delay_until_slot`, swap the subtraction operands:
    /// `last.saturating_duration_since(now)` instead of `now.saturating_duration_since(last)`.
    /// The skew case (`now` earlier than the grant) then computes 5s elapsed instead of 0s →
    /// remainder underflows to `Duration::ZERO` instead of the full interval → this test reds on
    /// the second assert. (Checked against tokio 1.53.1 source: BOTH `duration_since` and the
    /// `Sub`-for-`Instant` impl also saturate, so "swap in the panicking variant" is NOT a valid
    /// anchor — no panicking variant exists. The operand swap is the edit that actually reds.)
    #[test]
    fn clock_not_later_than_grant_yields_the_full_interval() {
        let now = Instant::now();
        // Equal instants: zero elapsed, full interval remaining.
        assert_eq!(delay_until_slot(Some(now), now), ASK_MIN_INTERVAL);
        // `now` EARLIER than the grant (skew / a caller reading the clock before its predecessor):
        // must saturate to zero elapsed → full interval. Never underflow, never panic.
        assert_eq!(
            delay_until_slot(Some(now + Duration::from_secs(5)), now),
            ASK_MIN_INTERVAL
        );
    }

    /// The ruling's core property at the wrapper: concurrent callers SERIALIZE through the one
    /// limiter — at most one ask per interval — and NONE of them is discarded (all four get
    /// through; "it delays, it never discards"). Paused clock + tokio auto-advance makes each
    /// one-second hop instantaneous, so this runs in microseconds, not seconds.
    ///
    /// MUTATION (P-10) — two anchors, both inside `acquire`:
    /// 1. **Release-then-sleep**: scope the guard so it drops before `tokio::time::sleep` (compute
    ///    the delay, release the lock, sleep, re-lock to record). Every waiter then reads the same
    ///    pre-grant state, computes the same delay, and fires together → two timestamps land inside
    ///    one interval → the spacing assert reds.
    /// 2. **No record**: delete the `*last_grant = Some(...)` write. After the first caller
    ///    everyone reads `None`, waits zero, and all four fire at the same instant → reds the same
    ///    assert.
    #[tokio::test(start_paused = true)]
    async fn concurrent_asks_serialize_through_the_one_limiter() {
        const CALLERS: usize = 4;
        let fired: std::sync::Arc<std::sync::Mutex<Vec<Instant>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for _ in 0..CALLERS {
            let fired = fired.clone();
            handles.push(tokio::spawn(async move {
                acquire().await;
                fired.lock().unwrap().push(Instant::now());
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let mut times = fired.lock().unwrap().clone();
        // "It DELAYS, it never DISCARDS": nobody was dropped or refused — every caller completed.
        assert_eq!(
            times.len(),
            CALLERS,
            "the throttle queues, it never discards"
        );
        times.sort();
        for w in times.windows(2) {
            assert!(
                w[1] - w[0] >= ASK_MIN_INTERVAL,
                "at most one ask per second, globally — got {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }

    /// **The scope fence** — chat/DM is NOT throttled (owner ruling 2026-09-04: 1/sec would make
    /// chat feel broken). The ONLY call sites of the limiter in the whole of `chat.rs` are the two
    /// manifest-ask commands; `send_message` and every other DM path must have none. A count of
    /// exactly 2 plus containment in the two named regions covers every other function in the file
    /// at once — an acquire smuggled into ANY path (not just `send_message`) breaks the count.
    ///
    /// Region resolution is by containing function (`fn_region` slices from the real signature to
    /// the next line-start signature), never by bare text: this file's own assertion literals echo
    /// the strings it looks for, and comments are stripped, so only production code can satisfy it.
    ///
    /// MUTATION (P-10) — two anchors, both in `commands/chat.rs`:
    /// 1. **Over-apply**: inside `send_message` (or any other DM path in `chat.rs`), insert
    ///    `crate::ask_throttle::acquire().await;` → the count assert reds (3 ≠ 2), and the
    ///    `send_message` region assert reds.
    /// 2. **Under-apply**: delete the acquire from `request_manifest` or `request_manifest_from_inner`
    ///    → the count assert reds (1 ≠ 2) and that command's region assert reds (0 ≠ 1).
    #[test]
    fn chat_and_dm_paths_are_not_throttled() {
        let src = include_str!("commands/chat.rs");
        // Comments stripped, so documenting the rule cannot satisfy it.
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            code.matches("crate::ask_throttle::acquire").count(),
            2,
            "exactly the two ask commands may take the throttle — no other path in chat.rs"
        );
        let rm = fn_region(&code, "pub async fn request_manifest(");
        assert_eq!(
            rm.matches("crate::ask_throttle::acquire").count(),
            1,
            "the owner-path ask must go through the shared limiter exactly once"
        );
        // The carrier-4 ask body lives in the INNER since QURATOR-164 item 3 extracted it so the
        // background fetch driver could call the production path instead of hand-rolling a copy.
        // The guard follows the behaviour, not the name.
        let rmf = fn_region(&code, "pub(crate) async fn request_manifest_from_inner(");
        assert_eq!(
            rmf.matches("crate::ask_throttle::acquire").count(),
            1,
            "the carrier-4 ask must go through the shared limiter exactly once"
        );
        assert_eq!(
            fn_region(&code, "pub async fn send_message(")
                .matches("crate::ask_throttle::acquire")
                .count(),
            0,
            "a chat send must not be throttled — 1/sec would make chat feel broken"
        );
    }

    /// The acquire must sit BEFORE the relay write in both ask commands — the ruling paces the
    /// outbound relay traffic, so an acquire moved past the send would pace nothing.
    ///
    /// MUTATION (P-10) — in `commands/chat.rs`, move the `crate::ask_throttle::acquire().await;`
    /// line in `request_manifest` (or `request_manifest_from_inner`) to AFTER the `send_dm_inner(…)`
    /// `.map_err(cmd_err)?;` → the index assert for that region reds.
    #[test]
    fn ask_acquire_precedes_the_relay_write_in_both_ask_commands() {
        let src = include_str!("commands/chat.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for sig in [
            "pub async fn request_manifest(",
            "pub(crate) async fn request_manifest_from_inner(",
        ] {
            let region = fn_region(&code, sig);
            let at = region
                .find("crate::ask_throttle::acquire")
                .unwrap_or_else(|| {
                    panic!("{sig} must take the shared ask throttle before its relay write")
                });
            let send = region
                .find("send_dm_inner(")
                .unwrap_or_else(|| panic!("{sig} must send its ask via send_dm_inner"));
            assert!(
                at < send,
                "{sig}: the throttle acquire must precede the relay write — it paces the outbound DM"
            );
        }
    }

    /// The FETCH-REQUEST half of the ruling's scope ("manifest asks and fetch requests"). Pins
    /// that the one production fetch initiation takes a slot, and takes it BEFORE dialing — a
    /// throttle applied after the fetch would pace nothing at all.
    ///
    /// MUTATION (P-10) — resolved by containing function, never bare text, since `fulfil.rs` has
    /// near-identical lines elsewhere: in `redeem_manifest_ticket_with_progress`, either delete
    /// the `crate::ask_throttle::acquire().await;` line (the count assert reds) or move it below
    /// the `fetch_manifest_with_progress(` call (the ordering assert reds).
    #[test]
    fn the_fetch_request_path_takes_a_slot_before_dialing() {
        let src = include_str!("commands/fulfil.rs");
        // Comments stripped, so documenting the rule cannot satisfy it — same construction as
        // `chat_and_dm_paths_are_not_throttled`, and for the same reason.
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            code.matches("crate::ask_throttle::acquire").count(),
            1,
            "exactly one fetch initiation in fulfil.rs may take the throttle"
        );
        let region = fn_region(&code, "pub(crate) async fn redeem_manifest_ticket_with_progress(");
        let acquire = region
            .find("crate::ask_throttle::acquire")
            .expect("the redeem path must go through the shared limiter");
        let fetch = region
            .find("fetch_manifest_with_progress(")
            .expect("the redeem path must still dial");
        assert!(
            acquire < fetch,
            "the throttle must precede the dial — it paces the INITIATION of a fetch, and a slot \
             taken afterwards would pace nothing"
        );
    }

    /// Slice `code` from the (first, i.e. production) occurrence of `sig` to the next line-start
    /// function signature — the containing-function resolution the P-10 anchor rule requires.
    fn fn_region<'a>(code: &'a str, sig: &str) -> &'a str {
        let start = code
            .find(sig)
            .unwrap_or_else(|| panic!("signature {sig} must exist"));
        let rest = &code[start..];
        let end = [
            "\npub async fn ",
            "\npub(crate) async fn ",
            "\nasync fn ",
            "\npub fn ",
            "\nfn ",
        ]
        .iter()
        .filter_map(|m| rest.find(*m))
        .min()
        .unwrap_or(rest.len());
        &rest[..end]
    }
}
