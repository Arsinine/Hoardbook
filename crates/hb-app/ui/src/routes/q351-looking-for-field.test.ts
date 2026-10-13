// @vitest-environment jsdom
// QURATOR-351 slice 2, state e — the "Looking for" line on the Home profile editor:
// a Public, 255-capped single-line input with a live N / 255 counter, saved trimmed and
// saved ABSENT when left empty (undefined, never ""), so a cleared line disappears from
// the teaser instead of persisting as an empty string.
//
// BEHAVIOURAL mount tests (q342/q138 pattern): the real page is rendered with `$lib/api.js`
// mocked; the assertions target what a person actually reads and what actually reaches the
// save call — never a source-scan (§7/§9).
//
// Each test names the exact production mutation that redded it (P-10), recorded beside it.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, fireEvent, waitFor } from '@testing-library/svelte';
import HomePage from './+page.svelte';
import {
	identity, profile, collections, appReady, homeDraft, identityLoadError, collectionsLoadError,
} from '$lib/stores.js';
import type { IdentityInfo, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		getCollections: vi.fn().mockResolvedValue([]),
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
		saveProfile: vi.fn().mockResolvedValue(undefined),
		publishProfile: vi.fn().mockResolvedValue(undefined),
	};
});

import { saveProfile } from '$lib/api.js';
const saveMock = saveProfile as unknown as ReturnType<typeof vi.fn>;

const IDENT: IdentityInfo = {
	npub: 'npub1q351' + 'c'.repeat(51),
	npub_short: 'npub1q351…cccc',
	share_code: 'hbk1q351test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Looking For Tester',
	bio: undefined,
	tags: ['anime'],
	since: undefined,
	est_size: undefined,
	languages: [],
	contact_hint: undefined,
	looking_for: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-10-13T00:00:00Z',
};

const LF_INPUT = /Kurosawa/; // unique placeholder of the Looking-for input

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
	collections.set([]);
	collectionsLoadError.set(false);
	appReady.set(false);
	identity.set(null);
	profile.set(null);
	homeDraft.set(null);
	vi.clearAllMocks();
});

describe('QURATOR-351 e — Looking for field', () => {
	// RED BY (P-10): removing `maxlength="255"` from the Looking-for input in
	// routes/+page.svelte made this test fail — getAttribute returned null.
	it('looking_for_input_is_255_capped_and_labelled_public', async () => {
		resetStores();
		const { findAllByText, findByPlaceholderText } = render(HomePage);
		const input = await findByPlaceholderText(LF_INPUT);
		expect(input.getAttribute('maxlength')).toBe('255');
		// The Public chip must sit ON the Looking-for label, not float anywhere on the page.
		const chips = await findAllByText('Public');
		expect(chips.some(c => c.closest('label')?.textContent?.includes('Looking for'))).toBe(true);
	});

	// RED BY (P-10): hardening the counter to a static `{0} / 255` in routes/+page.svelte
	// made this test fail — the counter stayed at 0 after typing.
	it('counter_tracks_typed_length_against_the_255_cap', async () => {
		resetStores();
		const { findByPlaceholderText, getByText } = render(HomePage);
		const input = await findByPlaceholderText(LF_INPUT);
		expect(getByText('0 / 255')).toBeTruthy();
		await fireEvent.input(input, { target: { value: 'Kurosawa scans' } });
		expect(getByText('14 / 255')).toBeTruthy();
	});

	// RED BY (P-10): deleting the `looking_for: form.looking_for?.trim() || undefined` line
	// from formForSave() in routes/+page.svelte made this test fail — the raw untrimmed value
	// reached saveProfile, and a cleared field saved "" instead of absent.
	it('save_trims_looking_for_and_saves_empty_as_absent', async () => {
		resetStores();
		const { findByPlaceholderText } = render(HomePage);
		const input = await findByPlaceholderText(LF_INPUT);
		await fireEvent.input(input, { target: { value: '  Kurosawa 70mm scans  ' } });
		// Delegated .fields flush-on-blur — do not wait out the debounce.
		await fireEvent.focusOut(input);
		await waitFor(() => expect(saveMock).toHaveBeenCalled());
		const saved = saveMock.mock.calls.at(-1)![0] as Profile;
		expect(saved.looking_for).toBe('Kurosawa 70mm scans');
		// Clearing the line must REMOVE it, not persist an empty string.
		await fireEvent.input(input, { target: { value: '' } });
		await fireEvent.focusOut(input);
		await waitFor(() => {
			const cleared = saveMock.mock.calls.at(-1)![0] as Profile;
			expect(cleared.looking_for).toBeUndefined();
		});
	}, 15000);
});
