// @vitest-environment jsdom
// QURATOR-102 (Browse half) — the "No public collections" empty state was migrated onto the shared
// EmptyState and GAINED a CTA: a peer with no public collections (and no locked listings) now links
// "Ask for access →" into the ask-access chat deep-link, instead of a dead end. This is a BEHAVIOURAL
// mount test: assert the CTA is a real LINK (role=link) whose href carries the peer npub + the
// intent=ask-access param — per §9, asserting the string "Ask for access" anywhere is satisfied by
// the locked-listings button one branch over, so the pin is the anchor, not the word.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { CachedPeer } from '$lib/types.js';

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
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer (the q92-private-collections
// pattern). The npub is inlined because vi.mock is hoisted above every const in the file.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1ctactactactactactactactactactactactactactactactacta') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture id (charset excludes 1/b/i/o after the separator). Length matches the npub in
// the stubbed $page URL above — peerFromQuery matches on the full string.
const PEER_NPUB = 'npub1ctactactactactactactactactactactactactactactactacta';

const PEER: CachedPeer = {
	npub: PEER_NPUB,
	browse_key_hex: 'aabbccdd', // keyed (not listingsLocked) so the empty branch, not the lock, renders
	collections: [], // no PUBLIC collections
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'Empty Peer', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('QURATOR-102 — Browse "No public collections" empty state gains the ask-access CTA', () => {
	it('a peer with no public collections renders a LINK "Ask for access →" into the chat deep-link', async () => {
		contacts.set([PEER]);
		const { getByText, getByRole } = render(BrowsePage);
		await tick();

		await waitFor(() => expect(getByText('No public collections')).toBeTruthy());

		// THE pin: the CTA is a real LINK (not the locked-listings button one branch over), carrying
		// the peer npub + intent=ask-access.
		const link = getByRole('link', { name: /ask for access/i }) as HTMLAnchorElement;
		expect(link.getAttribute('href')).toContain('/chat?peer=' + PEER_NPUB);
		expect(link.getAttribute('href')).toContain('intent=ask-access');
	});
});
