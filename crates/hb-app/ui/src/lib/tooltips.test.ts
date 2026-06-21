import { describe, expect, it } from 'vitest';
import { TOOLTIPS, TOOLTIP_KEYS, type TooltipKey } from './tooltips.js';

describe('tooltips — feature-help registry (HOARDBOOK_SPEC §8)', () => {
	it('has exactly the five spec-anchored keys, no more, no less (drift guard)', () => {
		const expected: TooltipKey[] = [
			'no-download',
			'willing-to',
			'listings-locked',
			'k-of-n-folders',
			'fingerprint',
		];
		expect(new Set(TOOLTIP_KEYS)).toEqual(new Set(expected));
		expect(Object.keys(TOOLTIPS).sort()).toEqual([...expected].sort());
		expect(TOOLTIP_KEYS).toHaveLength(5);
	});

	it('every key resolves to a non-empty { title, body }', () => {
		for (const key of TOOLTIP_KEYS) {
			const entry = TOOLTIPS[key];
			expect(entry, key).toBeDefined();
			expect(entry.title.trim().length, `${key} title`).toBeGreaterThan(0);
			expect(entry.body.trim().length, `${key} body`).toBeGreaterThan(0);
		}
	});

	// Product-promise regression guard: the no-download copy must carry the H4/INV-4 invariant.
	it('no-download body asserts the "moves no files" product promise', () => {
		expect(TOOLTIPS['no-download'].body.toLowerCase()).toContain('moves no files');
	});

	// The locked-listing copy lifts the spec's verbatim explanation.
	it('listings-locked body explains the npub-without-share-code state', () => {
		const body = TOOLTIPS['listings-locked'].body.toLowerCase();
		expect(body).toContain('npub');
		expect(body).toContain('share code');
	});

	// Fingerprint copy must state it binds to the npub, not the display name.
	it('fingerprint body binds the fingerprint to the npub, not the display name', () => {
		const body = TOOLTIPS['fingerprint'].body.toLowerCase();
		expect(body).toContain('npub');
		expect(body).toContain('display name');
	});
});
