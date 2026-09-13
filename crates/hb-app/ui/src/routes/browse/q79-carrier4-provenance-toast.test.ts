// @vitest-environment jsdom
// QURATOR-79 carrier 4 — the stale-import toast was made provenance-aware (names the serving peer,
// says the author is offline rather than "Ask the owner for a fresh manifest"). QURATOR-203 then
// deleted the paywall's manual import affordances ("or paste it" / "Import from text") as dead
// round-trip chrome: fetch/serve are now fully automatic (fetch_driver.rs, auto_approve.rs), so a
// user can no longer reach `importManifest` through the paywall UI at all. The provenance-copy
// behaviour this file used to exercise is therefore unreachable from the mounted page; this file
// now pins that unreachability instead.
//
// ⚠ Historical note (QURATOR-172 #1), kept for context: the provenance branches were already
// unreachable in PRODUCTION even before this slice (Browse's backend hardcoded `served_by: None`
// on the import path) — reachability was pinned separately by
// chat-q172-provenance-reachable.test.ts. That test is untouched by this slice.
//
// This is a BEHAVIOURAL mount test (the q92/q134/q83 pattern): the real Browse page is mounted
// with only `$lib/api.js` mocked, the peer is selected through the `/browse?peer=` deep-link, and
// the truncated collection is clicked open so the paywall block renders. A source-scan is NOT
// acceptable for this (CLAUDE.md → P-4): the property is what the mounted DOM does or does not
// expose.
//
// Per CLAUDE.md §9 / P-10, a green absence test proves nothing until seen red. Mutation probe
// (applied, confirmed red, then reverted; see the lane report): re-adding the
// `<button ...>or paste it</button>` markup to the paywall block reds
// "the manual import (paste) affordance no longer appears in the paywall block".
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

/** Drive the real page to the open truncated-collection paywall block. */
async function driveToPaywall(): Promise<void> {
	importMock.mockResolvedValue({ slug: 'archive', collection: FULL_COL, created_at: CREATED_AT, stale: false });
	contacts.set([PEER, SERVER]);
	render(BrowsePage);
	await tick();

	// Open the truncated collection — the paywall block is inside.
	await waitFor(() => expect(document.body.textContent).toContain('The Archive'));
	const card = document.querySelector<HTMLButtonElement>('.col-card');
	expect(card).toBeTruthy();
	await fireEvent.click(card!);
	await tick();
	await waitFor(() => expect(document.body.textContent).toContain('more item'));
}

describe('QURATOR-79 carrier 4 — the import toast names who served the copy', () => {
	it('the manual import (paste) affordance no longer appears in the paywall block', async () => {
		await driveToPaywall();
		// QURATOR-203: fetch/serve are now fully automatic; the manual "or paste it" / "Import from
		// text" round trip was deleted from the paywall markup, so `importManifest` (and the
		// provenance-aware toast copy it used to feed) is no longer reachable from this UI.
		const pasteBtn = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'or paste it');
		expect(pasteBtn).toBeUndefined();
		expect(document.querySelector('.paywall-paste')).toBeNull();
		const importBtn = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Import from text');
		expect(importBtn).toBeUndefined();
		expect(importMock).not.toHaveBeenCalled();
		expect(get(toastMessage)).toBeNull();
	});
});
