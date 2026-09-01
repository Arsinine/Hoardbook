// @vitest-environment jsdom
// QURATOR-159 — the manifest-download progress bar in the Browse paywall block. The producer (the
// Rust half emitting `manifest-progress` with `{ request_id, slug, received, total }`) lives in
// another lane; this pins the CONSUMER: the listener registered on mount, the bar rendered ONLY
// while the selected collection's slug has an in-flight run (received < total), the exact
// owner-signed copy "Fetching the full list", and byte progress. Ruling D6: the bar is
// source-agnostic — no peer name/npub/avatar/"from X" anywhere. Ruling D7: no stalled/"never
// answered" state exists, so none is tested for; a final received === total event must simply
// clear the bar back to the ask affordance.
//
// BEHAVIOURAL mount test (q79/q83 pattern): the real Browse page mounts with only `$lib/api.js`
// (and the Tauri modules) mocked; `@tauri-apps/api/event`'s `listen` mock CAPTURES the handler the
// page registers, so the producer is simulated by invoking it with synthetic payloads — the same
// contract the Rust side will emit against. A source-scan is not acceptable for this (CLAUDE.md →
// P-4): the property is which events move which bar, which only a mount can see.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red. The mutation probes (each applied
// alone, then reverted):
//   A. in `paywallProgress`, drop the slug key (read the FIRST entry in the map) — reds the
//      keyed-on-slug test (a foreign slug's event paints the bar) and no other.
//   B. in `paywallProgress`, delete the `p.received >= p.total` guard — reds the completion test
//      (the bar lingers after the final event).
//   C. in the template, delete the `{#if paywallProgress}` branch — reds every test here.
//   D. replace the label text with anything else — reds the exact-copy assertion.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { CachedPeer, Collection } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	requestManifestFrom: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue({}),
	getContacts: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// The producer stand-in: capture every (eventName, handler) the page registers so a test can fire
// the `manifest-progress` contract at it. The unlisten fn is what the page must call on destroy.
const listenHandlers = vi.hoisted(() => new Map<string, (e: { payload: unknown }) => void>());
const unlisteners = vi.hoisted(() => new Array<() => void>());
vi.mock('@tauri-apps/api/event', () => ({
	listen: vi.fn(async (name: string, handler: (e: { payload: unknown }) => void) => {
		listenHandlers.set(name, handler);
		const off = vi.fn();
		unlisteners.push(off);
		return off;
	}),
}));

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer (the
// q92/q102/q79 pattern). The npub is inlined because vi.mock is hoisted above every const.
vi.mock('$app/stores', () => ({
	page: {
		subscribe: (fn: (v: { url: URL }) => void) => {
			fn({ url: new URL('http://localhost/browse?peer=npub1archarcharcharcharcharcharcharcharcharcharchar') });
			return () => {};
		},
	},
}));

// bech32-safe fixture ids (charset excludes 1/b/i/o after the separator); length matches the npub
// in the stubbed $page URL — peerFromQuery matches on the full string.
const AUTHOR_NPUB = 'npub1archarcharcharcharcharcharcharcharcharcharchar';

const PAYWALL_COL: Collection = {
	slug: 'archive',
	path_alias: 'The Archive',
	item_count: 40,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	// devtest #7 paywall teaser: truncated + total_items > what the listing carries — the paywall
	// block (where the bar renders) only exists for a truncated collection.
	truncated: true,
	total_items: 40,
	snapshot_fingerprint: 'aaaa',
	teaser_event_id: 'ev-teaser',
	listing: Array.from({ length: 5 }, (_, i) => ({
		name: `clip-${i}.mkv`,
		item_type: 'File' as const,
		tags: [],
		children: [],
	})),
};

const OTHER_COL: Collection = {
	...PAYWALL_COL,
	slug: 'other-vault',
	path_alias: 'The Other Vault',
};

const AUTHOR_PEER: CachedPeer = {
	npub: AUTHOR_NPUB,
	browse_key_hex: 'aabbccdd',
	collections: [PAYWALL_COL, OTHER_COL],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'The Author', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	listenHandlers.clear();
	unlisteners.length = 0;
	contacts.set([]);
});

interface ProgressPayload { request_id: string; slug: string; received: number; total: number }

/** Fire the producer's contract at the page's registered listener. */
function emitProgress(p: ProgressPayload) {
	const handler = listenHandlers.get('manifest-progress');
	if (!handler) throw new Error('manifest-progress listener was never registered');
	handler({ payload: p });
}

/** Mount, select the peer (deep-link) and open `slug`'s collection card → the paywall block. */
async function driveToPaywall(slug: string) {
	contacts.set([AUTHOR_PEER]);
	render(BrowsePage);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('The Archive'));
	const card = [...document.querySelectorAll<HTMLButtonElement>('.col-card')].find(
		(c) => (c.textContent ?? '').includes(slug === 'archive' ? 'The Archive' : 'The Other Vault'),
	);
	expect(card, `the ${slug} collection card must render`).toBeTruthy();
	await fireEvent.click(card!);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('more item'));
}

function barEl(): HTMLElement | null {
	return document.querySelector('.paywall-progress');
}

