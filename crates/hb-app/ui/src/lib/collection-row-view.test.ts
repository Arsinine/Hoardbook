import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { deriveRowChip, menuItems, badges, sizeTier, sizeTierTooltip, MAX_COLLECTION_ITEMS } from './collection-row-view.js';
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

	it('menuItems_show_publish_when_draft_and_no_unpublish_anywhere', () => {
		const draftKeys = menuItems(col({ published: false })).map((i) => i.key);
		expect(draftKeys).toContain('publish');

		// Publish stays available for a published row too — re-publishing is the normal
		// parameterized-replaceable update path, and with Unpublish gone it is the only way to
		// refresh a listing (QURATOR-138).
		const publishedKeys = menuItems(col({ published: true })).map((i) => i.key);
		expect(publishedKeys).toContain('publish');

		// Always available regardless of state.
		expect(draftKeys).toEqual(expect.arrayContaining(['rescan', 'edit', 'remove']));
		// QURATOR-138 (owner ruling 2026-08-30): "Unpublish becomes DELETE. One destructive
		// operation that removes the local record and zeroes the published event." There is no
		// Unpublish affordance in ANY state — Delete is the single retract-and-remove verb.
		expect(draftKeys).not.toContain('unpublish');
		expect(publishedKeys).not.toContain('unpublish');
		// QURATOR-138: the collections-list Export affordance is deleted (owner ask).
		expect(draftKeys).not.toContain('export');
		expect(publishedKeys).not.toContain('export');
	});

	it('q138_the_remove_affordance_is_named_delete', () => {
		// QURATOR-138 AC 5: the affordance is named Delete, not Unpublish — in BOTH states, since
		// Delete is now the one operation (published collections retract + remove; drafts remove).
		// Mutation to redden: change menuItems' `{ key: 'remove', label: 'Delete' }` label back to
		// 'Remove' — both assertions fail.
		for (const published of [false, true]) {
			const labels = menuItems(col({ published })).filter((i) => i.key === 'remove');
			expect(labels).toHaveLength(1);
			expect(labels[0].label).toBe('Delete');
		}
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

// ── Size tiers (devtest 2026-08-26 item 6) ────────────────────────────────────────────────────────
describe('sizeTier — size-coded item counts', () => {
	it('under 80,000 is untiered: "remain as is"', () => {
		expect(sizeTier(0)).toBe('normal');
		expect(sizeTier(79_999)).toBe('normal');
		expect(sizeTierTooltip(79_999)).toBeNull();
	});

	it('80,000–99,999 is the amber band, inclusive at both ends', () => {
		expect(sizeTier(80_000)).toBe('warn');
		expect(sizeTier(99_999)).toBe('warn');
	});

	it('100,000 and over is the red band', () => {
		expect(sizeTier(100_000)).toBe('over');
		expect(sizeTier(1_000_000)).toBe('over');
	});

	it('both warning tiers carry a tooltip that names the cap', () => {
		for (const n of [80_000, 100_000]) {
			const tip = sizeTierTooltip(n);
			expect(tip, `tier at ${n} must explain itself`).toBeTruthy();
			expect(tip).toContain('100,000');
		}
	});

	// Chorus review 2026-08-27, finding 3. The tier and the Rust cap disagree by ONE at the boundary,
	// on purpose: the owner asked for red at "100000 items and over", but `enforce_item_cap` rejects
	// only `> MAX_COLLECTION_ITEMS`, so a 100,000-item collection is red AND scannable. The tooltip
	// is the thing that must not lie about that — it may say the cap is reached, never that this
	// collection is already broken.
	it('the red tooltip does not claim a scannable 100,000-item collection cannot be scanned', () => {
		const atCap = sizeTierTooltip(100_000)!;
		expect(atCap).toBeTruthy();
		// The false claim: an unconditional "this collection cannot be…" about a size Rust accepts.
		expect(atCap).not.toMatch(/A collection this large cannot be/);
		expect(atCap).not.toMatch(/this collection (can ?not|cannot)/i);
		// The true claim it must make instead: the consequence is PAST the cap, not AT it.
		expect(atCap).toMatch(/[Pp]ast the cap/);
	});

	// DRIFT GUARD: the 100,000 boundary is not ours to choose — it mirrors Rust's MAX_COLLECTION_ITEMS,
	// the cap `enforce_item_cap` rejects a scan at. If someone raises the Rust cap, the red tier would
	// otherwise keep warning about a wall that moved. Read the Rust source rather than a second copy of
	// the number, so this cannot pass against a stale duplicate.
	it('MAX_COLLECTION_ITEMS matches the Rust cap it describes', () => {
		const rust = readFileSync(resolve(process.cwd(), '../src/commands/collection.rs'), 'utf8');
		const m = rust.match(/const MAX_COLLECTION_ITEMS: u64 = ([0-9_]+);/);
		expect(m, 'MAX_COLLECTION_ITEMS not found in hb-app/src/commands/collection.rs').toBeTruthy();
		expect(Number(m![1].replace(/_/g, ''))).toBe(MAX_COLLECTION_ITEMS);
	});
});
