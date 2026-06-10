import { describe, it, expect } from 'vitest';
import type { Collection } from './types.js';

// HANDOVER scenario 4: the backend field is plural `content_types: Vec<String>`,
// but CollectionPanel.svelte and browse/+page.svelte read the singular
// `content_type`; `undefined[0]` / `.length` threw and blanked the whole render
// whenever a collection existed (this is what surfaced as "Add Collection stuck
// on scanning"). svelte-check in CI is the build-level guard against the singular
// access reappearing in the templates; this test locks the data contract and the
// null-safety the fix relies on.

function makeCollection(overrides: Partial<Collection> = {}): Collection {
	return {
		slug: 'criterion',
		path_alias: 'Criterion Collection',
		item_count: 3,
		total_bytes: 1024,
		content_types: ['video/mp4', 'image/png', 'application/pdf', 'audio/flac'],
		tags: [],
		languages: [],
		last_updated: '2026-06-09T00:00:00Z',
		listing: [],
		...overrides,
	};
}

describe('Collection content_types render contract', () => {
	it('exposes the plural content_types field, not singular content_type', () => {
		const col = makeCollection();
		expect(Array.isArray(col.content_types)).toBe(true);
		// The singular field the buggy templates read must not exist on the type/value.
		expect('content_type' in col).toBe(false);
	});

	it('derives the format badge from content_types[0] (CollectionPanel.svelte)', () => {
		const col = makeCollection();
		// Mirrors: $: fmt = collection.content_types?.[0] ?? '';
		const fmt = col.content_types?.[0] ?? '';
		expect(fmt).toBe('video/mp4');
	});

	it('renders up to three content_type tags (browse/+page.svelte)', () => {
		const col = makeCollection();
		// Mirrors: {#if (col.content_types?.length ?? 0) > 0} {#each (col.content_types ?? []).slice(0, 3) as t}
		expect((col.content_types?.length ?? 0) > 0).toBe(true);
		const badges = (col.content_types ?? []).slice(0, 3);
		expect(badges).toEqual(['video/mp4', 'image/png', 'application/pdf']);
	});

	it('does not throw when content_types is absent (e.g. older/partial payload)', () => {
		// A payload missing the field must degrade gracefully, never crash render.
		const partial = makeCollection();
		delete (partial as { content_types?: string[] }).content_types;
		expect(() => {
			const fmt = partial.content_types?.[0] ?? '';
			const badges = (partial.content_types ?? []).slice(0, 3);
			expect(fmt).toBe('');
			expect(badges).toEqual([]);
		}).not.toThrow();
	});
});
