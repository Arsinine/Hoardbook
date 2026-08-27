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
		const idx = s.indexOf('presence.lastSeen');
		expect(idx).toBeGreaterThan(-1);
		expect(s.slice(idx - 160, idx)).toContain('{#if !peer.online');
		// QURATOR-135: the age line also requires a KNOWN verdict — an unknown row shows only the
		// "Checking…" pill, never an age line either.
		expect(s.slice(idx - 160, idx)).toContain('presence.online !== null');
	});

	it('presence comes from the beacon, never from last_fetched', () => {
		const s = src();
		const fn = s.slice(s.indexOf('function presenceOf'), s.indexOf('function withPresence'));
		expect(fn).toContain('newestSeen(peer.npub, freshSeen, peer.last_presence)');
		expect(fn).not.toContain('last_fetched');
	});
});

describe('contacts W5 — no new relay load', () => {
	it('the fresh set rides the EXISTING online poll, not a new query', () => {
		const s = src();
		// The per-contact data is read off `onlineData` — the chip's own cached payload.
		expect(s).toMatch(/freshIndex\(\(?onlineData[^)]*\)?\?\.fresh\)/);
		// No per-contact presence call was introduced (that would be a fan-out across the roster).
		expect(s).not.toMatch(/presenceFor|fetchPresence|contactPresence/);
		// Exactly one NETWORK poll on this page, at the slow cadence. (The second interval is the
		// local clock tick — it touches no relay; asserted separately below.)
		expect(s.match(/setInterval\(/g) ?? []).toHaveLength(2);
		expect(s).toContain('setInterval(refreshOnline, ONLINE_POLL_VISIBLE_MS)');
		expect(s).toContain('setInterval(() => { nowMs = Date.now(); }, PRESENCE_TICK_MS)');
	});

	it('the age has its own clock, so it cannot freeze when a poll fails', () => {
		// Review MEDIUM: reading Date.now() in a template helper ties the age to the poll assigning
		// onlineData — a rejected poll or a hidden tab would freeze it, which is the original defect
		// in a new form. The row must read a reactive `nowMs`, never Date.now() directly.
		const s = src();
		const fn = s.slice(s.indexOf('function presenceOf'), s.indexOf('function withPresence'));
		expect(fn).toContain('presenceView(seen, nowMs)');
		expect(fn).not.toContain('Date.now()');
		expect(s).toContain('let nowMs = $state(Date.now())');
		// The tick is torn down with the page.
		expect(s).toContain('clearInterval(clockTimer)');
	});

	it('the header count reads the presence-adjusted roster, like the rows do', () => {
		// Review MEDIUM: the header counted raw `$contacts.online`, so it could say "0 online"
		// directly above a row showing an Online pill.
		const s = src();
		expect(s).toContain('{onlineTotal} online');
		expect(s).not.toContain('$contacts.filter(c => c.online).length');
		// Presence is applied once, to the whole roster, upstream of both the count and the filters.
		expect(s).toContain('let presenced = $derived($contacts.map(withPresence))');
		expect(s).toContain('let onlineTotal = $derived(presenced.filter(c => c.online).length)');
		expect(s).toMatch(/let visible = \$derived\(\s*presenced\./);
	});

	it('a real beacon outranks the stored online flag, but absence of one does not', () => {
		// Not seeing a beacon this window is not evidence they went offline — the stored flag stands.
		// QURATOR-135: the branch condition is the view's own tri-state (`online === null` ⇒ never
		// observed), so an unknown row keeps its stored flag and the PILL renders "Checking…".
		const s = src();
		const fn = s.slice(s.indexOf('function withPresence'), s.indexOf('// Tag editing state'));
		expect(fn).toContain('p.online !== null ? { ...peer, online: p.online } : peer');
	});
});
