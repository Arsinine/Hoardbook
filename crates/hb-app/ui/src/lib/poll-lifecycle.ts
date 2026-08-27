// Poll-lifecycle view logic (M12 W1, Decision B). The connect-per-command storm was driven by
// background pollers hammering the relays while the window was hidden (tray/minimized) and by the
// 4 s DM poll. This pure seam decides, given a visibility flag passed IN (never read from jsdom
// `document.hidden`, which is flaky in tests), whether a poll should run and at what cadence.
//
// Rules:
//   - hidden window → the poll is **paused** (active=false): no relay churn against a window nobody
//     is looking at.
//   - visible window → the poll runs at its (backed-off) interval.

/** DM poll cadence while the chat page is visible. devtest v0.12.4 #1: tightened 15 s → 3 s to hit the
 *  ≤ 2–3 s propagation target. Safe now that each poll is a `since`-bounded INCREMENTAL fetch on the
 *  persistent shared client + the local encrypted cache (`get_messages`, v0.12.4 #2) — most polls
 *  return ~nothing and never re-decrypt seen wraps, so this is not the whole-mailbox pull that forced
 *  the M12 back-off. Still visibility-gated (paused while the window is hidden). */
export const DM_POLL_VISIBLE_MS = 3_000;
/** How many DM ticks between topic-channel refreshes. The open channel's 24 h-ephemeral posts are
 *  low-velocity, so refreshing it every DM tick would over-poll the relay — hold it near the old
 *  ~15 s cadence (5 × 3 s) while DMs poll fast. */
export const CHANNEL_REFRESH_EVERY_TICKS = 5;
/** Layout nav-inbox poll cadence while visible. */
export const NAV_POLL_VISIBLE_MS = 20_000;
/** Online-count chip / presence-pill poll cadence while visible.
 *
 *  **devtest 2026-08-26 item 4 — "The online indicator and hoarders online count in Contacts do not
 *  automatically update unless i switch pages and come back."** This was 60_000, and that was the
 *  bug. `online_count` is a *cached read*: it returns the last completed refresh immediately and
 *  only then spawns a new one, throttled server-side to one relay query per
 *  `online.rs::REFRESH_INTERVAL` (60 s). Polling at exactly that same 60 s phase-locked the two —
 *  every poll both triggered a refresh AND read the value from the previous poll's refresh, so the
 *  screen was permanently one full cycle behind and displayed data 60–120 s old. Switching pages
 *  re-mounted and re-read, which is why the owner saw it update only then.
 *
 *  The fix is to read faster than the backend refreshes, so a completed refresh reaches the screen
 *  promptly instead of waiting out a whole cycle. **This does not add relay traffic**: the throttle
 *  is entirely inside `online_count` (`is_stale(last_attempt, now, REFRESH_INTERVAL)`), so the relay
 *  is still queried at most once per 60 s no matter how often the UI polls, and the other half of
 *  the tick (`relay_status`) reads the persistent client's in-memory status map with no round trip.
 *  Pinned by `poll-lifecycle.test.ts` → "the online poll reads FASTER than the backend refreshes",
 *  which parses `REFRESH_INTERVAL` out of online.rs so the two cannot drift back into lockstep.
 *
 *  Worst-case display staleness: 120 s → ~80 s. The remaining ~60 s is `REFRESH_INTERVAL` itself,
 *  which is a relay-cost decision, not a UI one. */
export const ONLINE_POLL_VISIBLE_MS = 20_000;

/** Relay-health poll cadence while visible — deliberately NOT `ONLINE_POLL_VISIBLE_MS`.
 *
 *  These two shared one constant until item 4, by coincidence rather than by design, and the item-4
 *  speed-up must not ride along here (chorus review 2026-08-27, finding 4). `relay_status` looks
 *  cheap but is not free: it re-reads and re-parses settings.json on every call, and when every
 *  configured relay is in a dead terminal state `get_or_connect` attempts a RECONNECT on each call.
 *  Tripling that rate would have changed reconnect pressure under failure — a behaviour change
 *  nobody asked for, in the failure mode that can least afford it. Reachability changes slowly;
 *  60 s is right for it, and 20 s is right for reading a cached count. */
export const RELAY_HEALTH_POLL_VISIBLE_MS = 60_000;
/** Topic-announcement alert poll cadence (devtest #2) — announcements are rate-limited to 1/topic/hr,
 *  and this reads every joined topic's channel, so it runs slower than the DM/nav polls. */
export const ANNOUNCE_POLL_VISIBLE_MS = 90_000;

export interface PollState {
	/** Whether the poll should run in this visibility state. */
	active: boolean;
	/** The interval to use when active (ms). */
	intervalMs: number;
}

/** Pure poll-lifecycle decision (Decision B): a poll runs only while the window is `visible`, at
 *  `visibleIntervalMs`; when hidden it is paused. Visibility is an input so the gate is unit-tested
 *  without a DOM. */
export function pollState(visible: boolean, visibleIntervalMs: number): PollState {
	return { active: visible, intervalMs: visibleIntervalMs };
}
