// @vitest-environment jsdom
// QURATOR-79 carrier 4 — the stale-import toast becomes provenance-aware. The old copy
// ("Imported an older version of this list. Ask the owner for a fresh manifest.") is wrong twice
// when a PEER re-served the manifest: the owner cannot be asked (offline — that is why a peer
// answered), and the user cannot tell WHO served the copy. The new copy names the serving peer
// (via the contact list, falling back to shortNpub) and says to ask again once the author is back.
//
// ⚠ WHAT THIS FILE DOES NOT PROVE (QURATOR-172 #1). It mocks `importManifest`, and Browse's real
// backend hardcodes `served_by: None` on that path — so the provenance branches asserted here were
// UNREACHABLE in production for as long as this file was green. It proves the COPY, given a
// provenance value; it never proved one could arrive. REACHABILITY is pinned separately, on the
// redeem path that actually produces the value, by chat-q172-provenance-reachable.test.ts. Keep
// both: this one guards the wording, that one guards the wiring.
//
// This is a BEHAVIOURAL mount test (the q92/q134/q83 pattern): the real Browse page is mounted
// with only `$lib/api.js` mocked, the peer is selected through the `/browse?peer=` deep-link
// (which routes through selectPeer exactly as production does), the truncated collection is
// clicked open, and the manifest is imported through the paywall's paste affordance — the same
// buttons a user presses. The toast is asserted on the `toastMessage` store because the toast
// DOM lives in +layout.svelte, which a route-page render does not mount; the store is what the
// layout renders, so asserting there is asserting the production payload.
//
// The four cases are one derivation apart, so the FILE is the discriminator, not any single
// test: stale+re-served, stale+direct (today's copy, unchanged), fresh+re-served (the lighter
// note), fresh+direct (the plain note).
//
// Per CLAUDE.md §9, a green test proves nothing until seen red. The mutation probes run for
// this file (each applied alone, then reverted) are documented in the lane report:
//   A. `const reServed = result.served_by !== undefined;` → `const reServed = false;`
//      — reds the two re-served cases, leaves the two direct cases green.
//   C. `servingPeerName` drops its contacts lookup (always shortNpub) — reds only the
//      peer-name assertion.
//
// jsdom computes no layout — nothing here proves the toast RENDERS on one line or that the
// paywall fade paints; only that the payload the page hands the toast store is right.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { get } from 'svelte/store';
import BrowsePage from './+page.svelte';
import { contacts, toastMessage } from '$lib/stores.js';
import type { CachedPeer, Collection } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
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

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer (the
// q92/q102 pattern). The npub is inlined because vi.mock is hoisted above every const.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1archarcharcharcharcharcharcharcharcharcharchar') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture ids (charset excludes 1/b/i/o after the separator); length matches the
// npub in the stubbed $page URL — peerFromQuery matches on the full string.
const PEER_NPUB = 'npub1archarcharcharcharcharcharcharcharcharcharchar';
// The serving peer (carrier C): a CONTACT, so the toast must name them by display name — not
// the raw npub, and not the author's name.
const SERVER_NPUB = 'npub1m1ram1ram1ram1ram1ram1ram1ram1ram1ram1ram1r';

// The envelope's own clock. There is deliberately no second (cache) clock: a `cached_at` field was
// declared for months with no producer and was removed in QURATOR-172 #2.
const CREATED_AT = 1_700_000_000;

const TRUNCATED_COL: Collection = {
	slug: 'archive',
	path_alias: 'The Archive',
	item_count: 40,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	// devtest #7 paywall teaser: truncated + total_items > what the listing carries.
	truncated: true,
	total_items: 40,
	snapshot_fingerprint: 'aaaa',
	listing: Array.from({ length: 5 }, (_, i) => ({
		name: `clip-${i}.mkv`,
		item_type: 'File' as const,
		tags: [],
		children: [],
	})),
};

const FULL_COL: Collection = {
	...TRUNCATED_COL,
	truncated: undefined,
	total_items: undefined,
	manifest_imported_at: CREATED_AT,
	listing: Array.from({ length: 40 }, (_, i) => ({
		name: `clip-${i}.mkv`,
		item_type: 'File' as const,
		tags: [],
		children: [],
	})),
};

const PEER: CachedPeer = {
	npub: PEER_NPUB,
	browse_key_hex: 'aabbccdd',
	collections: [TRUNCATED_COL],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'The Author', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

const SERVER: CachedPeer = {
	npub: SERVER_NPUB,
	collections: [],
	online: true,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'Mira', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	// A sticky ERROR toast would block later SUCCESS toasts (stores.ts: blockedByStickyError),
	// so the store must be reset between cases or test order changes the outcome.
	toastMessage.set(null);
});

import { importManifest } from '$lib/api.js';
const importMock = importManifest as unknown as ReturnType<typeof vi.fn>;

/** Drive the real page to a completed manifest import and hand back the live toast. */
async function importThroughThePage(
	result: { stale: boolean; served_by?: string },
): Promise<{ text: string; kind: 'success' | 'error' } | null> {
	importMock.mockResolvedValue({ slug: 'archive', collection: FULL_COL, created_at: CREATED_AT, ...result });
	contacts.set([PEER, SERVER]);
	render(BrowsePage);
	await tick();

	// Open the truncated collection — the paywall block with the import affordances is inside.
	await waitFor(() => expect(document.body.textContent).toContain('The Archive'));
	const card = document.querySelector<HTMLButtonElement>('.col-card');
	expect(card).toBeTruthy();
	await fireEvent.click(card!);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('more item'));

	// The paste affordance, then the import — the buttons a user actually presses.
	await fireEvent.click([...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'or paste it')!);
	await tick();
	const area = document.querySelector<HTMLTextAreaElement>('.paywall-paste');
	expect(area).toBeTruthy();
	await fireEvent.input(area!, { target: { value: 'eyJoYm1hbmlmZXN0IjoiMSJ9' } });
	const importBtn = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Import from text');
	expect(importBtn).toBeTruthy();
	await fireEvent.click(importBtn!);

	await waitFor(() => expect(get(toastMessage)).not.toBeNull());
	return get(toastMessage);
}

