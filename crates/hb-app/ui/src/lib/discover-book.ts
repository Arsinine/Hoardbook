// QURATOR-351 slice 5 — pure helpers for the Discover view (the phonebook, draft r5 states a/b/c).
// No DOM, no Svelte — unit-testable without mounting (see discover-book.test.ts).
import type { PhonebookEntry, MatchKind } from './types.js';

/** The ONE byte formatter for the phonebook surfaces. Moved VERBATIM from
 *  routes/contacts/+page.svelte (which now imports this — same output, the q351 contacts test
 *  pins "1.5 GB" / "880.0 MB"). Browse and CollectionRow keep their own component-local copies;
 *  consolidating those was explicitly out of scope. Goes up to TB (QURATOR-368 slice a). */
export function fmtBytes(bytes: number): string {
	if (bytes > 1e12) return (bytes / 1e12).toFixed(1) + ' TB';
	if (bytes > 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
	if (bytes > 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
	if (bytes > 1e3) return (bytes / 1e3).toFixed(0) + ' KB';
	return bytes + ' B';
}

export type BookSort = 'size' | 'likeness';

/** State-a order. 'size' (default, owner-greenlit): biggest hoard first, name asc on ties —
 *  "Default sort: size, then name (greenlit as drawn)". 'likeness': likeness desc, and
 *  null-likeness entries sort LAST (they have no score — the UI shows no percentage at all,
 *  never "0%"), size desc as the tiebreak. Returns a new array; never mutates the input. */
export function sortEntries(entries: PhonebookEntry[], mode: BookSort = 'size'): PhonebookEntry[] {
	const out = [...entries];
	if (mode === 'likeness') {
		out.sort((a, b) => {
			const an = a.likeness == null ? 1 : 0;
			const bn = b.likeness == null ? 1 : 0;
			if (an !== bn) return an - bn;
			if ((b.likeness ?? 0) !== (a.likeness ?? 0)) return (b.likeness ?? 0) - (a.likeness ?? 0);
			return b.total_bytes - a.total_bytes;
		});
	} else {
		out.sort((a, b) => b.total_bytes - a.total_bytes || a.display_name.localeCompare(b.display_name));
	}
	return out;
}

/** Match kinds that land an entry in the "Has it" group. `title` matches reach only cached
 *  listings (draft r5 data-gap 4) — the entry shows a `title` chip so the user knows WHY. */
const HAS_IT_KINDS: MatchKind[] = ['name', 'collection', 'tag', 'title'];

/** State-b groups. "Has it" = matched by name/collection/tag/title; "Looking for it" = matched
 *  by the looking-for line. An entry CAN appear in both. Each group is size-desc ("biggest
 *  first", as the mock's res-count says). */
export function splitResults(entries: PhonebookEntry[]): {
	hasIt: PhonebookEntry[];
	lookingFor: PhonebookEntry[];
} {
	const bySize = sortEntries(entries, 'size');
	return {
		hasIt: bySize.filter((e) => e.matched.some((k) => HAS_IT_KINDS.includes(k))),
		lookingFor: bySize.filter((e) => e.matched.includes('looking_for')),
	};
}

export interface HighlightSegment {
	text: string;
	hit: boolean;
}

/** Case-insensitive substring split for <mark class="hit"> rendering. A miss (or an empty
 *  query) returns ONE unhit segment, so a caller can loop unconditionally. */
export function highlight(text: string, query: string): HighlightSegment[] {
	const q = query.trim().toLowerCase();
	if (!q) return [{ text, hit: false }];
	const hay = text.toLowerCase();
	const out: HighlightSegment[] = [];
	let i = 0;
	for (;;) {
		const at = hay.indexOf(q, i);
		if (at === -1) break;
		if (at > i) out.push({ text: text.slice(i, at), hit: false });
		out.push({ text: text.slice(at, at + q.length), hit: true });
		i = at + q.length;
	}
	if (out.length === 0) return [{ text, hit: false }];
	if (i < text.length) out.push({ text: text.slice(i), hit: false });
	return out;
}
