import { describe, expect, it } from 'vitest';
import {
	pollState,
	DM_POLL_VISIBLE_MS,
	NAV_POLL_VISIBLE_MS,
	ONLINE_POLL_VISIBLE_MS,
} from './poll-lifecycle.js';

describe('poll-lifecycle — visibility-gate + backoff (M12 W1, Decision B)', () => {
	it('pauses the poll when the window is hidden', () => {
		expect(pollState(false, DM_POLL_VISIBLE_MS).active).toBe(false);
	});

	it('resumes the poll when the window is shown', () => {
		const s = pollState(true, DM_POLL_VISIBLE_MS);
		expect(s.active).toBe(true);
		expect(s.intervalMs).toBe(DM_POLL_VISIBLE_MS);
	});

	it('the DM cadence hits the ≤2–3 s propagation target (devtest v0.12.4 #1)', () => {
		// Supersedes the M12 15 s back-off: safe to tighten now that each poll is a `since`-bounded
		// INCREMENTAL fetch on the persistent shared client + the local encrypted cache (v0.12.4 #2),
		// not the whole-mailbox pull that made the old 4 s cadence the dominant connect source. Still
		// visibility-gated (paused while hidden), so a fast cadence no longer hammers relays.
		expect(DM_POLL_VISIBLE_MS).toBeLessThanOrEqual(3_000);
	});

	it('exposes the nav + online cadences', () => {
		expect(NAV_POLL_VISIBLE_MS).toBe(20_000);
		expect(ONLINE_POLL_VISIBLE_MS).toBe(60_000);
	});
});
