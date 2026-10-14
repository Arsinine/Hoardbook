// @vitest-environment jsdom
// QURATOR-374 — Browse's per-collection size line must top out at TB like the shared fmtBytes.
// Before this: Browse carried a local decimal GB-topping copy, so a peer collection with
// total_bytes 4.2e12 and no est_size read "4200.0 GB" while Contacts/Discover read "4.2 TB".
// The local copy is deleted; Browse imports the shared $lib/discover-book fmtBytes.
//
// BEHAVIOURAL mount test (the q342 pattern): the real page mounted with only `$lib/api.js`
// mocked, selection through the `/browse?peer=` deep-link (routes through selectPeer exactly
// as production does); TitleSearch is the TitleSearchProbe stand-in (see q342 for why).
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
	getContacts: vi.fn().mockResolvedValue([]),
	applyKeyGrants: vi.fn().mockResolvedValue([]),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// TitleSearch stand-in: async factory + dynamic import (no hoisting hazards) — the q342 probe.
vi.mock('$lib/components/TitleSearch.svelte', async () => {
	const { default: Probe } = await import('./TitleSearchProbe.svelte');
	return { default: Probe };
});

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer (the q134 pattern).
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1q374q374q374q374q374q374q374q374q374q374q374q374q374') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture id (charset excludes 1/b/i/o after the separator); length matches the npub
// in the stubbed $page URL — peerFromQuery matches on the full string.
const PEER_NPUB = 'npub1q374q374q374q374q374q374q374q374q374q374q374q374q374';

/** Keyed peer whose single collection is > 1e12 bytes with NO est_size — the exact row that
 *  used to read "4200.0 GB" through Browse's deleted local formatter. */
function peerWithTbCollection(): ContactSummary {
	return {
		npub: PEER_NPUB,
		has_browse_key: true,
		collections: [
			{ slug: 'big-hoard', path_alias: 'Big Hoard', item_count: 3, total_bytes: 4.2e12, content_types: [], tags: [], languages: [] },
		],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		profile: {
			tags: [], languages: [], social_links: [], willing_to: [], content_types: [],
			updated: '2026-08-01T00:00:00Z', display_name: 'TB Peer',
		},
	} as unknown as ContactSummary;
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	delete (window as unknown as { __TS_PROBE_NPUB__?: string }).__TS_PROBE_NPUB__;
});

import { refreshContact } from '$lib/api.js';
const refreshMock = refreshContact as unknown as ReturnType<typeof vi.fn>;

describe('QURATOR-374 — Browse sizes top out at TB (shared fmtBytes)', () => {
	it('a >1e12 collection with no est_size renders "4.2 TB", never the GB-topped "4200.0 GB"', async () => {
		// P-10 MUTATION: in routes/browse/+page.svelte alias the import to
		// `fmtBytes as sharedFmtBytes` and re-add a local GB-only
		// `function fmtBytes(bytes){>1e9 GB / >1e6 MB / >1e3 KB / B}` → REDS
		// (the row falls back to "4200.0 GB").
		refreshMock.mockResolvedValue(peerWithTbCollection());
		contacts.set([peerWithTbCollection()]);
		render(BrowsePage);
		await tick();

		// The card the deep-linked peer's grid renders — assert the meta line itself, not the
		// whole body (the People list legitimately carries other size strings).
		await waitFor(() => {
			const meta = Array.from(document.querySelectorAll('.col-card-meta'))
				.find((el) => el.textContent?.includes('TB'));
			expect(meta?.textContent).toContain('4.2 TB');
		});
		expect(document.body.textContent).not.toContain('4200.0 GB');
	});
});
