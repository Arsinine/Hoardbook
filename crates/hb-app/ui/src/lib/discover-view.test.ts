import { describe, expect, it } from 'vitest';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
	DISCOVER_CONTENT_TYPES,
	parseTagInput,
	canSearch,
	toggleContentType,
	DISCOVER_PAGE_SIZE,
	pageItems,
	pageCount,
	suggestTags,
	matchReason,
} from './discover-view.js';

describe('discover-view — §6 Browse filter bar (M12 W3)', () => {
	it('exposes the six coarse content-type categories', () => {
		expect(DISCOVER_CONTENT_TYPES.map((c) => c.value)).toEqual([
			'video',
			'audio',
			'image',
			'text',
			'software',
			'other',
		]);
	});

	it('requires at least one filter before a search runs (no unfiltered global list)', () => {
		expect(canSearch([], [])).toBe(false);
		expect(canSearch(['anime'], [])).toBe(true);
		expect(canSearch([], ['video'])).toBe(true);
	});

	it('parses + normalizes + dedupes the tag input', () => {
		expect(parseTagInput('Anime, VHS  anime')).toEqual(['anime', 'vhs']);
		expect(parseTagInput('   ')).toEqual([]);
	});

	it('toggles content-types (OR set)', () => {
		expect(toggleContentType([], 'video')).toEqual(['video']);
		expect(toggleContentType(['video'], 'audio')).toEqual(['video', 'audio']);
		expect(toggleContentType(['video', 'audio'], 'video')).toEqual(['audio']);
	});
});

describe('discover-view — QURATOR-44 pagination (page size 10)', () => {
	it('exposes a page size of 10', () => {
		expect(DISCOVER_PAGE_SIZE).toBe(10);
	});

	it('slices a ranked set into pages of 10', () => {
		const items = Array.from({ length: 25 }, (_, i) => i);
		expect(pageItems(items, 1)).toEqual(items.slice(0, 10));
		expect(pageItems(items, 2)).toEqual(items.slice(10, 20));
		expect(pageItems(items, 3)).toEqual(items.slice(20, 30));
		expect(pageItems(items, 4)).toEqual([]); // no page 4
	});

	it('pageCount is at least 1 even for an empty result set', () => {
		expect(pageCount(0)).toBe(1);
		expect(pageCount(10)).toBe(1);
		expect(pageCount(11)).toBe(2);
		expect(pageCount(25)).toBe(3);
	});

	it('pagination partitions without dupes or skips across pages', () => {
		// The same item never appears on two pages, and no item is skipped: the union of all pages
		// equals the original ranked set.
		const items = Array.from({ length: 25 }, (_, i) => `hit-${i}`);
		const collected: string[] = [];
		for (let p = 1; p <= pageCount(items.length); p++) {
			collected.push(...pageItems(items, p));
		}
		expect(collected).toEqual(items);
		// No dupes.
		expect(new Set(collected).size).toBe(collected.length);
	});
});

describe('discover-view — QURATOR-70 tag autocomplete (suggestTags)', () => {
	it('returns unselected observed tags whose lowercased form contains the typed stem', () => {
		const observed = ['anime', 'vhs', 'manga', 'anime-classics'];
		expect(suggestTags(observed, [], 'ani')).toEqual(['anime', 'anime-classics']);
		expect(suggestTags(observed, [], 'vhs')).toEqual(['vhs']);
	});

	it('is case-insensitive on the typed stem', () => {
		const observed = ['anime', 'manga'];
		expect(suggestTags(observed, [], 'ANI')).toEqual(['anime']);
		expect(suggestTags(observed, [], 'MaNgA')).toEqual(['manga']);
	});

	it('excludes already-selected tags so the dropdown never re-offers a chosen tag', () => {
		const observed = ['anime', 'anime-classics', 'vhs'];
		// 'anime' is already selected → only 'anime-classics' remains for stem 'ani'.
		expect(suggestTags(observed, ['anime'], 'ani')).toEqual(['anime-classics']);
	});

	it('returns nothing for an empty or whitespace-only stem', () => {
		const observed = ['anime', 'vhs'];
		expect(suggestTags(observed, [], '')).toEqual([]);
		expect(suggestTags(observed, [], '   ')).toEqual([]);
	});

	it('caps the suggestion list to keep the dropdown short', () => {
		// 12 observed tags all containing 'a'; default cap is 8.
		const observed = Array.from({ length: 12 }, (_, i) => `a-tag-${i}`);
		expect(suggestTags(observed, [], 'a').length).toBe(8);
		expect(suggestTags(observed, [], 'a', 5).length).toBe(5);
	});

	it('returns nothing when no observed tag contains the stem', () => {
		const observed = ['anime', 'vhs'];
		expect(suggestTags(observed, [], 'berserk')).toEqual([]);
	});
});