describe('QURATOR-79 carrier 4 — the import toast names who served the copy', () => {
	it('stale + re-served: names the serving peer and that the author is offline', async () => {
		const toast = await importThroughThePage({ stale: true, served_by: SERVER_NPUB });
		// The serving peer is named from the contact list (display name), never the raw npub.
		expect(toast?.text).toContain('Mira');
		expect(toast?.text).not.toContain(SERVER_NPUB);
		// No date is claimed: there is no cache clock, and the envelope's own clock is the AUTHOR's
		// writing time, so rendering it here would state a falsehood about when the copy was taken.
		expect(toast?.text).not.toContain(new Date(CREATED_AT * 1000).toLocaleDateString());
		// The author is offline — the whole reason a peer answered — and the ask is deferred.
		expect(toast?.text).toContain('offline');
		expect(toast?.text).not.toContain('Ask the owner for a fresh manifest');
		expect(toast?.kind).toBe('error');
	});

	it('stale + served directly by the author: today\'s copy, unchanged', async () => {
		const toast = await importThroughThePage({ stale: true });
		expect(toast?.text).toBe('Imported an older version of this list. Ask the owner for a fresh manifest.');
		expect(toast?.text).not.toContain('Mira');
		expect(toast?.kind).toBe('error');
	});

	it('fresh + re-served: the lighter note that it came from a peer\'s cached copy', async () => {
		const toast = await importThroughThePage({ stale: false, served_by: SERVER_NPUB });
		expect(toast?.text).toContain("Full manifest imported from Mira's cached copy");
		expect(toast?.text).not.toContain('older');
		expect(toast?.kind).toBe('success');
	});

	it('fresh + served directly: the plain note, no provenance', async () => {
		const toast = await importThroughThePage({ stale: false });
		expect(toast?.text).toBe('Full manifest imported');
		expect(toast?.text).not.toContain('cached');
		expect(toast?.kind).toBe('success');
	});
});
