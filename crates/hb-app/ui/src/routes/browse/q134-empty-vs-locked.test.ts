// @vitest-environment jsdom
// QURATOR-134 — a keyless contact with ZERO published collections rendered "Loading
// collections…" then "🔒 Listings locked". Root cause: `listingsLocked` keyed on
// `collections.length === 0`, which cannot distinguish "sealed and undecryptable" from
// "they published nothing" — both are an empty array after the resolve. The branch order
// compounded it: the locked branch matched before the (already-written, DEAD) "No public
// collections" branch for every keyless peer.
//
// This is a BEHAVIOURAL mount test (the q92/q102 pattern): Browse is mounted with only
// `$lib/api.js` mocked, the peer is selected through the `/browse?peer=` deep-link (which
// routes through selectPeer exactly as production does), and `refreshContact` resolves the
// tri-state `listings_state` the hb-app command now computes from hb-net's enumeration —
// the UI never re-derives the distinction (CLAUDE.md §9).
//
// WHICH TEST PINS WHICH STATE — the three states are one derivation apart, so the pair is
// the discriminator, not any single test:
//   • state 1 (nothing published -> "No public collections")  — RED on the broken code
//     (the old `listingsLocked` shows 🔒 for EVERY keyless empty peer).
//   • state 3 (fetch failed -> error + Retry)                 — RED on the broken code
//     (no error branch existed; a failed enumeration read as 🔒).
//   • state 2 (sealed -> 🔒 Listings locked)                  — GREEN on the broken code
//     *for the wrong reason* (the old derivation shows 🔒 regardless of what the peer
//     published). Its guard value is against regression: revert the fix and test 1 reds;
//     break the Sealed mapping alone and THIS test reds while test 1 stays green.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red. The pre-fix RED run and the
// post-fix mutation probes are documented in the task report.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
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

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer (the
// q92/q102 pattern). The npub is inlined because vi.mock is hoisted above every const.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1q134q134q134q134q134q134q134q134q134q134q134q134q134') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture id (charset excludes 1/b/i/o after the separator); length matches the
// npub in the stubbed $page URL — peerFromQuery matches on the full string.
const PEER_NPUB = 'npub1q134q134q134q134q134q134q134q134q134q134q134q134q134';

/** A KEYLESS contact (no browse_key_hex — the owner's exact scenario). */
function keylessPeer(listings_state: 'Fetched' | 'Sealed' | 'FetchFailed'): CachedPeer {
	return {
		npub: PEER_NPUB,
		// deliberately NO browse_key_hex — keyless
		collections: [],
		online: false,
		last_fetched: '2026-08-01T00:00:00Z',
		local_tags: [],
		listings_state,
		profile: { display_name: 'Bare Peer', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
	} as CachedPeer & { listings_state: 'Fetched' | 'Sealed' | 'FetchFailed' };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

import { refreshContact } from '$lib/api.js';
const refreshMock = refreshContact as unknown as ReturnType<typeof vi.fn>;

describe('QURATOR-134 — zero published ≠ locked ≠ failed', () => {
	it('state 1: keyless contact, fetch found NO listing events -> "No public collections", never 🔒', async () => {
		refreshMock.mockResolvedValue(keylessPeer('Fetched'));
		contacts.set([keylessPeer('Fetched')]);
		render(BrowsePage);
		await tick();

		await waitFor(() => expect(document.body.textContent).toContain('No public collections'));
		// The lock must NOT render — that is the owner's bug.
		expect(document.body.textContent).not.toContain('Listings locked');
	});

	it('state 2: keyless contact, listing events EXIST but none decryptable -> 🔒 Listings locked + Ask for access', async () => {
		refreshMock.mockResolvedValue(keylessPeer('Sealed'));
		contacts.set([keylessPeer('Sealed')]);
		const { getByText, getByRole } = render(BrowsePage);
		await tick();

		await waitFor(() => expect(getByText('🔒 Listings locked')).toBeTruthy());
		// The ask-access ramp (M17 W2) rides the genuine locked case.
		expect(getByRole('button', { name: /ask for access/i })).toBeTruthy();
		// …and the confident negative must NOT render.
		expect(document.body.textContent).not.toContain('No public collections');
	});

	it('state 3: the listings fetch FAILED -> error + Retry, not a confident negative and not 🔒', async () => {
		refreshMock.mockResolvedValue(keylessPeer('FetchFailed'));
		contacts.set([keylessPeer('FetchFailed')]);
		const { getByRole } = render(BrowsePage);
		await tick();

		// The retryable error EmptyState (the QURATOR-93 machinery), with a working Retry.
		const retry = await waitFor(() => getByRole('button', { name: 'Retry' }));
		expect(document.body.textContent).not.toContain('No public collections');
		expect(document.body.textContent).not.toContain('Listings locked');

		// Retry re-runs the refresh — the affordance is wired, not decorative.
		refreshMock.mockClear();
		await fireEvent.click(retry);
		await waitFor(() => expect(refreshMock).toHaveBeenCalledTimes(1));
		expect(refreshMock).toHaveBeenCalledWith(PEER_NPUB);
	});
});
