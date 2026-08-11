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

// QURATOR-44: pin the broadened search-box copy so reverting to tags-only wording reds this test.
// The old placeholder was "tags (e.g. anime, vhs)" which implied tags-only search; the ruling
// broadened it to name/bio/tags/types. Svelte source-scan (the route page cannot be mounted).
describe('discover-view — QURATOR-44 search-box copy', () => {
	const here = path.dirname(fileURLToPath(import.meta.url));
	const panelPath = path.resolve(here, 'components', 'AddContactPanel.svelte');

	it('AddContactPanel placeholder reads broadly (name/bio/tag), not tags-only', () => {
		const src = fs.readFileSync(panelPath, 'utf-8');
		expect(src).toContain('name, bio, or tag');
		expect(src).not.toContain('placeholder="tags (e.g. anime, vhs)"');
	});

	it('AddContactPanel subtitle mentions name/bio/tags/content type', () => {
		const src = fs.readFileSync(panelPath, 'utf-8');
		expect(src).toContain('name, bio, tags, or content type');
	});
});
