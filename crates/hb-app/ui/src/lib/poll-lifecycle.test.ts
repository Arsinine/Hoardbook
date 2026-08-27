import { readFileSync } from 'node:fs';
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
		expect(ONLINE_POLL_VISIBLE_MS).toBe(20_000);
	});

	// devtest 2026-08-26 item 4 — "The online indicator and hoarders online count in Contacts do not
	// automatically update unless i switch pages and come back."
	//
	// `online_count` hands back the LAST COMPLETED refresh and only then spawns the next one. So the
	// UI's poll period and the backend's throttle are not independent knobs: if they are equal, every
	// poll reads a value produced by the previous poll and the screen sits a full cycle behind
	// (60–120 s with both at 60 s — exactly the "doesn't update on its own" the owner reported).
	// This is the property that must hold, and it spans two languages, so the Rust constant is
	// PARSED rather than restated — a restated copy would keep passing after online.rs moved.
	it('the online poll reads FASTER than the backend refreshes (item 4 — no phase lock)', () => {
		const rs = readFileSync(
			new URL('../../../src/commands/online.rs', import.meta.url),
			'utf8',
		);
		const m = rs.match(/const REFRESH_INTERVAL: Duration = Duration::from_secs\((\d+)\);/);
		expect(m, 'REFRESH_INTERVAL not found in online.rs — this guard has gone vacuous').not.toBeNull();
		const refreshMs = Number(m![1]) * 1_000;
		expect(refreshMs).toBeGreaterThan(0);
		// Strictly faster, not merely different: equal is the phase-lock, slower is worse than it.
		expect(ONLINE_POLL_VISIBLE_MS).toBeLessThan(refreshMs);
	});

	// The other half of the same trade: reading faster is only free because the relay query is
	// throttled on the BACKEND. If that throttle ever moved into the frontend interval, a 20 s poll
	// would triple relay writes. Pin where the throttle lives.
	it('the relay-query throttle lives in online.rs, not in the poll interval', () => {
		const rs = readFileSync(
			new URL('../../../src/commands/online.rs', import.meta.url),
			'utf8',
		);
		expect(rs).toMatch(/is_stale\(c\.last_attempt, Instant::now\(\), REFRESH_INTERVAL\)/);
		// …and the slot is claimed under the same write lock that observed staleness, so N concurrent
		// polls still produce at most one relay query.
		const fn = rs.slice(rs.indexOf('pub async fn online_count'), rs.indexOf('// No cache yet'));
		expect(fn).toMatch(/c\.last_attempt = Some\(Instant::now\(\)\);/);
	});
});
