import { describe, expect, it } from 'vitest';
import { onlineChipView } from './online-chip.js';
import type { OnlineCount, RelayHealth } from './api';

const count = (online: number | null): OnlineCount => ({
	online,
	fetched_at: online === null ? null : '2026-06-21T00:00:00Z',
	relay_set: ['wss://relay.example'],
});

const connected = (): RelayHealth => ({ url: 'wss://relay.example', status: 'connected', connected: true, lastError: null });
const disconnected = (): RelayHealth => ({ url: 'wss://relay.example', status: 'disconnected', connected: false, lastError: 'timeout' });

describe('online chip — relay-derived count (M9)', () => {
	it('show_online_count off hides the chip entirely', () => {
		const v = onlineChipView(count(12), false, [connected()]);
		expect(v.show).toBe(false);
	});

	it('renders a real count when known', () => {
		const v = onlineChipView(count(12), true, [connected()]);
		expect(v.show).toBe(true);
		expect(v.unknown).toBe(false);
		expect(v.label).toContain('12');
	});

	it('m4 — unknown count (null, relays up) shows a dash, never a misleading 0', () => {
		const v = onlineChipView(count(null), true, [connected()]);
		expect(v.show).toBe(true);
		expect(v.unknown).toBe(true);
		expect(v.label).toContain('–');
		expect(v.label).not.toContain('0 on network');
	});

	it('m4 — a missing count object also falls back to the dash', () => {
		const v = onlineChipView(null, true, [connected()]);
		expect(v.show).toBe(true);
		expect(v.unknown).toBe(true);
		expect(v.label).toContain('–');
	});

	it('a genuine zero count renders 0 honestly (not the unknown dash)', () => {
		const v = onlineChipView(count(0), true, [connected()]);
		expect(v.unknown).toBe(false);
		expect(v.label).toContain('0 on network');
	});
});

describe('online chip — QURATOR-67 relay-health colour (red / amber / green)', () => {
	it('RED: no relay connected', () => {
		const v = onlineChipView(count(5), true, [disconnected()]);
		expect(v.state).toBe('red');
	});

	it('RED: empty relay health list', () => {
		const v = onlineChipView(count(5), true, []);
		expect(v.state).toBe('red');
	});

	it('RED: count is unknown (null), even with relays connected', () => {
		const v = onlineChipView(count(null), true, [connected()]);
		expect(v.state).toBe('red');
	});

	it('RED: missing count object, even with relays connected', () => {
		const v = onlineChipView(null, true, [connected()]);
		expect(v.state).toBe('red');
	});

	it('AMBER: relays connected, count is exactly 0', () => {
		const v = onlineChipView(count(0), true, [connected()]);
		expect(v.state).toBe('amber');
	});

	it('GREEN: relays connected, count is >= 1', () => {
		const v = onlineChipView(count(3), true, [connected()]);
		expect(v.state).toBe('green');
	});

	it('GREEN: a count of exactly 1 is green, NOT amber', () => {
		const v = onlineChipView(count(1), true, [connected()]);
		expect(v.state).toBe('green');
		expect(v.state).not.toBe('amber');
	});

	it('GREEN: mixed relay health — at least one connected is enough', () => {
		const v = onlineChipView(count(2), true, [disconnected(), connected(), disconnected()]);
		expect(v.state).toBe('green');
	});

	it('RED: multiple relays, all disconnected, non-zero count', () => {
		const v = onlineChipView(count(7), true, [disconnected(), disconnected()]);
		expect(v.state).toBe('red');
	});

	// The ordering invariant: relay health is checked BEFORE the count. A stale non-zero count in the
	// cache must NOT paint the chip amber/green when the client cannot reach any relay — the red dot is
	// the signal that the number you see may be wrong because your machine is unplugged. This is the
	// confusion that made a presence bug take five separate reports to diagnose.
	it('ORDERING: relays down + a stale non-zero count is STILL red, not green or amber', () => {
		const v = onlineChipView(count(42), true, [disconnected()]);
		expect(v.state).toBe('red');
	});

	it('ORDERING: relays down + a count of exactly 0 is red, not amber', () => {
		const v = onlineChipView(count(0), true, [disconnected()]);
		expect(v.state).toBe('red');
	});
});

describe('online chip — no hardcoded colour emoji in the label (QURATOR-67)', () => {
// The defect: the label baked 🟢 into the string in BOTH branches, so no amount of relay health
// reaching the component could ever change the colour. The dot is now rendered from `state`; the
// label must never carry a colour glyph that could drift apart from it.
	it('every shown label is free of colour-circle emoji', () => {
		const samples = [
			onlineChipView(count(12), true, [connected()]),
			onlineChipView(count(1), true, [connected()]),
			onlineChipView(count(0), true, [connected()]),
			onlineChipView(count(null), true, [connected()]),
			onlineChipView(null, true, [connected()]),
			onlineChipView(count(99), true, [disconnected()]),
		];
		for (const v of samples) {
			expect(v.label, `label "${v.label}" must not carry a colour-circle emoji`).not.toMatch(/[🟢🔴🟡🟠🟦🟪⭕]/);
		}
	});
});
