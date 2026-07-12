import { describe, expect, it } from 'vitest';
import {
	availabilityBadge,
	collectionAvailability,
	dedupAndCap,
	flattenTree,
	parseEstSize,
	peerAccessBadge,
	peerFromQuery,
	summarizeCollectionsSize,
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

describe('browse-view — peerAccessBadge (devtest #1)', () => {
	it('a keyed peer (non-empty browse_key_hex) reads as browseable', () => {
		expect(peerAccessBadge({ browse_key_hex: 'deadbeef' })).toEqual({
			locked: false,
			icon: '🔓',
			label: 'browseable',
			hint: '',
		});
	});

	it('a bare peer (absent browse_key_hex) reads as locked with a remedy hint', () => {
		const badge = peerAccessBadge({});
		expect(badge.locked).toBe(true);
		expect(badge.icon).toBe('🔒');
		expect(badge.label).toBe('key needed');
		expect(badge.hint.length).toBeGreaterThan(0);
	});

	it('an empty-string browse_key_hex reads as bare, not keyed', () => {
		expect(peerAccessBadge({ browse_key_hex: '' }).locked).toBe(true);
	});

	it('stays locked even when a bare peer carries cached collections — keys off browse_key_hex only', () => {
		const badge = peerAccessBadge({ browse_key_hex: undefined });
		expect(badge.locked).toBe(true);
	});
});

describe('browse-view — parseEstSize (devtest #7)', () => {
	it('parses "14.2 GB"', () => {
		expect(parseEstSize('14.2 GB')).toBeCloseTo(14.2 * 1024 ** 3, 0);
	});

	it('parses "14.2GB" (no space)', () => {
		expect(parseEstSize('14.2GB')).toBeCloseTo(14.2 * 1024 ** 3, 0);
	});

	it('parses "~12 TB" (leading tilde)', () => {
		expect(parseEstSize('~12 TB')).toBeCloseTo(12 * 1024 ** 4, 0);
	});

	it('parses "512 MB"', () => {
		expect(parseEstSize('512 MB')).toBeCloseTo(512 * 1024 ** 2, 0);
	});

	it('is 0 for undefined', () => {
		expect(parseEstSize(undefined)).toBe(0);
	});

	it('is 0 for an empty string', () => {
		expect(parseEstSize('')).toBe(0);
	});

	it('is 0 for an unparseable string', () => {
		expect(parseEstSize('lots')).toBe(0);
	});
});

describe('browse-view — summarizeCollectionsSize (devtest #7)', () => {
	it('sums parseable collections and pluralizes the count', () => {
		expect(summarizeCollectionsSize([{ est_size: '1.0 GB' }, { est_size: '1.0 GB' }])).toBe(
			'~2.0 GB across 2 collections',
		);
	});

	it('keeps "collection" singular for exactly one', () => {
		expect(summarizeCollectionsSize([{ est_size: '512 MB' }])).toBe('~512.0 MB across 1 collection');
	});

	it('is null for an empty list', () => {
		expect(summarizeCollectionsSize([])).toBeNull();
	});

	it('is null when every entry is unparseable (never fabricates a "0 B" summary)', () => {
		expect(summarizeCollectionsSize([{ est_size: 'lots' }, { est_size: undefined }])).toBeNull();
	});

	it('sums only the parseable entries but counts every collection in M', () => {
		expect(summarizeCollectionsSize([{ est_size: '1.0 GB' }, { est_size: 'lots' }])).toBe(
			'~1.0 GB across 2 collections',
		);
	});
});

describe('peerFromQuery — M15 W4 browse deep-link', () => {
	const contacts = [{ npub: 'npub_a' }, { npub: 'npub_b' }];

	it('resolves a ?peer= param to the matching contact', () => {
		const p = peerFromQuery(new URLSearchParams('peer=npub_b'), contacts);
		expect(p?.npub).toBe('npub_b');
	});

	it('is null for an absent param', () => {
		expect(peerFromQuery(new URLSearchParams(''), contacts)).toBeNull();
	});

	it('is null for an unknown npub (degrades to the empty state)', () => {
		expect(peerFromQuery(new URLSearchParams('peer=npub_z'), contacts)).toBeNull();
	});
});
