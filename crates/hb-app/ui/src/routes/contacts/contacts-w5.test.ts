// M17 W5 — the contact row's wiring, guarded at the source. The *behaviour* (ages that grow, the
// unknown state, "checked" vs "Last seen") is pinned against real logic in
// `$lib/presence-view.test.ts`; these assertions pin the two things only the route can get wrong:
// which field feeds which label, and that the per-contact presence rides the poll that already
// existed rather than a new relay query.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const src = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('contacts W5 — the row stops calling our poll time "seen"', () => {
	it('the old "seen {last_fetched}" label is gone', () => {
		const s = src();
		expect(s).not.toContain('seen {lastSeenLabel');
		expect(s).not.toContain('function lastSeenLabel');
		// The ad-hoc ladder that produced "just now" for a week-old contact is gone with it.
		expect(s).not.toContain('function formatLastSeen');
	});

	it('renders our cache age as "checked {t}" from last_fetched', () => {
		expect(src()).toContain('checkedLabel(peer.last_fetched');
	});

	it('renders their last-seen from presence, and only while offline', () => {
		const s = src();
		// The age line is inside a `{#if !peer.online}` guard — an online contact needs no age.
		const idx = s.indexOf('presenceOf(peer).lastSeen');
		expect(idx).toBeGreaterThan(-1);
		expect(s.slice(idx - 120, idx)).toContain('{#if !peer.online}');
	});

	it('presence comes from the beacon, never from last_fetched', () => {
		const s = src();
		const fn = s.slice(s.indexOf('function presenceOf'), s.indexOf('function withPresence'));
		expect(fn).toContain('newestSeen(peer.npub, freshSeen, peer.last_presence)');
		expect(fn).not.toContain('last_fetched');
	});
});

describe('contacts W5 — no new relay load', () => {
	it('the fresh set rides the EXISTING 60s online poll, not a new query', () => {
		const s = src();
		// The per-contact data is read off `onlineData` — the chip's own cached payload.
		expect(s).toMatch(/freshIndex\(\(?onlineData[^)]*\)?\?\.fresh\)/);
		// No per-contact presence call was introduced (that would be a fan-out across the roster).
		expect(s).not.toMatch(/presenceFor|fetchPresence|contactPresence/);
		// Still exactly one poll interval on this page, at the slow cadence.
		expect(s.match(/setInterval\(/g) ?? []).toHaveLength(1);
		expect(s).toContain('ONLINE_POLL_VISIBLE_MS');
	});

	it('a real beacon outranks the stored online flag, but absence of one does not', () => {
		// Not seeing a beacon this window is not evidence they went offline — the stored flag stands.
		const s = src();
		const fn = s.slice(s.indexOf('function withPresence'), s.indexOf('// Tag editing state'));
		expect(fn).toContain('p.known ? { ...peer, online: p.online } : peer');
	});
});
