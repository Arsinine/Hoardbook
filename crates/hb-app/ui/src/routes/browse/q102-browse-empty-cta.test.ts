// @vitest-environment jsdom
// QURATOR-102 (Browse half) — the "No public collections" empty state used to carry a CTA
// linking "Ask for access →" into the ask-access chat deep-link. QURATOR-342 (owner ruling
// 2026-09-27) RETIRED the ask ramp: no Ask-for-access affordance anywhere on Browse, and "No
// public collections" is not a locked state — it now renders as the plain message. This is a
// BEHAVIOURAL mount test (the q92/q134 pattern): assert the message renders and NO ask
// affordance (link or button) exists anywhere on the page.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary } from '$lib/types.js';

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

const PEER: ContactSummary = {
	npub: PEER_NPUB,
	has_browse_key: true, // keyed (not unreadable) so the empty branch, not the teaser, renders
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

describe('QURATOR-342 — Browse "No public collections" carries no ask affordance', () => {
	it('a peer with no public collections renders the plain message and NO "Ask for access" link or button anywhere', async () => {
		contacts.set([PEER]);
		const { getByText } = render(BrowsePage);
		await tick();

		await waitFor(() => expect(getByText('No public collections')).toBeTruthy());

		// THE pin (ruling 2026-09-27): the ask CTA is retired — no affordance named for it may
		// render anywhere on the page, in any branch.
		expect(document.body.textContent).not.toContain('Ask for access');
		const asks = [...document.querySelectorAll('a, button')].filter((el) =>
			/ask for access/i.test(el.textContent ?? ''),
		);
		expect(asks).toEqual([]);
	});
});