// Why-matched badge (owner sign-off 2026-08-15). `matchReason` infers the axis a hit surfaced on
// from the terms the search ran with, mirroring the backend `teaser_matches` rule (hb-net
// discover.rs): tags first, content-types alongside, and name/bio fuzzy ONLY for a single term.
describe('discover-view — matchReason (why-matched badge)', () => {
	const hit = {
		tags: ['anime', 'vhs'],
		content_types: ['video'],
		display_name: 'Berserk Collector',
		bio: 'I collect Berserk manga',
	};

	it('tag outranks everything: an exact/substring tag match reports "tag" even when name and content-type also match', () => {
		// The hit's name contains 'berserk', its bio contains 'berserk', its content_types contain
		// 'video' — but the term 'vhs' is one of its tags, so the reason is 'tag'.
		expect(matchReason(hit, ['vhs'], ['video'])).toBe('tag');
	});

	it('content-type applies when no search tag matched, including type-only searches', () => {
		expect(matchReason(hit, [], ['video'])).toBe('content-type');
		// The tag term 'manga' is nowhere in hit.tags → falls through to the content-type.
		expect(matchReason(hit, ['manga'], ['video'])).toBe('content-type');
	});

	it('Codex review 2026-08-15: a single TYPED term also matches via content_types, with no type chip selected', () => {
		// 'video' is nowhere in hit.tags/display_name/bio, but IS in hit.content_types — and the
		// backend's substring_in_teaser folds content_types into the single-term haystack, so this
		// must resolve to 'content-type' even though `contentTypes` (the selected chips) is empty.
		expect(matchReason(hit, ['video'], [])).toBe('content-type');
	});

	it('single-term fuzzy: name and bio reasons apply when the term misses tags and content-types', () => {
		expect(matchReason(hit, ['collector'], [])).toBe('name');
		expect(matchReason(hit, ['manga'], [])).toBe('bio');
		// A null bio must not crash the bio branch.
		expect(matchReason({ ...hit, bio: null }, ['collector'], [])).toBe('name');
	});

	it('name outranks bio when a single term appears in both', () => {
		// 'berserk' is in the display_name AND the bio — name wins by precedence.
		expect(matchReason(hit, ['berserk'], [])).toBe('name');
	});

	it('two+ tag terms NEVER yield name/bio (multi-term is strict AND-on-tags server-side)', () => {
		// The single-term fuzzy rule does not exist for multi-term searches, so 'collector'/'manga'
		// can only be reported via a tag/content-type axis; neither applies here → null.
		expect(matchReason(hit, ['collector', 'manga'], [])).toBeNull();
	});

	it('matching is case-insensitive on both sides', () => {
		expect(matchReason({ ...hit, tags: ['Anime'] }, ['ANIME'], [])).toBe('tag');
		expect(matchReason(hit, ['COLLECTOR'], [])).toBe('name');
	});

	it('returns null when nothing confidently matched (badge omitted, never a guess)', () => {
		// Empty search terms is not a real client state (canSearch gates it), but the helper stays
		// honest: no terms → no reason.
		expect(matchReason(hit, [], [])).toBeNull();
		expect(matchReason(hit, ['unrelated'], ['software'])).toBeNull();
	});
});
// because the filter never widened. QURATOR-70 now widens SINGLE-TERM search to name/bio/tags fuzzy
// (multi-term stays strict AND-on-tags), so "name, bio, or tag" is TRUE for one term and the copy
// must say so. The copy also explains the two-term narrowing rule so a user who narrows understands
// the second term is a tag. Svelte source-scan (the route page cannot be mounted).
describe('discover-view — QURATOR-70 search-box copy tracks what the filter does', () => {
	const here = path.dirname(fileURLToPath(import.meta.url));
	const panelPath = path.resolve(here, 'components', 'AddContactPanel.svelte');

	it('the Discover copy claims name/bio/tag for the single-term fuzzy match', () => {
		const src = fs.readFileSync(panelPath, 'utf8');
		// QURATOR-70: single-term search matches name+bio+tags fuzzily; the copy now honestly says so.
		expect(src).toContain('Search public profiles by name, bio, or tag.');
		// The placeholder mirrors the widened axis.
		expect(src).toContain('placeholder="name, bio, or tag (e.g. anime, vhs)"');
		// The two-term narrowing rule is stated so narrowing does not read as broken (a hit found by
		// bio on term 1 would vanish under strict AND-on-tags on term 2 without this affordance).
		// f50985a (QURATOR-134) tightened the wording — "Add more tags to narrow" carries the same
		// affordance the old "two or more tags narrow" clause did.
		expect(src).toContain('Add more tags to narrow');
		// And it must NOT overpromise the multi-term behaviour: multi-term is tags-only, not name/bio.
		expect(src).not.toContain('name, bio, tags, or content type');
	});
});
