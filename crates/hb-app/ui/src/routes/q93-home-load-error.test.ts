// @vitest-environment jsdom
// QURATOR-93 (Home half) — a FAILED `get_collections` used to render as the confident empty state
// "No collections yet. Click 'Add collection'…". The layout's `.catch(() => {})` left the store
// empty, and the page's `$collections.length === 0` branch couldn't tell "no data" from "no rows".
//
// BEHAVIOURAL mount tests (not a source-scan): the page is mounted with the api mocked to REJECT,
// and the assertions target the AFFORDANCES (role=alert, role=button named Retry) plus the ABSENCE
// of the confident empty string — per §9, asserting the word "retry" anywhere is satisfied by prose.
//
// QURATOR-80 rule both ways: a later successful fetch clears the error (second test), and the
// genuine-empty string never renders while the error holds (first test).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import HomePage from './+page.svelte';
import {
	identity, profile, collections, appReady, homeDraft, identityLoadError, collectionsLoadError,
} from '$lib/stores.js';
import type { IdentityInfo, Profile, Collection } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		// The load under test: starts REJECTING; individual tests flip it.
		getCollections: vi.fn(),
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
	};
});

import { getCollections } from '$lib/api.js';
const getCollectionsMock = getCollections as unknown as ReturnType<typeof vi.fn>;

const IDENT: IdentityInfo = {
	npub: 'npub1q93' + 'a'.repeat(54),
	npub_short: 'npub1q93…aaaa',
	share_code: 'hbk1q93test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Load Error Tester',
	bio: undefined,
	tags: [],
	since: undefined,
	est_size: undefined,
	languages: [],
	contact_hint: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

const ONE_COLLECTION: Collection = {
	slug: 'ebooks',
	path_alias: 'Ebooks',
	item_count: 3,
	total_bytes: 1024,
	content_types: [],
	tags: [],
	languages: [],
	last_updated: '2026-08-01T00:00:00Z',
	listing: [],
	published: false,
};

const EMPTY_STRING = /no collections yet/i;
const ERROR_STRING = /couldn.t load your collections/i;

function resetStores() {
	identity.set(IDENT);
	profile.set({ ...PROF });
	collections.set([]);
	collectionsLoadError.set(false);
	appReady.set(true);
	homeDraft.set({ ...PROF });
	identityLoadError.set(null);
}

afterEach(() => {
	cleanup();
	resetStores();
	// Leave the page's post-test state clean for the next file too.
	collections.set([]);
	collectionsLoadError.set(false);
	appReady.set(false);
	identity.set(null);
	profile.set(null);
	homeDraft.set(null);
	vi.clearAllMocks();
});

describe('QURATOR-93 — Home collections load failure is not a confident empty', () => {
	it('load REJECTS → error alert + Retry render; the confident "No collections yet" does NOT', async () => {
		getCollectionsMock.mockRejectedValue(new Error('scan catalog unavailable'));
		resetStores();
		// Simulate the layout's failed seed: the helper sets the flag on failure.
		const { loadCollectionsInto } = await import('$lib/stores.js');
		void loadCollectionsInto(getCollections);

		const { getByRole, queryByText } = render(HomePage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		// The Retry affordance is a BUTTON (not the word appearing in prose).
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		// THE core assertion: the confident negative must NOT render on a failed load.
		expect(queryByText(EMPTY_STRING)).toBeNull();
		expect(queryByText(ERROR_STRING)).toBeTruthy();
	});

	it('Retry re-fetches: reject → resolve → the list renders and the error is gone', async () => {
		getCollectionsMock.mockRejectedValueOnce(new Error('first attempt fails'));
		resetStores();
		const { loadCollectionsInto } = await import('$lib/stores.js');
		void loadCollectionsInto(getCollections);

		const { getByRole, queryByRole, findByText } = render(HomePage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		// The retry path now succeeds (the layout fetch is done through the same helper, so its
		// success clears the flag — the both-directions rule).
		getCollectionsMock.mockResolvedValue([ONE_COLLECTION]);
		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		// The list actually lands (the user's real goal)…
		expect(await findByText('Ebooks')).toBeTruthy();
		// …the error alert is gone, and the genuine-empty string never appeared.
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});

	it('a SUCCESSFUL load renders the genuine empty state, not the error', async () => {
		getCollectionsMock.mockResolvedValue([]);
		resetStores();
		const { loadCollectionsInto } = await import('$lib/stores.js');
		await loadCollectionsInto(getCollections);

		const { queryByRole, findByText } = render(HomePage);
		expect(await findByText(EMPTY_STRING)).toBeTruthy();
		expect(queryByRole('alert')).toBeNull();
	});
});
