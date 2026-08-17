// @vitest-environment jsdom
// QURATOR-92 — a peer's Private collections (sealed TO US, M10 `browse_private_collections`)
// were listed in the Contacts card detail but UNVIEWABLE: the Browse page never called the api
// at all, so the only surface was an inert row in Contacts. This is a BEHAVIOURAL mount test:
// the bug is "no fetch happens and no section renders", and the only honest way to pin that is
// to mount Browse with the api mocked and assert the Private section + badge + the collection's
// items actually render for the selected peer.
//
// Guardrails pinned alongside (M21 W4 lock): private collections must NOT inflate the public
// side — the "No public collections" empty state still shows for a peer whose only collections
// are private.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (drop the browsePrivateCollections fetch, re-run this file) is documented in the task
// brief; the initial RED run is the pre-fix evidence.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { CachedPeer, Collection, PrivatePeerCollections } from '$lib/types.js';

// The api mock — every Tauri command Browse imports is stubbed. browsePrivateCollections is the
// spy under test; the others just need to resolve so the page's mount effects don't throw.
vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer, exactly
// the entry the Contacts card detail now links to. The npub is inlined here because vi.mock is
// hoisted above every const declaration in the file.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1pr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0be') }) };
});
vi.mock('$app/stores', () => stubPage);

import { browsePrivateCollections } from '$lib/api.js';
const privateMock = browsePrivateCollections as unknown as ReturnType<typeof vi.fn>;

// bech32-safe fixture ids (charset excludes 1/b/i/o after the separator). Length matches the
// npub in the stubbed $page URL above — peerFromQuery matches on the full string.
const PEER_NPUB = 'npub1pr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0be';

const PRIVATE_COL: Collection = {
	slug: 'vault',
	path_alias: 'The Vault',
	description: 'sealed to me',
	item_count: 1,
	total_bytes: 0,
	content_types: ['video'],
	tags: ['rare'],
	languages: [],
	visibility: 'Private',
	sorted: false,
	last_updated: '2026-08-01T00:00:00Z',
	listing: [{ name: 'rare-clip.mkv', item_type: 'File', size: '9GB', tags: [], children: [] }],
};

const PEER: CachedPeer = {
	npub: PEER_NPUB,
	browse_key_hex: 'aabbccdd',
	collections: [], // no PUBLIC collections — the private section is the only content
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'Sealed Peer', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-92 — a peer\'s Private collections are viewable in Browse', () => {
	it('fetches browse_private_collections and renders a badged Private section for the selected peer', async () => {
		privateMock.mockResolvedValue([{ npub: PEER_NPUB, collections: [PRIVATE_COL] } as PrivatePeerCollections]);
		contacts.set([PEER]);
		render(BrowsePage);
		await tick();

		await waitFor(() => {
			expect(privateMock).toHaveBeenCalled();
		});
		// The Private section renders for the selected peer, with the Private badge on the card.
		await waitFor(() => {
			expect(document.body.textContent).toContain('Private collections');
		});
		expect(document.querySelector('.private-collections .private-pill')).toBeTruthy();

		// Guardrail (M21 W4): the public side is NOT inflated — a peer with only private
		// collections still shows the public empty state.
		expect(document.body.textContent).toContain('No public collections');
	});

	it('clicking a Private collection card opens its sealed listing (the items render)', async () => {
		privateMock.mockResolvedValue([{ npub: PEER_NPUB, collections: [PRIVATE_COL] } as PrivatePeerCollections]);
		contacts.set([PEER]);
		const { getByText } = render(BrowsePage);
		await tick();

		await waitFor(() => {
			expect(getByText('The Vault')).toBeTruthy();
		});
		await fireEvent.click(getByText('The Vault'));
		await tick();

		// The file view now shows the sealed collection's item — the actual QURATOR-92 fix.
		await waitFor(() => {
			expect(getByText('rare-clip.mkv')).toBeTruthy();
		});
	});

	it('a peer with no private entry renders nothing new (no empty Private section)', async () => {
		privateMock.mockResolvedValue([]);
		contacts.set([PEER]);
		render(BrowsePage);
		await tick();
		await waitFor(() => {
			expect(document.body.textContent).toContain('No public collections');
		});
		expect(document.body.textContent).not.toContain('Private collections');
	});
});
