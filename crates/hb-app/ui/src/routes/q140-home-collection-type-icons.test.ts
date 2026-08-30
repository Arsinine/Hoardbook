// @vitest-environment jsdom
// QURATOR-140 — every collection on Home rendered the folder icon regardless of type.
// The row icon is now keyed off the collection's first `content_types` entry (the fixed six-value
// enum the details form offers), and the folder is the "Other"/unknown shape ONLY.
//
// BEHAVIOURAL test (a real mount of the Home route), not a source-scan — same harness shape as
// q95-home-published-race: mock only `$lib/api.js`, hydrate the stores the layout owns, then
// assert on the rendered `.row-icon` SVG. The discriminator is the icon's CSS class
// (`hb-icon-video` vs `hb-icon-audio` vs …), which is unique per glyph — jsdom computes no
// layout, but `{@html}` SVGs land in the DOM with their class intact.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (make `rowIcon` return 'folder' unconditionally, re-run this file) MUST fail the
// first test; the folder-fallback test below must survive it.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';
import HomePage from './+page.svelte';
import { identity, profile, collections, appReady, homeDraft, identityLoadError, collectionsLoadError } from '$lib/stores.js';
import type { Collection, IdentityInfo, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
	};
});

const IDENT: IdentityInfo = {
	npub: 'npub1q140' + 'a'.repeat(53),
	npub_short: 'npub1q140…aaaa',
	share_code: 'hbk1q140test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Icon Tester',
	bio: undefined,
	tags: [],
	since: 2020,
	est_size: undefined,
	languages: ['English'],
	contact_hint: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

function makeCollection(slug: string, content_types: string[]): Collection {
	return {
		slug,
		path_alias: slug,
		item_count: 3,
		total_bytes: 1000,
		content_types,
		tags: [],
		languages: [],
		last_updated: '2026-01-01T00:00:00Z',
		listing: [],
		published: false,
	};
}

function resetStores() {
	identity.set(null);
	profile.set(null);
	collections.set([]);
	appReady.set(false);
	homeDraft.set(null);
	identityLoadError.set(null);
	collectionsLoadError.set(false);
}

afterEach(() => {
	cleanup();
	resetStores();
	vi.clearAllMocks();
});

/** Mount Home with `cols` in the collections store (the layout's load already applied). */
async function mountHome(cols: Collection[]) {
	identity.set(IDENT);
	profile.set({ ...PROF });
	homeDraft.set({ ...PROF });
	collections.set(cols);
	appReady.set(true);
	const { container } = render(HomePage);
	await tick();
	await new Promise((r) => setTimeout(r, 20));
	return container.querySelectorAll<HTMLElement>('.row-icon');
}

describe('QURATOR-140 — each collection type renders its own icon on Home', () => {
	it('audio / video / image / text / software each render a DISTINCT icon', async () => {
		const icons = await mountHome([
			makeCollection('audio-col', ['audio']),
			makeCollection('video-col', ['video']),
			makeCollection('image-col', ['image']),
			makeCollection('text-col', ['text']),
			makeCollection('software-col', ['software']),
		]);
		expect(icons.length).toBe(5);
		// NB: read the class via getAttribute — reading `.className` on an `{@html}`-injected SVG
		// throws rune_outside_svelte under Svelte 5 + jsdom (bisected: this assertion alone reds).
		const classes = [...icons].map((el) => el.querySelector('svg')?.getAttribute('class') ?? '');
		expect(classes).toEqual([
			'hb-icon-audio',
			'hb-icon-video',
			'hb-icon-image',
			'hb-icon-text',
			'hb-icon-software',
		]);
		// Distinctness is the acceptance criterion — five rows, five different glyphs.
		expect(new Set(classes).size).toBe(5);
	});

	it('unknown / absent / "other" types still render the folder — and only those do', async () => {
		const icons = await mountHome([
			makeCollection('other-col', ['other']),
			makeCollection('weird-col', ['robótica']), // not in the enum — must not crash, must fall back
			makeCollection('empty-col', []),           // no type picked yet (a fresh draft)
		]);
		expect(icons.length).toBe(3);
		for (const el of icons) {
			expect(el.querySelector('svg')?.getAttribute('class')).toBe('hb-icon-folder');
		}
	});

	// Owner ruling 2026-08-27: "a video icon should be for a pure video collection." A type icon is a
	// claim about what the collection IS, so a mixed collection has not earned one — it falls to the
	// folder, which is precisely what keeps the folder meaningful ("not one thing"). Keying off
	// content_types[0] would put the video icon on a video+audio collection and assert something
	// false; the user ticks these boxes in a form, so [0] is the first one they happened to pick.
	it('a MIXED collection renders the folder, never the icon of its first type', async () => {
		const icons = await mountHome([
			makeCollection('video-audio', ['video', 'audio']),
			makeCollection('audio-video', ['audio', 'video']), // reversed: order must not decide
			makeCollection('three-way', ['text', 'image', 'software']),
		]);
		expect(icons.length).toBe(3);
		const classes = [...icons].map((el) => el.querySelector('svg')?.getAttribute('class') ?? '');
		expect(classes).toEqual(['hb-icon-folder', 'hb-icon-folder', 'hb-icon-folder']);
	});
});
