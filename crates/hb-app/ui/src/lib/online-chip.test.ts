import { describe, expect, it } from 'vitest';
import { onlineChipView } from './online-chip.js';
import type { OnlineCount } from './api';

const count = (online: number | null): OnlineCount => ({
	online,
	fetched_at: online === null ? null : '2026-06-21T00:00:00Z',
	relay_set: ['wss://relay.example'],
});

describe('online chip — relay-derived count (M9)', () => {
	it('show_online_count off hides the chip entirely', () => {
		const v = onlineChipView(count(12), false);
		expect(v.show).toBe(false);
	});

	it('renders a real count when known', () => {
		const v = onlineChipView(count(12), true);
		expect(v.show).toBe(true);
		expect(v.unknown).toBe(false);
		expect(v.label).toContain('12');
	});

	it('m4 — unknown count (null) shows a dash, never a misleading 0', () => {
		const v = onlineChipView(count(null), true);
		expect(v.show).toBe(true);
		expect(v.unknown).toBe(true);
		expect(v.label).toContain('–');
		expect(v.label).not.toContain('0 on network');
	});

	it('m4 — a missing count object also falls back to the dash', () => {
		const v = onlineChipView(null, true);
		expect(v.show).toBe(true);
		expect(v.unknown).toBe(true);
		expect(v.label).toContain('–');
	});

	it('a genuine zero count renders 0 honestly (not the unknown dash)', () => {
		const v = onlineChipView(count(0), true);
		expect(v.unknown).toBe(false);
		expect(v.label).toContain('0 on network');
	});
});
