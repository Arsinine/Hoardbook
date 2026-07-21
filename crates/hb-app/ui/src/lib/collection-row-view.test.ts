import { describe, expect, it } from 'vitest';
import { deriveRowChip, menuItems, badges } from './collection-row-view.js';
import type { Collection } from './types.js';

function col(overrides: Partial<Collection> = {}): Collection {
	return {
		slug: 'movies',
		path_alias: 'Movies',
		item_count: 3,
		total_bytes: 1000,
		content_types: [],
		tags: [],
		languages: [],
		last_updated: '2026-01-01T00:00:00Z',
		listing: [],
		...overrides,
	};
}

describe('collection-row-view', () => {
	it('deriveRowChip_draft_vs_published', () => {
		expect(deriveRowChip(col({ published: false }))).toBe('Draft');
		expect(deriveRowChip(col({ published: undefined }))).toBe('Draft'); // pre-publish collection
		expect(deriveRowChip(col({ published: true }))).toBe('Published');
	});

	it('menuItems_show_publish_when_draft_and_unpublish_when_published', () => {
		const draftKeys = menuItems(col({ published: false })).map((i) => i.key);
		expect(draftKeys).toContain('publish');
		expect(draftKeys).not.toContain('unpublish');

		const publishedKeys = menuItems(col({ published: true })).map((i) => i.key);
		expect(publishedKeys).toContain('unpublish');
		expect(publishedKeys).not.toContain('publish');

		// Always available regardless of state.
		expect(draftKeys).toEqual(expect.arrayContaining(['rescan', 'edit', 'export', 'remove']));
	});

	it('export_submenu_offers_the_manifest_file_alongside_the_checklists', () => {
		// M16 W4: the `.hbmanifest` full-listing envelope is a third export format, next to the two
		// human-readable checklists.
		const exportItem = menuItems(col()).find((i) => i.key === 'export');
		const subKeys = exportItem && 'submenu' in exportItem ? exportItem.submenu.map((s) => s.key) : [];
		expect(subKeys).toEqual(['text', 'markdown', 'manifest']);
	});

	it('badges_include_sorted_and_private_when_set', () => {
		expect(badges(col({ sorted: false, visibility: 'Public' }))).toEqual([]);
		expect(badges(col({ sorted: true, visibility: 'Public' }))).toEqual([
			{ label: 'Sorted', kind: 'sorted' },
		]);
		expect(badges(col({ sorted: true, visibility: 'Private' }))).toEqual([
			{ label: 'Sorted', kind: 'sorted' },
			{ label: 'Private', kind: 'private' },
		]);
		// Absent visibility ⇒ Public (pre-M10 collection) — never a silent Private badge.
		expect(badges(col({ sorted: false, visibility: undefined }))).toEqual([]);
	});
});
