import { describe, expect, it } from 'vitest';
import {
	isIncluded,
	hasDescendantUnder,
	triState,
	serializeInclude,
	selectAllTopLevel,
} from './scan-tree.js';

const set = (...xs: string[]) => new Set(xs);

describe('scan-tree — isIncluded / hasDescendantUnder (mirror of the Rust IncludeSet)', () => {
	it('is_included: a node is included if it is checked or lives under a checked ancestor', () => {
		const checked = set('a', 'x/y');
		expect(isIncluded('a', checked)).toBe(true); // exact
		expect(isIncluded('a/b', checked)).toBe(true); // under checked a
		expect(isIncluded('a/b/c', checked)).toBe(true); // deep under checked a
		expect(isIncluded('x/y', checked)).toBe(true); // exact
		expect(isIncluded('x/y/z', checked)).toBe(true); // under checked x/y
		expect(isIncluded('x', checked)).toBe(false); // x is only an ancestor of x/y
		expect(isIncluded('ab', checked)).toBe(false); // separator-respecting: 'a' is not a prefix of 'ab'
	});

	it('has_descendant_under: true iff some checked path lives strictly below rel', () => {
		const checked = set('a', 'x/y');
		expect(hasDescendantUnder('x', checked)).toBe(true); // x/y is below x
		expect(hasDescendantUnder('a', checked)).toBe(false); // a is itself checked, no checked descendant
		expect(hasDescendantUnder('x/y', checked)).toBe(false); // x/y is the leaf, nothing below it checked
		expect(hasDescendantUnder('q', checked)).toBe(false);
	});
});

describe('scan-tree — triState precedence (F7: locked beats indeterminate, explicit beats locked)', () => {
	it('an explicitly-checked node is "checked"', () => {
		expect(triState('a', set('a'))).toBe('checked');
	});

	it('a node included via a checked ancestor is "locked"', () => {
		expect(triState('a/b', set('a'))).toBe('locked');
		expect(triState('a/b/c', set('a'))).toBe('locked');
	});

	it('a node with a checked descendant but not itself included is "indeterminate"', () => {
		expect(triState('a', set('a/b'))).toBe('indeterminate');
	});

	it('a node neither included nor an ancestor-of-checked is "unchecked"', () => {
		expect(triState('z', set('a'))).toBe('unchecked');
	});

	// F7 (i) overlap: checked = {a, a/b/c} → triState(a/b) = locked (ancestor a wins over the
	// descendant-indeterminate from a/b/c).
	it('(i) overlap — ancestor-locked beats descendant-indeterminate', () => {
		const checked = set('a', 'a/b/c');
		expect(triState('a/b', checked)).toBe('locked');
	});

	// F7 (ii) deep-only: checked = {a/b/c} → ancestors are indeterminate, the leaf is checked.
	it('(ii) deep-only — ancestors indeterminate, leaf checked', () => {
		const checked = set('a/b/c');
		expect(triState('a', checked)).toBe('indeterminate');
		expect(triState('a/b', checked)).toBe('indeterminate');
		expect(triState('a/b/c', checked)).toBe('checked');
	});

	// F7 (iv) independently-checked child vs locked: an explicit check on a node that is ALSO under a
	// checked ancestor still reads as "checked", and survives unchecking the ancestor.
	it('(iv) explicit check beats locked, and survives unchecking the ancestor', () => {
		const both = set('a', 'a/b');
		expect(triState('a/b', both)).toBe('checked'); // explicit wins over locked
		// uncheck the ancestor:
		const afterUncheckParent = set('a/b');
		expect(triState('a/b', afterUncheckParent)).toBe('checked'); // explicit check persists
		// a purely-LOCKED sibling disappears once its only ancestor is unchecked:
		expect(triState('a/x', both)).toBe('locked');
		expect(triState('a/x', afterUncheckParent)).toBe('unchecked');
	});
});

describe('scan-tree — serializeInclude (F7 iii: drop redundant deep checks under a checked ancestor)', () => {
	it('(iii) drops a child when an ancestor is also checked', () => {
		expect(serializeInclude(set('a', 'a/b')).sort()).toEqual(['a']);
	});

	it('keeps independent selections', () => {
		expect(serializeInclude(set('a', 'x/y')).sort()).toEqual(['a', 'x/y']);
	});

	it('drops every descendant under a single checked ancestor', () => {
		expect(serializeInclude(set('a', 'a/b', 'a/b/c', 'x')).sort()).toEqual(['a', 'x']);
	});

	it('an empty selection serializes to []', () => {
		expect(serializeInclude(set())).toEqual([]);
	});
});

describe('scan-tree — selectAllTopLevel', () => {
	it('checks every top-level node name', () => {
		expect(selectAllTopLevel(['a', 'x', 'films'])).toEqual(set('a', 'x', 'films'));
	});

	it('empty input → empty set', () => {
		expect(selectAllTopLevel([])).toEqual(set());
	});
});
