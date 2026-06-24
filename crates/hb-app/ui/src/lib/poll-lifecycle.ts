// Poll-lifecycle view logic (M12 W1, Decision B). The connect-per-command storm was driven by
// background pollers hammering the relays while the window was hidden (tray/minimized) and by the
// 4 s DM poll. This pure seam decides, given a visibility flag passed IN (never read from jsdom
// `document.hidden`, which is flaky in tests), whether a poll should run and at what cadence.
//
// Rules:
//   - hidden window → the poll is **paused** (active=false): no relay churn against a window nobody
//     is looking at.
//   - visible window → the poll runs at its (backed-off) interval.

/** Backed-off DM poll cadence while the chat page is visible — was 4 s, the dominant connect source. */
export const DM_POLL_VISIBLE_MS = 15_000;
/** Layout nav-inbox poll cadence while visible. */
export const NAV_POLL_VISIBLE_MS = 20_000;
/** Online-count chip poll cadence while visible. */
export const ONLINE_POLL_VISIBLE_MS = 60_000;

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
