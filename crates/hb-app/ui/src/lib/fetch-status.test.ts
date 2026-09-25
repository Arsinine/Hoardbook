// QURATOR-335 — the per-collection full-list status (approved design, states 1–7). Pure: every case
// drives `deriveFetchStatus` with the inputs production reads (ask trace, progress event, flag).
import { describe, it, expect } from 'vitest';
import { deriveFetchStatus, FETCH_DIAL_CAP } from './fetch-status.js';
import type { ManifestAsk } from './api.js';

const NOW = new Date('2026-09-25T14:00:00Z');
const ask = (over: Partial<ManifestAsk> = {}): ManifestAsk => ({
	fingerprint_seen: 'fp',
	sent_at: '2026-09-25T13:53:00Z',
	nonce: 'n',
	...over,
});

describe('deriveFetchStatus', () => {
	it('never asked, nothing in flight → nothing to say', () => {
		expect(deriveFetchStatus({ now: NOW })).toBeNull();
	});

	// mutation: in fetch-status.ts, change `if (!ask || ask.spent) return null;` to
	// `if (!ask) return null;` → this reds (a spent ask would read "Asked for the full list").
	it('a spent ask (the list arrived) → nothing to say', () => {
		expect(deriveFetchStatus({ ask: ask({ spent: true }), now: NOW })).toBeNull();
	});

	it('1 · asked, owner has not answered → amber, no bar, no timeout', () => {
		const s = deriveFetchStatus({ ask: ask(), now: NOW })!;
		expect(s.tone).toBe('waiting');
		expect(s.title).toBe('Asked for the full list');
		expect(s.sub).toMatch(/^Waiting for the owner · sent /);
		expect(s.pct).toBeNull();
		// Ruling D7: however old the ask, it never turns into a "never answered" failure.
		const ancient = deriveFetchStatus({ ask: ask({ sent_at: '2020-01-01T00:00:00Z' }), now: NOW })!;
		expect(ancient.tone).toBe('waiting');
	});

	// mutation: in fetch-status.ts, change `tone: 'ok',` in the receiving branch to `tone: 'bad',`
	// → this reds (a moving transfer must be green).
	it('3 · receiving → green bar with the byte count', () => {
		const s = deriveFetchStatus({ progress: { received: 4_928_307, total: 11_738_931 }, ask: ask(), now: NOW })!;
		expect(s.tone).toBe('ok');
		expect(s.title).toBe('Receiving the full list');
		expect(s.pct).toBe(42);
		expect(s.sub).toBe('4.7 MB of 11.2 MB · 42%');
	});

	it('a finished run is not "receiving"', () => {
		const s = deriveFetchStatus({ progress: { received: 10, total: 10 }, now: NOW });
		expect(s).toBeNull();
	});

	// mutation: in fetch-status.ts, change `if (attempts > 0) {` to `if (attempts > 99) {`
	// → this reds (a failed dial would read as still waiting).
	it('4 · a dial failed → red, names the attempt and the retry time', () => {
		const lastFail = Date.parse('2026-09-25T13:55:00Z') / 1000;
		const s = deriveFetchStatus({ ask: ask({ dial_attempts: 1, dial_last_fail_unix: lastFail }), now: NOW })!;
		expect(s.tone).toBe('bad');
		expect(s.title).toBe("Couldn't reach the owner");
		expect(s.sub).toMatch(new RegExp(`^Attempt 1 of ${FETCH_DIAL_CAP} · retrying at `));
	});

	it('4 · a retry already due reads "on the next check", never a time in the past', () => {
		const s = deriveFetchStatus({ ask: ask({ dial_attempts: 1, dial_last_fail_unix: 1 }), now: NOW })!;
		expect(s.sub).toBe(`Attempt 1 of ${FETCH_DIAL_CAP} · retrying on the next check`);
	});

	// mutation: in fetch-status.ts, change `if (attempts >= FETCH_DIAL_CAP) {` to
	// `if (attempts > FETCH_DIAL_CAP) {` → this reds (at the cap the driver has stopped).
	it('5 · the dial budget is spent → red, gave up', () => {
		const s = deriveFetchStatus({ ask: ask({ dial_attempts: FETCH_DIAL_CAP, dial_last_fail_unix: 1 }), now: NOW })!;
		expect(s.tone).toBe('bad');
		expect(s.title).toBe("Couldn't get the full list");
	});

	// mutation: in fetch-status.ts, move the `if (oversized)` block BELOW the progress branch
	// → this reds (oversized must win over any other signal).
	it('7 · oversized wins over everything, with no bar', () => {
		const s = deriveFetchStatus({
			oversized: true,
			ask: ask({ dial_attempts: 1 }),
			progress: { received: 1, total: 10 },
			now: NOW,
		})!;
		expect(s.tone).toBe('bad');
		expect(s.title).toBe('Too large to send in full');
		expect(s.sub).toBe('This list is over the 16 MB limit · showing the preview only');
		expect(s.pct).toBeNull();
	});

	it('no state ever says the D-word (MAS-INV-5)', () => {
		const all = [
			deriveFetchStatus({ ask: ask(), now: NOW }),
			deriveFetchStatus({ progress: { received: 1, total: 2 }, now: NOW }),
			deriveFetchStatus({ ask: ask({ dial_attempts: 1, dial_last_fail_unix: 1 }), now: NOW }),
			deriveFetchStatus({ ask: ask({ dial_attempts: 3 }), now: NOW }),
			deriveFetchStatus({ oversized: true, now: NOW }),
		];
		for (const s of all) expect(`${s!.title} ${s!.sub}`).not.toMatch(/download/i);
	});
});
