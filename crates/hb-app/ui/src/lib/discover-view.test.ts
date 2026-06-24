import { describe, expect, it } from 'vitest';
import {
	DISCOVER_CONTENT_TYPES,
	parseTagInput,
	canSearch,
	toggleContentType,
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