/** Click the contact breadcrumb, returning to the peer's collection grid. */
async function crumbBackToPeer() {
	const crumb = [...document.querySelectorAll<HTMLButtonElement>('.bc-btn')].find(
		(b) => (b.textContent ?? '').includes('The Author'),
	);
	expect(crumb, 'the contact breadcrumb must be reachable to back out of a collection').toBeTruthy();
	await fireEvent.click(crumb!);
	await tick();
}

/** Open a collection card by its visible name. Fails loudly rather than skipping. */
async function openCard(name: string) {
	const card = [...document.querySelectorAll<HTMLButtonElement>('.col-card')].find(
		(c) => (c.textContent ?? '').includes(name),
	);
	expect(card, `the "${name}" card must be reachable or this test proves nothing`).toBeTruthy();
	await fireEvent.click(card!);
	await tick();
}

describe('QURATOR-159 — the manifest-fetch progress bar (paywall block)', () => {
	it('an event for the SELECTED slug renders the bar with the exact copy and byte progress', async () => {
		await driveToPaywall('archive');
		// Before any event: no bar, the ask affordance stands.
		expect(barEl()).toBeNull();

		emitProgress({ request_id: 'req-1', slug: 'archive', received: 2621440, total: 10485760 });
		await tick();
		const bar = barEl();
		expect(bar, 'the in-flight bar replaces the ask affordance').toBeTruthy();
		// Owner-signed copy, EXACT.
		expect(bar!.textContent).toContain('Fetching the full list');
		// Byte progress alongside (fmtLargestUnit: 2621440 → "2.5 MB", 10485760 → "10.0 MB").
		expect(bar!.textContent).toContain('2.5 MB');
		expect(bar!.textContent).toContain('10.0 MB');
		// Determinate: the progressbar role carries the live percentage (25%).
		const track = bar!.querySelector('[role="progressbar"]');
		expect(track?.getAttribute('aria-valuenow')).toBe('25');
		// The ask button is gone while the fetch is in flight (the bar stands in its place).
		expect(document.body.textContent).not.toContain('Ask the owner for the full list');
		// D6: the bar is source-agnostic — no peer name, npub, or "from X".
		expect(bar!.textContent).not.toContain('The Author');
		expect(bar!.textContent).not.toContain(AUTHOR_NPUB);
		expect(/\bfrom\b/i.test(bar!.textContent ?? '')).toBe(false);
	});

	it('progress advances: a second event moves the bar', async () => {
		await driveToPaywall('archive');
		emitProgress({ request_id: 'req-1', slug: 'archive', received: 5242880, total: 10485760 });
		await tick();
		expect(barEl()!.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('50');
	});

	it('the final event (received === total) clears the bar back to the ask affordance', async () => {
		await driveToPaywall('archive');
		emitProgress({ request_id: 'req-1', slug: 'archive', received: 5242880, total: 10485760 });
		await tick();
		expect(barEl()).toBeTruthy();
		emitProgress({ request_id: 'req-1', slug: 'archive', received: 10485760, total: 10485760 });
		await tick();
		expect(barEl(), 'a completed run is no longer in flight').toBeNull();
		expect(document.body.textContent).toContain('Ask the owner for the full list');
	});

	it('is keyed on slug: a DIFFERENT collection\'s event never paints this one', async () => {
		await driveToPaywall('archive');
		emitProgress({ request_id: 'req-9', slug: 'other-vault', received: 5242880, total: 10485760 });
		await tick();
		expect(barEl(), 'a foreign slug must not render a bar here').toBeNull();
		expect(document.body.textContent).toContain('Ask the owner for the full list');

		// And the same slug's event does — proving the null above is the KEY, not a dead listener.
		emitProgress({ request_id: 'req-1', slug: 'archive', received: 5242880, total: 10485760 });
		await tick();
		expect(barEl()).toBeTruthy();
	});

	it('navigating to another collection drops the run — no stale bar crosses a selection', async () => {
		await driveToPaywall('archive');
		emitProgress({ request_id: 'req-1', slug: 'archive', received: 5242880, total: 10485760 });
		await tick();
		expect(barEl()).toBeTruthy();

		// Back out via the BREADCRUMB -- the contact crumb is a `.bc-btn` labelled with the peer name
		// (navigateBc -> kind 'contact' -> selectedCollection = null). This page has no
		// aria-label="Back"; looking for one is how the first draft of this test silently no-opped.
		await crumbBackToPeer();
		await openCard('The Other Vault');
		expect(barEl(), 'a foreign collection shows no bar').toBeNull();

		// THE DISCRIMINATOR: return to the collection the run belonged to. If selectCollection did
		// not clear the map, the stale bar reappears here -- "no bar on the other card" alone would
		// pass even with the clear deleted, since that slug never had an entry.
		await crumbBackToPeer();
		await openCard('The Archive');
		expect(barEl(), 'the run was CLEARED on navigation, not merely hidden by the slug key').toBeNull();
	});

	it('registers the listener on mount and unregisters it on destroy', async () => {
		const { unmount } = render(BrowsePage);
		await waitFor(() => expect(listenHandlers.has('manifest-progress')).toBe(true));
		unmount();
		await tick();
		// Every unlisten fn the page captured (incl. any registered by siblings of the bar) must
		// have been invoked on destroy — a leaked listener keeps a dead page updating state.
		expect(unlisteners.length).toBeGreaterThan(0);
		for (const off of unlisteners) expect(off).toHaveBeenCalled();
	});
});
