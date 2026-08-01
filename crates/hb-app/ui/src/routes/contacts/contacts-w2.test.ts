// M20 W2 — "add is slow / resolves twice" + "Contacts mount fan-out is unbounded", guarded at the
// source. The repo's established pattern for route-page guards is source scanning (see contacts-w1,
// contacts-w5, mas-inv5-no-download): the route's onMount fan-out and $app/navigation goto make a
// full mount heavier than the wiring checks warrant, so we pin the two things only the page can get
// wrong: (1) the resolved peer is threaded from the lookup into the follow, and (2) the mount
// fan-out is bounded + freshness-skipped. The behaviour-level proof that the follow leg no longer
// resolves is the Rust suite (`save_followed_peer_persists_a_pre_resolved_peer_without_a_relay` and
// siblings) — `save_followed_peer` takes no `SharedRelay`, the structural proof the relay is not dialed.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const contactsSrc = () =>
	readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

const panelSrc = () =>
	readFileSync(new URL('../../lib/components/AddContactPanel.svelte', import.meta.url), 'utf8');

describe('contacts W2 — add resolves once, not twice', () => {
	it('openAddContact carries a pre-resolved peer through the funnel', () => {
		const s = contactsSrc();
		// The dialog opener holds the resolved peer (the lookup's result) alongside the code/npub.
		expect(s).toMatch(/let addContactResolved/);
		// openAddContact's signature accepts the resolved peer as its 4th arg.
		expect(s).toMatch(/function openAddContact\(code: string, npub: string, displayName: string, resolved/);
		// completeFollow threads the resolved peer into the follow call.
		const fn = s.slice(s.indexOf('async function completeFollow'), s.indexOf('async function handleAddContact'));
		expect(fn).toMatch(/resolved/);
		expect(fn).toMatch(/await follow\(code, group \?\? undefined, petname, resolved/);
	});

	it('handleAddContactSave and handleAddContactSkip pass the resolved peer to completeFollow', () => {
		const s = contactsSrc();
		const saveFn = s.slice(s.indexOf('async function handleAddContactSave'), s.indexOf('async function handleAddContactSkip'));
		expect(saveFn).toContain('addContactResolved = null');
		expect(saveFn).toMatch(/await completeFollow\(code, npub, detail\.group, detail\.petname, resolved\)/);
		const skipFn = s.slice(s.indexOf('async function handleAddContactSkip'), s.indexOf('// "+ Add contact"'));
		expect(skipFn).toContain('addContactResolved = null');
		expect(skipFn).toMatch(/await completeFollow\(code, npub, null, undefined, resolved\)/);
	});

	it('the AddContactPanel hands the lookup result through onadd', () => {
		const s = panelSrc();
		// The onadd callback signature carries the resolved peer as its 4th arg.
		expect(s).toMatch(/onadd\?: \(code: string, npub: string, displayName: string, resolved: CachedPeer \| null\)/);
		// handleFollow passes the lookup result (not null) so the follow leg skips the resolve.
		const hf = s.slice(s.indexOf('function handleFollow'), s.indexOf('function handleKeydown'));
		expect(hf).toMatch(/onadd\?\.\(lookedUpCode, result\.npub, result\.profile\?\.display_name \?\? '', result\)/);
		// A discovery hit carries null (no pre-resolved peer) — follow resolves it as before.
		const fh = s.slice(s.indexOf('function followHit'), s.indexOf('function close'));
		expect(fh).toMatch(/onadd\?\.\(hit\.npub, hit\.npub, hit\.display_name, null\)/);
	});

	it('the api.ts follow bridge accepts a resolvedPeer', async () => {
		const api = await import(new URL('../../lib/api.ts', import.meta.url).pathname);
		// The follow function exists and accepts the 4th resolvedPeer arg (its length is the declared
		// param count). A structural check that the bridge forwards it by name.
		const src = readFileSync(new URL('../../lib/api.ts', import.meta.url), 'utf8');
		expect(src).toMatch(/resolvedPeer/);
		expect(typeof api.follow).toBe('function');
	});
});

describe('contacts W2 — mount fan-out is bounded + freshness-skipped', () => {
	it('replaces the unbounded forEach fan-out with a bounded worker pool', () => {
		const s = contactsSrc();
		// The old defect: `$contacts.forEach(async c => refreshContact(...))` fired every contact's
		// resolve in parallel, unbounded. The fix is a cursor-driven worker pool.
		expect(s).not.toMatch(/\$contacts\.forEach\(async \(c\) =>/);
		expect(s).toContain('REFRESH_CONCURRENCY');
		expect(s).toContain('let cursor = 0');
		expect(s).toMatch(/Array\.from\(\{ length: Math\.min\(REFRESH_CONCURRENCY, stale\.length\) \}, worker\)/);
	});

	it('caps concurrency at 4', () => {
		const s = contactsSrc();
		expect(s).toMatch(/const REFRESH_CONCURRENCY = 4/);
	});

	it('skips contacts refreshed within the last 10 minutes', () => {
		const s = contactsSrc();
		expect(s).toMatch(/const REFRESH_FRESHNESS_MS = 10 \* 60 \* 1000/);
		// The freshness gate filters contacts BEFORE any refresh runs.
		expect(s).toMatch(/const stale = \$contacts\.filter\(/);
		expect(s).toMatch(/now - fetched > REFRESH_FRESHNESS_MS/);
	});
});
