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
// The copy must claim only what the FILTER does. It was briefly broadened to name/bio/tags/types,
// but teaser_matches filters on exact TAGS only — see the test body. Svelte source-scan (the route
// page cannot be mounted).
describe('discover-view — QURATOR-44 search-box copy', () => {
	const here = path.dirname(fileURLToPath(import.meta.url));
	const panelPath = path.resolve(here, 'components', 'AddContactPanel.svelte');

	it('the Discover copy claims only what the FILTER actually does (tags + content types)', () => {
		const src = fs.readFileSync(panelPath, 'utf8');
		// QURATOR-44 briefly broadened this copy to "name, bio, or tag". A Codex review caught that the
		// FILTER never widened: teaser_matches requires every typed term to be an exact TAG
		// (discover.rs `tags.iter().all(|q| teaser.tags.contains(q))`), so a peer whose bio mentions the
		// word but who carries no such tag is discarded BEFORE rank_hits ever runs. rank_hits does score
		// name/bio — but only to ORDER an already tag-filtered set. Promising bio search while filtering
		// on tags reads to a user as the app being broken, so the copy was reverted to the truth.
		//
		// Widening the filter is its own workstream: teaser_matches is public API, hb-it's DISC1 keeps an
		// independent oracle of the AND-tag/OR-content-type rule, and WAN-D relies on it discarding a
		// teaser for a missing tag. Changing it needs both live suites re-run.
		expect(src).toContain('by tag &amp; content type');
		expect(src).toContain('placeholder="tags (e.g. anime, vhs)"');
		// And it must NOT claim the unimplemented axes.
		expect(src).not.toContain('name, bio, or tag');
		expect(src).not.toContain('name, bio, tags, or content type');
	});
});
