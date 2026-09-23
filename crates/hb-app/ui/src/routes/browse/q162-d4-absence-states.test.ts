// @vitest-environment jsdom
// QURATOR-162 lane B, D4 — the absence-state check. QURATOR-134 pinned the tri-state of the
// COLLECTIONS GRID (Fetched / Sealed / FetchFailed); QURATOR-159 pinned the progress bar that
// appears once an in-flight manifest fetch streams bytes. The state between them — the ask has
// been MINTED (background auto-fetch, QURATOR-164) but ZERO bytes have arrived yet — is the one
// absence state with no dedicated guard: no progress event has fired, so `paywallProgress` is
// null and the pane shows the paywall teaser alone. Per the D4 rule this must render as the
// honest "there is more" teaser, NEVER as a confident negative. Ruling D7 (see the production
// comment on the paywall block): no stalled/"never answered" state exists by design — tickets
// never expire and there is no timeout event to key on — so the teaser-without-bar IS the
// asked-no-answer rendering, and this file pins that it is honest, never negative.
//
// (a) "never fetched" and (b) "prefetched but stale" have NO distinct UI state: see the findings
// in the lane report. (a) is a BACKEND defect (topics.rs `upsert_topic_contact` stamps the
// honest-empty `Fetched` default on a contact whose listings were never enumerated), not a UI
// rendering — a fixture-level UI test cannot reach it and would only pin the defect.
//
// BEHAVIOURAL mount test (q159 pattern): the real Browse page mounts with only `$lib/api.js`
// (and the Tauri/navigation modules) mocked; `@tauri-apps/api/event`'s `listen` mock captures
// the handler the page registers, so a later lane can fire producer events at it. The
// asked-no-answer case needs no event at all: it is the pre-event baseline.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red. The mutation (applied alone,
// then reverted; backup held OUTSIDE the tree, restore verified by diff):
//   M. in the template, gate the paywall block off (`{#if false && paywall}`) — the
//      asked-no-answer state then renders NOTHING honest (the pane shows the partial listing
//      with no teaser), and this test reds on the hidden-count and sub-copy assertions.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary, Collection } from '$lib/types.js';

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
	// The page pulls pending browse-key grants on mount (QURATOR-188); q159's mock predates it.
	applyKeyGrants: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// The producer stand-in: capture the handlers so the D4 contract ("bytes arrive" comes LATER)
// can be fired by a future test without re-deriving the mount. Unused in the red path.
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
// q92/q102/q79/q159 pattern). Keyed peer: the paywall/ask flow is a keyed browse.
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

// The truncated-teaser shape is the ONLY shape that carries the paywall block: 5 items shown of
// 40 published. hidden = 35, so the asked-no-answer copy must read "35 more items hidden".
const PAYWALL_COL: Collection = {
	slug: 'archive',
	path_alias: 'The Archive',
	item_count: 40,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
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

const AUTHOR_PEER: ContactSummary = {
	npub: AUTHOR_NPUB,
	has_browse_key: true,
	collections: [PAYWALL_COL],
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

/** Mount, select the peer (deep-link) and open the truncated collection → the paywall block. */
async function driveToPaywall() {
	contacts.set([AUTHOR_PEER]);
	render(BrowsePage);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('The Archive'));
	const card = [...document.querySelectorAll<HTMLButtonElement>('.col-card')].find(
		(c) => (c.textContent ?? '').includes('The Archive'),
	);
	expect(card, 'the truncated collection card must render').toBeTruthy();
	await fireEvent.click(card!);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('more item'));
}

describe('QURATOR-162 D4 — the asked-no-answer absence state renders honest, never negative', () => {
	it('zero progress events: the teaser alone carries the ask — honest hidden-count, no bar, no confident negative anywhere', async () => {
		await driveToPaywall();

		// No `manifest-progress` event has fired (received === 0, nothing in flight keyed to this
		// slug), so the bar must NOT render — D7 ruled there is no stalled state, and the bar is
		// the bytes-flowing state, not the asked state.
		expect(document.querySelector('.paywall-progress'), 'no bar before any progress event (D7: the ask itself has no indicator by ruling)').toBeNull();

		// …but the ask state is still HONEST: the teaser names the withheld rest.
		expect(document.body.textContent).toContain('35 more items hidden');
		expect(document.body.textContent).toContain('too large to publish in full');
		// The partial listing is visible alongside (an honest "here is some of it", not an empty).
		expect(document.body.textContent).toContain('clip-0.mkv');

		// THE D4 RULE: no confident-negative copy anywhere in the collection view while the ask
		// is pending. These are the four shipped-4-times failure shapes (QURATOR-83/134/135):
		const text = document.body.textContent ?? '';
		expect(text).not.toContain('No public collections');
		expect(text).not.toContain('Empty folder');
		expect(text).not.toContain("Couldn't load collections");
		expect(text).not.toContain('No items match your filters');
	});
});
