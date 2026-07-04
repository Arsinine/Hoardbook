import { describe, expect, it } from 'vitest';
import {
	availabilityBadge,
	collectionAvailability,
	dedupAndCap,
	flattenTree,
	type RenderedListing,
	type SearchHit,
	type TreeNode,
} from './browse-view.js';

describe('browse-view — tree flattening', () => {
	it('flattens a nested listing depth-first with depths, preserving order', () => {
		const tree: TreeNode[] = [
			{ name: 'Films', children: [{ name: 'Ran' }, { name: 'Seven Samurai' }] },
			{ name: 'README.txt' },
		];
		expect(flattenTree(tree)).toEqual([
			{ name: 'Films', depth: 0 },
			{ name: 'Ran', depth: 1 },
			{ name: 'Seven Samurai', depth: 1 },
			{ name: 'README.txt', depth: 0 },
		]);
	});
});

describe('browse-view — K of N availability badge', () => {
	it('returns null when the listing is complete', () => {
		const complete: RenderedListing = { entries: [], partsTotal: 3, partsPresent: 3, missing: [] };
		expect(availabilityBadge(complete)).toBeNull();
	});

	it('reports "K of N folders available" when parts are missing', () => {
		const partial: RenderedListing = { entries: [], partsTotal: 3, partsPresent: 2, missing: [2] };
		expect(availabilityBadge(partial)).toBe('2 of 3 folders available');
	});
});

describe('browse-view — collection K of N availability (M13 HANDOVER gap #5)', () => {
	it('reports "K of N folders available" when a peer collection is partial', () => {
		expect(collectionAvailability({ parts_total: 5, parts_present: 3 })).toBe('3 of 5 folders available');
	});

	it('returns null when a peer collection is complete', () => {
		expect(collectionAvailability({ parts_total: 5, parts_present: 5 })).toBeNull();
	});

	it('returns null when parts info is absent (a pre-M13 cached collection)', () => {
		expect(collectionAvailability({})).toBeNull();
	});
});

describe('browse-view — result list dedup + cap', () => {
	const hits: SearchHit[] = [
		{ npub: 'npub1a', displayName: 'a' },
		{ npub: 'npub1a', displayName: 'a-dup' }, // duplicate npub
		{ npub: 'npub1b', displayName: 'b' },
		{ npub: 'npub1c', displayName: 'c' },
	];

	it('dedups by npub keeping first', () => {
		const out = dedupAndCap(hits, 100);
		expect(out.map((h) => h.npub)).toEqual(['npub1a', 'npub1b', 'npub1c']);
	});

	it('caps the result count', () => {
		expect(dedupAndCap(hits, 2)).toHaveLength(2);
	});
});

describe('browse-view — flattenTree handles deep nesting without recursion', () => {
	it('flattens a deeply-nested chain iteratively (no stack overflow)', () => {
		// Build a 5000-deep single chain — recursion + spread would risk a stack/arg-limit blow-up.
		let node: TreeNode = { name: 'leaf' };
		for (let i = 0; i < 5000; i++) node = { name: `d${i}`, children: [node] };
		const rows = flattenTree([node]);
		expect(rows).toHaveLength(5001);
		expect(rows[rows.length - 1]).toEqual({ name: 'leaf', depth: 5000 });
	});
});
