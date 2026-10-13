// QURATOR-351 slice 5 — pure-helper tests for the phonebook. No mounting; every test names the
// exact production edit that must red it (P-10), and each has been performed and seen red.
import { describe, expect, it } from 'vitest';
import { fmtBytes, sortEntries, splitResults, highlight } from './discover-book.js';
import type { PhonebookEntry } from './types.js';

function e(
	npub: string,
	display_name: string,
	total_bytes: number,
	over: Partial<PhonebookEntry> = {},
): PhonebookEntry {
	return {
		npub,
		display_name,
		fingerprint: null,
		is_contact: false,
		total_bytes,
		collections: [],
		tags: [],
		matched: [],
		...over,
	};
}

describe('fmtBytes (the ONE phonebook formatter, moved from the contacts page)', () => {
	it('keeps the four bands and the q351-pinned strings byte-identical', () => {
		// P-10 mutation: in lib/discover-book.ts change the GB band `(bytes / 1e9).toFixed(1)` to
		// `.toFixed(2)` → fmtBytes(1.5e9) becomes "1.50 GB" → this test reds. SEEN RED.
		expect(fmtBytes(1.5e9)).toBe('1.5 GB'); // pinned by q351-contacts-size-looking-for.test.ts
		expect(fmtBytes(880e6)).toBe('880.0 MB'); // pinned likewise
		expect(fmtBytes(5_000)).toBe('5 KB');
		expect(fmtBytes(512)).toBe('512 B');
	});
});

describe('sortEntries', () => {
	it('size mode: biggest first, name asc on ties, input untouched', () => {
		// P-10 mutation: in lib/discover-book.ts flip the size comparator
		// `b.total_bytes - a.total_bytes || a.display_name...` to `a.total_bytes - b.total_bytes || …`
		// → the order inverts → this test reds. SEEN RED (also reds the component size-order test).
		const input = [e('3', 'Zed', 100), e('1', 'Ana', 3e9), e('2', 'Ben', 3e9), e('4', 'Cy', 100)];
		const out = sortEntries(input);
		expect(out.map((x) => x.npub)).toEqual(['1', '2', '4', '3']);
		expect(input.map((x) => x.npub)).toEqual(['3', '1', '2', '4']); // input NOT reordered
	});

	it('likeness mode: likeness desc, null-likeness entries LAST, then size', () => {
		// P-10 mutation: in lib/discover-book.ts flip the nulls-last guard
		// `if (an !== bn) return an - bn;` to `return bn - an;` → the null-score entry sorts FIRST
		// → this test reds. SEEN RED.
		const input = [
			e('n', 'NoScore', 9e9),
			e('hi', 'High', 10, { likeness: 90 }),
			e('mid', 'Mid', 5e9, { likeness: 40 }),
		];
		expect(sortEntries(input, 'likeness').map((x) => x.npub)).toEqual(['hi', 'mid', 'n']);
	});
});

describe('splitResults', () => {
	it('splits Has-it from Looking-for-it; an entry can be in BOTH; each group size-desc', () => {
		// P-10 mutation: in lib/discover-book.ts change the lookingFor filter
		// `e.matched.includes('looking_for')` to `false` → the Looking-for-it group is always empty
		// → this test reds. SEEN RED.
		const p = e('p', 'Pat', 100, { matched: ['looking_for'] });
		const q = e('q', 'Quinn', 200, { matched: ['collection'] });
		const r = e('r', 'Rosa', 300, { matched: ['looking_for', 'name'] });
		const { hasIt, lookingFor } = splitResults([p, q, r]);
		expect(hasIt.map((x) => x.npub)).toEqual(['r', 'q']); // Rosa(300) before Quinn(200)
		expect(lookingFor.map((x) => x.npub)).toEqual(['r', 'p']); // Rosa in BOTH groups
	});
});

describe('highlight', () => {
	it('splits case-insensitively; a miss and an empty query each give one unhit segment', () => {
		// P-10 mutation: in lib/discover-book.ts change `const at = hay.indexOf(q, i);` to
		// `const at = text.indexOf(q, i);` (raw haystack — case-sensitive) → 'kuro' no longer
		// matches 'Kurosawa' → the first assertion reds. SEEN RED.
		expect(highlight('Kurosawa Films', 'kuro')).toEqual([
			{ text: 'Kuro', hit: true },
			{ text: 'sawa Films', hit: false },
		]);
		expect(highlight('Plain', 'zzz')).toEqual([{ text: 'Plain', hit: false }]);
		expect(highlight('Plain', '')).toEqual([{ text: 'Plain', hit: false }]);
		expect(highlight('ab cd ab', 'ab')).toEqual([
			{ text: 'ab', hit: true },
			{ text: ' cd ', hit: false },
			{ text: 'ab', hit: true },
		]);
	});
});
