import { describe, expect, it } from 'vitest';
import { beaconLine, relativeAgo } from './beacon-view.js';
import type { BeaconReport } from './api';

const report = (over: Partial<BeaconReport> = {}): BeaconReport => ({
	lastAttemptAt: 0,
	lastSuccessAt: 0,
	relays: [],
	lastError: null,
	...over,
});

describe('beacon-view — presence-beacon status line (devtest #9)', () => {
	it('relativeAgo: under a minute reads "just now"', () => {
		expect(relativeAgo(0)).toBe('just now');
		expect(relativeAgo(59)).toBe('just now');
	});

	it('relativeAgo: minutes boundary', () => {
		expect(relativeAgo(60)).toBe('1 min ago');
		expect(relativeAgo(120)).toBe('2 min ago');
		expect(relativeAgo(3599)).toBe('59 min ago');
	});

	it('relativeAgo: hours boundary', () => {
		expect(relativeAgo(3600)).toBe('1 h ago');
		expect(relativeAgo(7200)).toBe('2 h ago');
	});

	it('no report yet ⇒ warn "not sent yet"', () => {
		expect(beaconLine(null, 'wss://relay.example', 1000)).toEqual({
			text: 'beacon: not sent yet',
			tone: 'warn',
		});
	});

	it('lastAttemptAt 0 ⇒ warn "not sent yet"', () => {
		const r = report({ lastAttemptAt: 0 });
		expect(beaconLine(r, 'wss://relay.example', 1000)).toEqual({
			text: 'beacon: not sent yet',
			tone: 'warn',
		});
	});

	it('accepted + recent ⇒ ok "sent N min ago"', () => {
		const r = report({
			lastAttemptAt: 880,
			lastSuccessAt: 880,
			relays: [{ url: 'wss://relay.example', outcome: 'accepted', reason: null }],
		});
		const v = beaconLine(r, 'wss://relay.example', 1000);
		expect(v.tone).toBe('ok');
		expect(v.text).toBe('beacon: sent 2 min ago');
	});

	it('rejected ⇒ bad with reason', () => {
		const r = report({
			lastAttemptAt: 1000,
			lastSuccessAt: 1000,
			relays: [{ url: 'wss://relay.example', outcome: 'rejected', reason: 'rate-limited' }],
		});
		const v = beaconLine(r, 'wss://relay.example', 1000);
		expect(v.tone).toBe('bad');
		expect(v.text).toBe('beacon failing: rate-limited');
	});

	it('url absent from relays but lastError set ⇒ bad', () => {
		const r = report({ lastAttemptAt: 1000, lastError: 'no relay this cycle: pool empty' });
		const v = beaconLine(r, 'wss://relay.example', 1000);
		expect(v.tone).toBe('bad');
		expect(v.text).toBe('beacon failing: no relay this cycle: pool empty');
	});

	it('url absent, no lastError ⇒ warn "not sent yet"', () => {
		const r = report({
			lastAttemptAt: 1000,
			lastSuccessAt: 1000,
			relays: [{ url: 'wss://other.example', outcome: 'accepted', reason: null }],
		});
		const v = beaconLine(r, 'wss://relay.example', 1000);
		expect(v.tone).toBe('warn');
		expect(v.text).toBe('beacon: not sent yet');
	});
});
