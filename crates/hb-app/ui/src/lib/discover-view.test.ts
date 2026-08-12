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
		expect(src).toContain('two or more tags narrow');
		// And it must NOT overpromise the multi-term behaviour: multi-term is tags-only, not name/bio.
		expect(src).not.toContain('name, bio, tags, or content type');
	});
});
