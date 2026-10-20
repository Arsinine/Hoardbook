// @vitest-environment jsdom
// QURATOR-342 Lane E — My Profile copy: "Tags" → "Interests" (+ the recommendation help line),
// a visible "Public" chip on the Contact-hint label, and the public-collections disclosure.
//
// BEHAVIOURAL mount tests (q93 pattern): the real page is rendered with `$lib/api.js` mocked and
// the assertions target what a person actually reads — never a source-scan (§7/§9).
//
// Each test names the exact production mutation that redded it (P-10), recorded beside it below.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import HomePage from './+page.svelte';
import {
	identity, profile, collections, appReady, homeDraft, identityLoadError, collectionsLoadError,
} from '$lib/stores.js';
import type { IdentityInfo, Profile, Collection } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		getCollections: vi.fn(),
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
	};
});

import { getCollections } from '$lib/api.js';
const getCollectionsMock = getCollections as unknown as ReturnType<typeof vi.fn>;

const IDENT: IdentityInfo = {
	npub: 'npub1q342' + 'b'.repeat(51),
	npub_short: 'npub1q342…bbbb',
	share_code: 'hbk1q342test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Interests Tester',
	bio: undefined,
	tags: ['anime', 'scifi'],
	since: undefined,
	est_size: undefined,
	languages: [],
	contact_hint: 'you@example.com',
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-09-28T00:00:00Z',
};

function makeCollection(slug: string, published: boolean): Collection {
	return {
		slug,
		path_alias: slug,
		item_count: 3,
		total_bytes: 1024,
		content_types: [],
		tags: [],
		languages: [],
		last_updated: '2026-09-28T00:00:00Z',
		listing: [],
		published,
	};
}

const HELP_TEXT = /This decides who and what gets recommended/i;
const DISCLOSURE = /Published collection names and sizes are public/i;

function resetStores(publishedCount = 0) {
	identity.set(IDENT);
	profile.set({ ...PROF });
	collections.set([
		makeCollection('ebooks', publishedCount >= 1),
		makeCollection('films', publishedCount >= 2),
	]);
	collectionsLoadError.set(false);
	appReady.set(true);
	homeDraft.set({ ...PROF });
	identityLoadError.set(null);
	getCollectionsMock.mockResolvedValue([]);
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

describe('QURATOR-342 Lane E — My Profile copy', () => {
	// RED BY (P-10): reverting to the pre-QURATOR-388 markup — bare `<label>Interests</label>` plus
	// the always-visible `.field-hint` div, no HintMarker — reds this at the trigger query (no
	// "More information: Interests" button) and at the visible-hint absence assert.
	it('interests_label_and_help_line_replace_tags', async () => {
		// QURATOR-388, owner devtest 2026-10-20: the help line moved off the page into a HintMarker
		// tooltip on the Interests label (text verbatim), asserted through the real trigger/tip
		// pair — not raw page text, which a hidden tooltip span would still satisfy.
		resetStores();
		const { findByText, getByRole } = render(HomePage);
		expect(await findByText('Interests')).toBeTruthy();
		const trigger = getByRole('button', { name: 'More information: Interests' });
		const id = trigger.getAttribute('aria-describedby');
		expect(id, 'the Interests marker must carry aria-describedby').toBeTruthy();
		const tip = document.getElementById(id!);
		expect(tip?.textContent).toMatch(HELP_TEXT);
		// The second half names the thing the ranking feeds ("People like you" on Contacts).
		expect(tip?.textContent).toMatch(/People like you on Contacts is ranked from these/i);
		// And the old always-visible hint line is gone from the page proper (the tooltip span is
		// .hint-tip, never .field-hint).
		const visibleHints = Array.from(document.querySelectorAll('.field-hint'));
		expect(visibleHints.some((el) => HELP_TEXT.test(el.textContent ?? ''))).toBe(false);
	});

	// RED BY (P-10): deleting `<span class="hb-tag">Public</span>` from the Contact-hint label
	// made this test fail — no Public chip inside a label whose text names Contact hint.
	// 2026-10-13: QURATOR-351 added a second Public chip (the Looking-for label), so a bare
	// findByText('Public') now throws on multiplicity; the query is scoped to the chip ON the
	// Contact-hint label, which was always the assertion's meaning.
	it('contact_hint_label_carries_a_visible_public_chip', async () => {
		resetStores();
		const { findAllByText } = render(HomePage);
		const chips = await findAllByText('Public');
		const onContactHint = chips.filter((c) => /Contact hint/.test(c.closest('label')?.textContent ?? ''));
		expect(onContactHint).toHaveLength(1);
	});

	// RED BY (P-10): deleting the `coll-disclosure` div under the collections-pane header made
	// this test fail. Two PUBLISHED collections are seeded so "once, not per row" is pinned: the
	// sentence renders exactly one element even with two rows below it.
	it('public_collections_disclosure_renders_once_above_the_list', async () => {
		resetStores(2);
		const { findAllByText, getByText } = render(HomePage);
		const hits = await findAllByText(DISCLOSURE);
		expect(hits.length).toBe(1);
		expect(getByText(/even people who can.t read your files/i)).toBeTruthy();
	});
});
