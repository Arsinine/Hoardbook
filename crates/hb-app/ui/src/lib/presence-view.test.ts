// M17 W5 — the devtest-item-2 regression, pinned. The headline case is the last test in the first
// block: an offline contact's age must GROW with the clock and must never read "just now".

import { describe, it, expect } from 'vitest';
import {
	PRESENCE_TICK_MS,
	PRESENCE_WINDOW_MS,
	checkedLabel,
	formatAge,
	freshIndex,
	newestSeen,
	presenceView,
} from './presence-view.js';

const NOW = Date.UTC(2026, 6, 26, 12, 0, 0);
const iso = (msAgo: number) => new Date(NOW - msAgo).toISOString();

describe('presenceView — real presence, honestly labelled', () => {
	it('a beacon inside the window is Online, with no age line', () => {
		const v = presenceView(iso(60_000), NOW);
		expect(v.online).toBe(true);
		expect(v.lastSeen).toBe('');
	});

	it('the window boundary is inclusive (matches ONLINE_WINDOW_SECS)', () => {
		expect(PRESENCE_WINDOW_MS).toBe(600_000);
		expect(presenceView(iso(PRESENCE_WINDOW_MS), NOW).online).toBe(true);
		expect(presenceView(iso(PRESENCE_WINDOW_MS + 1_000), NOW).online).toBe(false);
	});

	it('never-observed reads "unknown", never "never" and never "just now"', () => {
		for (const empty of [null, undefined, '', 'not-a-date']) {
			const v = presenceView(empty, NOW);
			expect(v.online).toBe(false);
			expect(v.lastSeen).toBe('Last seen — unknown');
			expect(v.lastSeen).not.toMatch(/never|just now/);
		}
	});

	it('a future-dated beacon does not invent a negative age (untrusted relay clock)', () => {
		const v = presenceView(new Date(NOW + 3_600_000).toISOString(), NOW);
		expect(v.online).toBe(true);
		expect(v.lastSeen).toBe('');
	});

	it('DEVTEST ITEM 2: an offline contact carries an age that GROWS, and is never "just now"', () => {
		// The exact field regression: contact goes offline, the label used to freeze at "seen just
		// now" forever. Advance a mocked clock against ONE fixed beacon and watch the age move.
		const beacon = iso(0); // seen exactly at NOW
		const at = (msLater: number) => presenceView(beacon, NOW + msLater);

		expect(at(0).online).toBe(true); // still inside the window
		const t20m = at(20 * 60_000);
		const t4h = at(4 * 3_600_000);
		const t8d = at(8 * 86_400_000);

		expect(t20m.online).toBe(false);
		expect(t20m.lastSeen).toBe('Last seen 20m ago');
		expect(t4h.lastSeen).toBe('Last seen 4h ago');
		expect(t8d.lastSeen).toBe('Last seen 8d ago');
		for (const v of [t20m, t4h, t8d]) expect(v.lastSeen).not.toContain('just now');
	});

	it('the clock tick is finer than the window, so Online flips promptly when it lapses', () => {
		// The row re-reads the clock every PRESENCE_TICK_MS. If that were coarser than the presence
		// window, a peer could sit "Online" well past their beacon's expiry.
		expect(PRESENCE_TICK_MS).toBeLessThan(PRESENCE_WINDOW_MS);
		// One tick after the window lapses, the state has already flipped — no poll required.
		const beacon = iso(0);
		expect(presenceView(beacon, NOW + PRESENCE_WINDOW_MS).online).toBe(true);
		expect(presenceView(beacon, NOW + PRESENCE_WINDOW_MS + PRESENCE_TICK_MS).online).toBe(false);
	});
});

describe('formatAge — the offline ladder has no "just now" rung', () => {
	it('floors at 1m rather than collapsing to "just now"', () => {
		expect(formatAge(0)).toBe('1m ago');
		expect(formatAge(59_000)).toBe('1m ago');
		expect(formatAge(90_000)).toBe('1m ago');
	});

	it('climbs m → h → d → mo', () => {
		expect(formatAge(45 * 60_000)).toBe('45m ago');
		expect(formatAge(3 * 3_600_000)).toBe('3h ago');
		expect(formatAge(2 * 86_400_000)).toBe('2d ago');
		expect(formatAge(70 * 86_400_000)).toBe('2mo ago');
	});
});

describe('checkedLabel — OUR cache age, said as "checked" (W5.1)', () => {
	it('says "checked", never "seen" — the old label was the lie', () => {
		expect(checkedLabel(iso(0), NOW)).toBe('checked just now');
		expect(checkedLabel(iso(5 * 60_000), NOW)).toBe('checked 5m ago');
		expect(checkedLabel(iso(3 * 86_400_000), NOW)).toBe('checked 3d ago');
		for (const s of [iso(0), iso(5 * 60_000), iso(3 * 86_400_000)]) {
			expect(checkedLabel(s, NOW)).not.toContain('seen');
		}
	});

	it('"just now" is allowed here — a poll really did just happen', () => {
		expect(checkedLabel(iso(60_000), NOW)).toBe('checked just now');
	});

	it('a missing or unparseable stamp degrades without throwing', () => {
		expect(checkedLabel(null, NOW)).toBe('checked never');
		expect(checkedLabel('garbage', NOW)).toBe('checked never');
	});
});

describe('newestSeen / freshIndex — this poll beats the persisted stamp', () => {
	it('prefers the live fresh-set entry when it is newer', () => {
		const fresh = freshIndex([{ npub: 'npub1a', seen_at: iso(60_000) }]);
		expect(newestSeen('npub1a', fresh, iso(86_400_000))).toBe(iso(60_000));
	});

	it('keeps the persisted stamp when it is newer than a stale live entry', () => {
		const fresh = freshIndex([{ npub: 'npub1a', seen_at: iso(86_400_000) }]);
		expect(newestSeen('npub1a', fresh, iso(60_000))).toBe(iso(60_000));
	});

	it('falls back to the persisted stamp (restart survival) and then to null', () => {
		const empty = freshIndex([]);
		expect(newestSeen('npub1a', empty, iso(3_600_000))).toBe(iso(3_600_000));
		expect(newestSeen('npub1a', empty, null)).toBeNull();
		expect(newestSeen('npub1a', freshIndex(undefined), undefined)).toBeNull();
	});

	it('a contact absent from the fresh set keeps its stored age (not blanked to unknown)', () => {
		// A poll that simply did not see Bob is not evidence about Bob; his age must survive.
		const fresh = freshIndex([{ npub: 'npub1other', seen_at: iso(0) }]);
		const seen = newestSeen('npub1bob', fresh, iso(4 * 3_600_000));
		expect(presenceView(seen, NOW).lastSeen).toBe('Last seen 4h ago');
	});
});
