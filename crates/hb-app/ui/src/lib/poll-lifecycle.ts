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
/** Online-count chip poll cadence while visible. */
export const ONLINE_POLL_VISIBLE_MS = 60_000;
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
