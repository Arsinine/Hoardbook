// @vitest-environment jsdom
// QURATOR-379 — the Home "How you appear" live preview and the Looking-for field-hint sentence
// are REMOVED (owner, 2026-10-18: the preview restated what Discover already shows, and the hint
// restated what the Public chip and the 0/255 counter already say).
//
// This file used to pin the preview itself (QURATOR-364). It now pins the removal: Home renders
// neither the "How you appear" block nor the hint sentence, while the Looking-for field itself
// (label, Public chip, input, counter) survives. Mounted page, not a source scan — but note the
// absences are asserted page-wide (the removed strings must appear NOWHERE on Home).
//
// MUTATION (P-10): restore the deleted preview — in routes/+page.svelte, re-insert the field-hint
// line "Public, like your collection names. It leaves with your teaser…" after the
// `.field-count` div in the Looking-for field AND an `<div class="field-label">How you appear</div>`
// anywhere in the fields. Both assertions below red. Seen red, then removed again.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
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
	};
});

const IDENT: IdentityInfo = {
	npub: 'npub1q364' + 'c'.repeat(51),
	npub_short: 'npub1q364…cccc',
	share_code: 'hbk1q364test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Kiki',
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
	updated: '2026-10-14T00:00:00Z',
};

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
	collections.set([]);
	collectionsLoadError.set(false);
	appReady.set(false);
	identity.set(null);
	profile.set(null);
	homeDraft.set(null);
	vi.clearAllMocks();
});

describe('QURATOR-379 — Home renders no "How you appear" preview and no Looking-for hint', () => {
	it('the_removed_preview_and_hint_are_gone_while_the_looking_for_field_survives', async () => {
		resetStores();
		const { findByPlaceholderText, container } = render(HomePage);

		// The field itself survives: the input (by its placeholder) and the N/255 counter.
		const lfInput = await findByPlaceholderText(/Kurosawa 70mm/);
		expect(lfInput).toBeTruthy();
		const counter = Array.from(container.querySelectorAll('.field-count'))
			.find((el) => /\/\s*255$/.test(el.textContent ?? ''));
		expect(counter, 'the Looking-for counter (N / 255) must still render').toBeTruthy();

		// The removed copy appears NOWHERE on the page.
		expect(container.textContent).not.toContain('How you appear');
		expect(container.textContent).not.toContain('leaves with your teaser');
		expect(container.textContent).not.toContain('has no “Looking for” line');
	});
});
