// @vitest-environment jsdom
// QURATOR-79 carrier 4 — the ASK-ORIGINATION slice originally pinned the Browse paywall's
// "ask a contact" affordance (picker + `request_manifest_from`). QURATOR-203 deleted that
// affordance from the paywall markup: fetch/serve are now fully automatic (fetch_driver.rs,
// auto_approve.rs), so the manual ask-a-contact round trip is dead chrome. This file now pins the
// ABSENCE of that affordance instead of its behaviour.
//
// This is a BEHAVIOURAL mount test (the q83/q79-provenance pattern): the real Browse page is
// mounted with only `$lib/api.js` mocked, the peer is selected through the `/browse?peer=` deep-link
// (which routes through selectPeer exactly as production does), and the truncated collection is
// clicked open so the paywall block renders. A source-scan is NOT acceptable for this (CLAUDE.md →
// P-4): the property is what the mounted DOM does or does not expose.
//
// Per CLAUDE.md §9 / P-10, a green absence test proves nothing until seen red. Mutation probe
// (applied, confirmed red, then reverted; see the lane report): re-adding the
// `<button class="btn-ghost btn-sm" onclick={...}>Ask a contact for this list</button>` markup to
// the paywall block reds "the ask-a-contact affordance no longer appears in the paywall block".
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts, toastMessage } from '$lib/stores.js';
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

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer (the
// q92/q102/q79 pattern). The npub is inlined because vi.mock is hoisted above every const.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1archarcharcharcharcharcharcharcharcharcharchar') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture ids (charset excludes 1/b/i/o after the separator); length matches the npub
// in the stubbed $page URL — peerFromQuery matches on the full string.
const AUTHOR_NPUB = 'npub1archarcharcharcharcharcharcharcharcharcharchar';
// Peer C: the contact D asks. A CONTACT, so the picker must offer them by display name.
const CONTACT_NPUB = 'npub1m1ram1ram1ram1ram1ram1ram1ram1ram1ram1ram1r';

const TRUNCATED_COL: Collection = {
	slug: 'archive',
	path_alias: 'The Archive',
	item_count: 40,
	total_bytes: 0,
	content_types: ['video'],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	// devtest #7 paywall teaser: truncated + total_items > what the listing carries. The paywall
	// block — where the ask affordance lives — only renders for a truncated collection.
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

const AUTHOR_PEER: CachedPeer = {
	npub: AUTHOR_NPUB,
	browse_key_hex: 'aabbccdd',
	collections: [TRUNCATED_COL],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'The Author', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

const CONTACT_PEER: CachedPeer = {
	npub: CONTACT_NPUB,
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
	toastMessage.set(null);
});

import { requestManifestFrom, requestManifest } from '$lib/api.js';
const askFromMock = requestManifestFrom as unknown as ReturnType<typeof vi.fn>;
const askOwnerMock = requestManifest as unknown as ReturnType<typeof vi.fn>;

/** Drive the real page to the open truncated-collection paywall block. */
async function driveToPaywall(): Promise<void> {
	contacts.set([AUTHOR_PEER, CONTACT_PEER]);
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

describe('QURATOR-79 carrier 4 — ask origination (D asks C for A\'s manifest)', () => {
	it('the ask-a-contact affordance no longer appears in the paywall block', async () => {
		await driveToPaywall();
		// QURATOR-203: fetch/serve are now fully automatic; the manual "ask a contact" round trip
		// (button, picker, "Ask them") was deleted from the paywall markup. Neither the trigger
		// button, the contact picker, nor the hint text should be reachable from the mounted DOM.
		const openBtn = [...document.querySelectorAll('button')].find(
			(b) => b.textContent?.trim() === 'Ask a contact for this list',
		);
		expect(openBtn).toBeUndefined();
		expect(document.querySelector('.ask-contact-select')).toBeNull();
		expect(document.querySelector('.ask-contact-hint')).toBeNull();
		const askThemBtn = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Ask them');
		expect(askThemBtn).toBeUndefined();
		// Neither ask path is ever invoked, since neither trigger exists anymore.
		expect(askFromMock).not.toHaveBeenCalled();
		expect(askOwnerMock).not.toHaveBeenCalled();
	});
});
