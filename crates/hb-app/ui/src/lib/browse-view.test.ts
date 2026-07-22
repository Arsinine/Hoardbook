import { describe, expect, it } from 'vitest';
import {
	arrangeItems,
	availabilityBadge,
	collectionAvailability,
	countListingItems,
	dedupAndCap,
	fileTypesPresent,
	flattenTree,
	importedManifestNote,
	parseEstSize,
	paywallTeaser,
	peerAccessBadge,
	peerFromQuery,
	summarizeCollectionsSize,
	type ArrangeableItem,
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

describe('countListingItems (devtest #7 paywall)', () => {
	it('counts every node recursively (files + folders)', () => {
		const tree = [
			{ name: 'a', children: [] },
			{ name: 'b', children: [{ name: 'b1', children: [] }, { name: 'b2', children: [{ name: 'b2a', children: [] }] }] },
		];
		expect(countListingItems(tree)).toBe(5); // a, b, b1, b2, b2a
	});

	it('is 0 for an empty listing and tolerates a missing children field', () => {
		expect(countListingItems([])).toBe(0);
		expect(countListingItems([{ name: 'x' }])).toBe(1);
	});
});

describe('paywallTeaser (M16 W3 — resolves to full tree when upgraded)', () => {
	it('shows the teaser for a truncated collection with hidden items', () => {
		const col = { truncated: true, total_items: 100, listing: [{ name: 'a' }, { name: 'b' }] };
		expect(paywallTeaser(col)).toEqual({ shown: 2, hidden: 98, total: 100 });
	});

	it('returns null for a non-truncated collection so the FULL tree renders', () => {
		// Never-truncated (published whole).
		expect(paywallTeaser({ truncated: false, total_items: 100, listing: [{ name: 'a' }] })).toBeNull();
		// M16 W3: a big-relay upgrade CLEARS `truncated`, so a once-truncated collection now renders
		// whole — this is the "paywall resolves to the full tree when a family is present" case.
		expect(paywallTeaser({ total_items: 100, listing: [{ name: 'a' }] })).toBeNull();
	});

	it('returns null when nothing is actually hidden (shown >= total)', () => {
		expect(
			paywallTeaser({ truncated: true, total_items: 2, listing: [{ name: 'a' }, { name: 'b' }] }),
		).toBeNull();
	});

	it('returns null for an absent collection', () => {
		expect(paywallTeaser(null)).toBeNull();
		expect(paywallTeaser(undefined)).toBeNull();
	});
});

describe('importedManifestNote (M16 W4 — imported full manifest tag)', () => {
	it('returns a dated note once a manifest has been imported', () => {
		const note = importedManifestNote({ manifest_imported_at: 1_700_000_000 });
		expect(note).toMatch(/^Full manifest imported · /);
	});

	it('returns null for a normally-browsed collection', () => {
		expect(importedManifestNote({})).toBeNull();
		expect(importedManifestNote(null)).toBeNull();
		expect(importedManifestNote(undefined)).toBeNull();
	});

	it('the imported (full) collection no longer shows a paywall fade', () => {
		// After import the backend clears `truncated`; the fade lifts and the note takes its place.
		const imported = { total_items: 100, listing: [{ name: 'a' }], manifest_imported_at: 1_700_000_000 };
		expect(paywallTeaser(imported)).toBeNull();
		expect(importedManifestNote(imported)).not.toBeNull();
	});
});

describe('browse-view — file view controls (devtest v0.12.4 #4)', () => {
	const F = (name: string, format?: string, size?: string): ArrangeableItem => ({ name, item_type: 'File', format, size });
	const D = (name: string): ArrangeableItem => ({ name, item_type: 'Folder' });
	const items: ArrangeableItem[] = [
		F('zulu.mkv', 'mkv', '2 GB'),
		D('Season 2'),
		F('alpha.mp4', 'mp4', '700 MB'),
		D('Extras'),
		F('cover.jpg', 'jpg', '512 KB'),
		F('bravo.mkv', 'MKV', '1.5 GB'),
		F('readme', undefined, '2 KB'),
	];

	it('fileTypesPresent lists distinct normalized file formats, sorted; ignores folders + formatless', () => {
		// "MKV" and "mkv" collapse to one; folders (Season 2/Extras) and the formatless `readme` drop out.
		expect(fileTypesPresent(items)).toEqual(['jpg', 'mkv', 'mp4']);
	});

	it('folders always sort before files, and that grouping is NOT flipped by descending sort', () => {
		const asc = arrangeItems(items, { sortKey: 'name', sortDir: 'asc' });
		const desc = arrangeItems(items, { sortKey: 'name', sortDir: 'desc' });
		expect(asc.slice(0, 2).every((i) => i.item_type === 'Folder')).toBe(true);
		expect(desc.slice(0, 2).every((i) => i.item_type === 'Folder')).toBe(true);
		// Folders themselves reverse within their group on desc; files follow after.
		expect(asc.map((i) => i.name).slice(0, 2)).toEqual(['Extras', 'Season 2']);
		expect(desc.map((i) => i.name).slice(0, 2)).toEqual(['Season 2', 'Extras']);
	});

	it('type filter keeps only matching files but ALWAYS keeps folders (navigation must not break)', () => {
		const out = arrangeItems(items, { types: ['mkv'] });
		const folders = out.filter((i) => i.item_type === 'Folder').map((i) => i.name);
		const files = out.filter((i) => i.item_type === 'File').map((i) => i.name);
		expect(folders).toEqual(['Extras', 'Season 2']); // both folders survive the filter
		expect(files.sort()).toEqual(['bravo.mkv', 'zulu.mkv']); // case-insensitive format match
	});

	it('empty type set shows everything', () => {
		expect(arrangeItems(items, { types: [] })).toHaveLength(items.length);
	});

	it('search matches file AND folder names, case-insensitively', () => {
		expect(arrangeItems(items, { search: 'SEASON' }).map((i) => i.name)).toEqual(['Season 2']);
		expect(arrangeItems(items, { search: 'mkv' }).map((i) => i.name).sort()).toEqual(['bravo.mkv', 'zulu.mkv']);
	});

	it('sort by size orders files by parsed bytes within the files group', () => {
		const files = arrangeItems(items, { sortKey: 'size', sortDir: 'asc' }).filter((i) => i.item_type === 'File');
		expect(files.map((i) => i.name)).toEqual(['readme', 'cover.jpg', 'alpha.mp4', 'bravo.mkv', 'zulu.mkv']);
	});

	it('equal-primary items keep an ASCENDING name tiebreak even in descending sort', () => {
		// Two files of equal size: their name tiebreak must stay a→z regardless of sortDir (review #8).
		const eq: ArrangeableItem[] = [
			F('banana.txt', 'txt', '1 MB'),
			F('apple.txt', 'txt', '1 MB'),
		];
		expect(arrangeItems(eq, { sortKey: 'size', sortDir: 'desc' }).map((i) => i.name)).toEqual(['apple.txt', 'banana.txt']);
		expect(arrangeItems(eq, { sortKey: 'size', sortDir: 'asc' }).map((i) => i.name)).toEqual(['apple.txt', 'banana.txt']);
	});

	it('does not mutate the input array', () => {
		const snapshot = items.map((i) => i.name);
		arrangeItems(items, { sortKey: 'size', sortDir: 'desc', search: 'a', types: ['mp4'] });
		expect(items.map((i) => i.name)).toEqual(snapshot);
	});
});
