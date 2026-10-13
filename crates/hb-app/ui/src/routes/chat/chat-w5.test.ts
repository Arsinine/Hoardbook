// M17 W5.3 added a muted "inbox may be stale" relay line to Chat. The owner removed it on
// 2026-10-13, along with the matching relay message on the Contacts topbar: relay state belongs in
// Settings → Relays. `relayWhyHint` itself stays, because the Contacts network chip still shows it
// as a tooltip, so its wording is still pinned here.

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

describe('relayWhyHint — the wording the Contacts network chip tooltip shows', () => {
	it('is empty while every relay is connected (no chrome when nothing is wrong)', () => {
		expect(relayWhyHint(health(3, 3) as never)).toBe('');
	});

	it('names the failure when relays are down', () => {
		expect(relayWhyHint(health(3, 0) as never)).toBe('No relay reachable');
		expect(relayWhyHint(health(3, 2) as never)).toBe('1 of 3 relays unreachable');
		expect(relayWhyHint([])).toBe('No relays configured');
	});
});

describe('chat — no relay line (owner, 2026-10-13)', () => {
	it('renders no relay-health line and runs no relay-health poll', () => {
		const s = src();
		// MUTATION (P-10): put `<div class="relay-hint">No relays configured</div>` back into the
		// conversation list markup — this reds.
		expect(s).not.toContain('class="relay-hint"');
		expect(s).not.toContain('inbox may be stale');
		// The 60 s poll existed only to feed that line; a left-over poll is wasted relay traffic.
		expect(s).not.toContain('relayStatus()');
		expect(s).not.toContain('RELAY_HEALTH_POLL_VISIBLE_MS');
	});
});
