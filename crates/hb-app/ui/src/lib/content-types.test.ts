import { describe, it, expect } from 'vitest';
import { toggleContentType } from './content-types.js';

describe('toggleContentType (devtest 2026-06-25 #4)', () => {
	it('adds a type when absent and removes it when present', () => {
		expect(toggleContentType([], 'video')).toEqual(['video']);
		expect(toggleContentType(['video'], 'video')).toEqual([]);
	});

	it('accumulates all distinct selections (no rapid-toggle drop)', () => {
		// The bug computed each toggle from a STALE snapshot and wrote after the await, so rapid
		// multi-select dropped all but the last. Deriving each toggle from the freshest set — a fold —
		// must accumulate ALL three, regardless of timing.
		const got = ['video', 'audio', 'image'].reduce(toggleContentType, [] as string[]);
		expect(got).toEqual(['video', 'audio', 'image']);
	});

	it('is order-independent and preserves earlier selections when toggling a new one', () => {
		const after = toggleContentType(['video', 'audio'], 'image');
		expect(after).toEqual(['video', 'audio', 'image']);
		// Removing the middle one leaves the rest intact.
		expect(toggleContentType(after, 'audio')).toEqual(['video', 'image']);
	});

	it('does not mutate the input array', () => {
		const input = ['video'];
		toggleContentType(input, 'audio');
		expect(input).toEqual(['video']);
	});
});
