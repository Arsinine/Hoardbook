import { describe, expect, it } from 'vitest';
import { relayRowView, relayWhyHint } from './relay-health.js';
import type { RelayHealth } from './api';

const h = (status: string, connected: boolean): RelayHealth => ({
	url: 'wss://relay.example',
	status,
	connected,
	lastError: null,
});

describe('relay-health — per-relay status view (M12 W1, Decision D)', () => {
	it('maps connected → ok dot', () => {
		const v = relayRowView(h('connected', true));
		expect(v.tone).toBe('ok');
		expect(v.label).toBe('Connected');
	});

	it('maps connecting → warn dot', () => {
		expect(relayRowView(h('connecting', false)).tone).toBe('warn');
	});

	it('maps disconnected/terminated → bad (Unreachable)', () => {
		expect(relayRowView(h('disconnected', false))).toMatchObject({ tone: 'bad', label: 'Unreachable' });
		expect(relayRowView(h('terminated', false)).tone).toBe('bad');
	});

	it('a fully-connected set has no "why" hint', () => {
		expect(relayWhyHint([h('connected', true), h('connected', true)])).toBe('');
	});

	it('names how many relays are unreachable so the dash says why', () => {
		const hint = relayWhyHint([h('connected', true), h('disconnected', false), h('disconnected', false)]);
		expect(hint).toBe('2 of 3 relays unreachable');
	});

	it('says "No relay reachable" when none are connected (the – cause)', () => {
		expect(relayWhyHint([h('disconnected', false)])).toBe('No relay reachable');
	});
});
