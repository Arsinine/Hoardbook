// @vitest-environment jsdom
// QURATOR-95 — Home claimed "Not published yet" on a profile that IS published.
//
// `publishedSnapshot` was captured only inside onMount, and only if `$profile` was ALREADY truthy
// when `hasPublishedProfile()` resolved. The two hydrate on parallel chains (the layout's
// `getProfile()` vs the page's published-check), so whenever the check wins the race the snapshot
// is lost for the whole session: `neverPublished` stays true, the banner reads "Not published yet"
// and the Publish button pulses on an already-published profile.
//
// BEHAVIOURAL test (real mount + deferred store hydration), not a source-scan: the bug is a
// resolve-ordering race and only controlling the exact sequence can reproduce it — the same
// deferred-settle pattern as QURATOR-83/85.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (delete ONLY the snapshot-capturing $effect, keep the onMount half, re-run this file)
// MUST fail the first test and leave the others as they were.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import HomePage from './+page.svelte';
import { identity, profile, collections, appReady, homeDraft, identityLoadError } from '$lib/stores.js';
import type { IdentityInfo, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
	};
});

import { hasPublishedProfile } from '$lib/api.js';
const hasPubMock = hasPublishedProfile as unknown as ReturnType<typeof vi.fn>;

const IDENT: IdentityInfo = {
	npub: 'npub1q95' + 'a'.repeat(54),
	npub_short: 'npub1q95…aaaa',
	share_code: 'hbk1q95test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Race Tester',
	bio: undefined,
	tags: ['books'],
	since: 2020,
	est_size: undefined,
	languages: ['English'],
	contact_hint: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: ['seed'],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

function resetStores() {
	identity.set(null);
	profile.set(null);
	collections.set([]);
	appReady.set(false);
	homeDraft.set(null);
	identityLoadError.set(null);
}

afterEach(() => {
	cleanup();
	resetStores();
	vi.clearAllMocks();
});

describe('QURATOR-95 — a published profile is never reported as "Not published yet"', () => {
	// THE race from the live report: hasPublishedProfile → true resolves instantly; the profile
	// store hydrates later on the layout's parallel chain. The old one-shot onMount check read
	// `$profile` while it was still null and never re-derived.
	it('published-check resolves BEFORE the profile store hydrates → Published state, no pulse', async () => {
		hasPubMock.mockResolvedValue(true);
		// homeDraft primes the editor form, so form === the incoming profile (a clean "Published",
		// not "Unpublished changes" — that way this pins the exact pub-ok branch).
		homeDraft.set({ ...PROF });
		identity.set(IDENT);
		appReady.set(true); // profile store is STILL null — getProfile() is in flight

		const { container, queryByText, findByText } = render(HomePage);

		// Let the published-check fire AND resolve, while $profile is still null.
		await waitFor(() => expect(hasPubMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));

		// NOW the layout's getProfile() lands — after the check already resolved.
		profile.set({ ...PROF });
		await tick();
		await new Promise((r) => setTimeout(r, 20));

		// THE ASSERTION: an already-published profile must show the Published state.
		expect(queryByText(/Not published yet/i)).toBeNull();
		expect(await findByText(/discoverable in search/i)).toBeTruthy();
		// The pulse is the second live symptom (Publish pulsing on a published profile).
		expect(container.querySelector('.publish-pulse')).toBeNull();
	});

	// Counterpart: the fix must not flip genuinely-never-published profiles to "Published".
	it('published-check resolves false → "Not published yet" still shows and the button still pulses', async () => {
		hasPubMock.mockResolvedValue(false);
		homeDraft.set({ ...PROF });
		profile.set({ ...PROF });
		identity.set(IDENT);
		appReady.set(true);

		const { container, getByText, getByRole } = render(HomePage);
		await waitFor(() => expect(hasPubMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));

		expect(getByText(/Not published yet/i)).toBeTruthy();
		const pubBtn = getByRole('button', { name: /publish profile/i });
		expect(pubBtn.classList.contains('publish-pulse')).toBe(true);
	});

	// The opposite order (profile first, check later) worked even on the broken code — the onMount
	// await resumed into a non-null store. Kept as a guard so the join covers BOTH orders.
	it('profile store hydrates BEFORE the published-check resolves → Published state', async () => {
		hasPubMock.mockResolvedValue(true);
		homeDraft.set({ ...PROF });
		profile.set({ ...PROF });
		identity.set(IDENT);
		appReady.set(true);

		const { queryByText, findByText } = render(HomePage);
		await waitFor(() => expect(hasPubMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));
		await tick();

		expect(queryByText(/Not published yet/i)).toBeNull();
		expect(await findByText(/discoverable in search/i)).toBeTruthy();
	});
});
