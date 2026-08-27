// M17 W5.3 — Chat says when the inbox is stale. The DM poll's `catch {}` eats every relay failure,
// so an unreachable relay and a quiet day looked identical. One muted line, from the relay-health
// store the Contacts topbar already reads — no toast, no new backend surface.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { relayWhyHint } from '$lib/relay-health.js';

const src = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

const health = (n: number, connected: number) =>
	Array.from({ length: n }, (_, i) => ({
		url: `wss://r${i}`,
		status: i < connected ? 'connected' : 'terminated',
		connected: i < connected,
	}));

describe('chat W5.3 — the unreachable-relay line appears and clears', () => {
	it('is empty while every relay is connected (no chrome when nothing is wrong)', () => {
		expect(relayWhyHint(health(3, 3) as never)).toBe('');
	});

	it('names the failure when relays are down', () => {
		expect(relayWhyHint(health(3, 0) as never)).toBe('No relay reachable');
		expect(relayWhyHint(health(3, 2) as never)).toBe('1 of 3 relays unreachable');
		expect(relayWhyHint([])).toBe('No relays configured');
	});

	it('the line is rendered only when the hint is non-empty', () => {
		const s = src();
		const idx = s.indexOf('class="relay-hint"');
		expect(idx).toBeGreaterThan(-1);
		expect(s.slice(idx - 200, idx)).toContain('{#if relayHint}');
		expect(s).toContain('let relayHint = $derived(relayWhyHint(relayHealth))');
	});

	it('is muted chrome, not a toast', () => {
		const s = src();
		const idx = s.indexOf('class="relay-hint"');
		const line = s.slice(idx, s.indexOf('\n', idx));
		expect(line).not.toContain('toast');
		// Styled with the dim foreground, no alert tone.
		const css = s.slice(s.indexOf('.relay-hint {'), s.indexOf('}', s.indexOf('.relay-hint {')));
		expect(css).toContain('--fg-dim');
	});

	it('polls relay health on the slow tick, not the 3s DM cadence', () => {
		// The constant was ONLINE_POLL_VISIBLE_MS until devtest item 4 sped that one up to 20 s for the
		// presence chip. Relay health kept the 60 s cadence under its own name — see the note on
		// RELAY_HEALTH_POLL_VISIBLE_MS for why the two must not share a knob.
		const s = src();
		const idx = s.indexOf('const healthPoll = setInterval');
		expect(idx).toBeGreaterThan(-1);
		expect(s.slice(idx, s.indexOf('\n', idx))).toContain('RELAY_HEALTH_POLL_VISIBLE_MS');
		// And it is torn down with the page.
		expect(s).toContain('clearInterval(healthPoll)');
	});
});
