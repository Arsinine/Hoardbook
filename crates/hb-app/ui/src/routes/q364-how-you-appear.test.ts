// @vitest-environment jsdom
// QURATOR-364 — Home "How you appear" live preview (QURATOR-351 r5 state e).
//
// The preview under the Looking-for field must mirror the entry other people see in Discover
// (DiscoverBook entryCard): avatar, display name, "<size> · N collections", the published
// collection names, the Looking-for line — and it must update LIVE from `form`, no save needed.
// Published-only: an unpublished collection must never appear.
//
// BEHAVIOURAL mount tests (q342/q93 pattern): the real page rendered with `$lib/api.js` mocked;
// assertions scope to the `.appear` preview container's textContent — the page repeats the name
// (profile header) and "Looking for" (the field label) elsewhere, so page-wide queries are
// ambiguous. Never a source-scan (§7/§9).
//
// Each test names the exact production mutation that redded it (P-10), by LINE NUMBER:
//   (a) PUBLISHED-ONLY: `sed -i '499s|.*|let publishedCols = $derived($collections);|' \
//         src/routes/+page.svelte` — the derived at line 499 loses its `.published` filter,
//       so "3 collections" appears and the unpublished name leaks into the preview. Seen red.
//   (b) LOOKING-FOR GUARD: `sed -i '878s|{#if appearLookingFor}|{#if true}|' \
//         src/routes/+page.svelte` — the guard on the preview's Looking-for line is removed,
//       so the line still renders (empty) after the input is cleared. Seen red.
// Line numbers as of the F4 lane's tree (same file also carries the Q342E Looking-for field);
// re-grep `appearLookingFor` / `publishedCols` if siblings shift them.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, fireEvent } from '@testing-library/svelte';
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
	npub: 'npub1q364' + 'c'.repeat(51),
	npub_short: 'npub1q364…cccc',
	share_code: 'hbk1q364test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: '',
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

// 2 published + 1 unpublished. 1024 + 2048 = 3072 → fmtBytes "3 KB" → meta "3 KB · 2 collections".
function makeCollection(slug: string, published: boolean): Collection {
	return {
		slug,
		path_alias: slug,
		item_count: 3,
		total_bytes: 1024,
		content_types: [],
		tags: [],
		languages: [],
		last_updated: '2026-10-14T00:00:00Z',
		listing: [],
		published,
	};
}

function resetStores() {
	identity.set(IDENT);
	profile.set({ ...PROF });
	collections.set([
		{ ...makeCollection('ebooks', true), total_bytes: 1024 },
		{ ...makeCollection('films', true), total_bytes: 2048 },
		makeCollection('vault', false),
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

describe('QURATOR-364 — How you appear (live preview)', () => {
	it('preview_mirrors_the_discover_entry_and_updates_live', async () => {
		resetStores();
		const { findByText, getByPlaceholderText, container } = render(HomePage);
		const preview = container.querySelector('.appear');
		expect(preview).toBeTruthy();
		// Empty name → the preview says "You"; the collections are already seeded at mount,
		// so the meta and names lines start populated (that IS the live behaviour).
		expect(preview!.textContent).toContain('How you appear');
		expect(preview!.textContent).toContain('You');
		expect(preview!.textContent).toContain('3 KB · 2 collections');
		expect(preview!.textContent).toContain('ebooks · films');
		// Empty looking-for → no Looking-for line, even before typing.
		expect(preview!.textContent).not.toContain('Looking for');

		// Type live (no save): name, then a Looking-for line with padding that must be trimmed.
		fireEvent.input(getByPlaceholderText('e.g. DataHoarder_42'), { target: { value: 'Kiki' } });
		fireEvent.input(getByPlaceholderText(/Kurosawa 70mm/), {
			target: { value: '  film noir posters of 1938  ' },
		});

		expect(preview!.textContent).toContain('Kiki');
		// Published-only size + pluralised count (DiscoverBook's exact meta shape).
		expect(preview!.textContent).toContain('3 KB · 2 collections');
		// Both published names joined with " · ".
		expect(preview!.textContent).toContain('ebooks · films');
		// The Looking-for line is TRIMMED. (jsdom renders the label span + content with no
		// space between — `Looking forfilm noir…` — so assert label and content separately.)
		expect(preview!.textContent).toContain('Looking for');
		expect(preview!.textContent).toContain('film noir posters of 1938');
		// The unpublished collection never appears.
		expect(preview!.textContent).not.toContain('vault');
		expect(preview!.textContent).not.toContain('3 collections');
		expect(await findByText('How you appear')).toBeTruthy();
	});

	it('clearing_looking_for_removes_the_preview_line', async () => {
		resetStores();
		const { findByText, getByPlaceholderText, container } = render(HomePage);
		const preview = container.querySelector('.appear')!;
		fireEvent.input(getByPlaceholderText(/Kurosawa 70mm/), {
			target: { value: '  film noir posters of 1938  ' },
		});
		await findByText('How you appear');
		expect(preview.textContent).toContain('film noir posters of 1938');

		fireEvent.input(getByPlaceholderText(/Kurosawa 70mm/), { target: { value: '' } });
		// The guard (the `{#if appearLookingFor}` at line 878) removes the whole line.
		expect(preview.textContent).not.toContain('Looking for');
		expect(preview.textContent).not.toContain('film noir posters of 1938');
		// The rest of the preview survives the clear.
		expect(preview.textContent).toContain('ebooks · films');
		expect(preview.textContent).toContain('3 KB · 2 collections');
	});
});
