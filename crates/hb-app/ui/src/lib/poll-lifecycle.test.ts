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

	it('the DM cadence is backed off from the old 4 s (the dominant connect source)', () => {
		// The whole point of Decision B: the DM poll no longer hammers relays every 4 s.
		expect(DM_POLL_VISIBLE_MS).toBeGreaterThanOrEqual(10_000);
		expect(DM_POLL_VISIBLE_MS).not.toBe(4_000);
	});

	it('exposes the nav + online cadences', () => {
		expect(NAV_POLL_VISIBLE_MS).toBe(20_000);
		expect(ONLINE_POLL_VISIBLE_MS).toBe(60_000);
	});
});
