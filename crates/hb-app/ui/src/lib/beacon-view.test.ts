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

	it('no report at all ⇒ "status unavailable", NOT "not sent yet"', () => {
		// A failed `beacon_status` invoke is swallowed by the caller. It must not masquerade as a
		// beacon that was never sent — those are different faults with different fixes.
		expect(beaconLine(null, 'wss://relay.example', 1000)).toEqual({
			text: 'beacon: status unavailable',
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

	it('url absent, no lastError ⇒ "no ack from this relay", NOT "not sent yet"', () => {
		// The attempt DID happen and reached the relays — this one just never answered. Reporting it
		// as "not sent yet" hid a live beacon behind a never-fired label.
		const r = report({
			lastAttemptAt: 1000,
			lastSuccessAt: 1000,
			relays: [{ url: 'wss://other.example', outcome: 'accepted', reason: null }],
		});
		const v = beaconLine(r, 'wss://relay.example', 1000);
		expect(v.tone).toBe('warn');
		expect(v.text).toBe('beacon: no ack from this relay');
	});
});

// The row's URL is the CONFIGURED string; `report.relays[].url` is the relay pool's rendering of it
// (`RelayUrl::to_string`). They differ in trailing slash and host case, so the old raw `===` match
// missed every row and fell through to "not sent yet" — a healthy beacon reported as never sent.
// `hb-net::RelayClient::relay_status` already trims the slash for exactly this reason.
describe('beacon-view — relay URLs match the way the rest of the codebase matches them', () => {
	const sent = (relayUrl: string): BeaconReport => ({
		lastAttemptAt: 880,
		lastSuccessAt: 880,
		relays: [{ url: relayUrl, outcome: 'accepted', reason: null }],
		lastError: null,
	});

	it('pool URL has a trailing slash, configured URL does not ⇒ still "sent"', () => {
		const v = beaconLine(sent('wss://relay.damus.io/'), 'wss://relay.damus.io', 1000);
		expect(v.tone).toBe('ok');
		expect(v.text).toBe('beacon: sent 2 min ago');
	});

	it('configured URL has a trailing slash, pool URL does not ⇒ still "sent"', () => {
		const v = beaconLine(sent('wss://relay.damus.io'), 'wss://relay.damus.io/', 1000);
		expect(v.tone).toBe('ok');
	});

	it('host case differs ⇒ still "sent"', () => {
		const v = beaconLine(sent('wss://Relay.Damus.io'), 'wss://relay.damus.io', 1000);
		expect(v.tone).toBe('ok');
	});

	it('a rejection is still surfaced through the same normalization', () => {
		const r: BeaconReport = {
			lastAttemptAt: 1000,
			lastSuccessAt: 1000,
			relays: [{ url: 'wss://relay.damus.io/', outcome: 'rejected', reason: 'rate-limited' }],
			lastError: null,
		};
		const v = beaconLine(r, 'wss://relay.damus.io', 1000);
		expect(v.tone).toBe('bad');
		expect(v.text).toBe('beacon failing: rate-limited');
	});

	it('a genuinely different relay still does NOT match', () => {
		const v = beaconLine(sent('wss://nos.lol'), 'wss://relay.damus.io', 1000);
		expect(v.text).toBe('beacon: no ack from this relay');
	});
});
