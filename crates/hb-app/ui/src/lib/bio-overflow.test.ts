// M23 W6 — pure unit test for the bio "more ⌄" overflow decision.
//
// The DOM measurement (`scrollHeight > clientHeight`) cannot be exercised in jsdom (it reports 0
// for both, with no real layout), so this test pins only the comparison seam. The route page's
// `bioMeasure` action is the thin DOM read that feeds this function; it is NOT covered here.

import { describe, it, expect } from 'vitest';
import { bioOverflows } from './bio-overflow.js';

describe('M23 W6 — bioOverflows', () => {
	it('is true when scrollHeight exceeds clientHeight (text is clipped)', () => {
		expect(bioOverflows(60, 40)).toBe(true);
		expect(bioOverflows(41, 40)).toBe(true);
	});

	it('is false when the bio fits exactly within the clamp (equal heights)', () => {
		// A two-line bio that does not wrap to a third line has scrollHeight === clientHeight.
		// The control must not appear for it.
		expect(bioOverflows(40, 40)).toBe(false);
	});

	it('is false when the bio is shorter than the available height (one short line)', () => {
		expect(bioOverflows(20, 40)).toBe(false);
		expect(bioOverflows(1, 40)).toBe(false);
		expect(bioOverflows(0, 0)).toBe(false);
	});
});
