// §6 Discovery filter-bar view logic (M12 W3). Pure, so the ≥1-filter rule + the tag/content-type
// handling are unit-tested without a DOM. The actual matching (tags AND-intersect, content-types
// OR-union) happens in Rust (`ingest_teasers`); this is the input side.

/** The six coarse content-type categories (mirrors the publish picker + `hb-core` enum). OR-logic. */
export const DISCOVER_CONTENT_TYPES: { value: string; label: string }[] = [
	{ value: 'video', label: 'Video' },
	{ value: 'audio', label: 'Audio' },
	{ value: 'image', label: 'Image' },
	{ value: 'text', label: 'Text' },
	{ value: 'software', label: 'Software' },
	{ value: 'other', label: 'Other' },
];

/** Parse the freeform tag input (comma/space separated) into normalized, deduped tags. */
export function parseTagInput(raw: string): string[] {
	const out: string[] = [];
	for (const piece of raw.split(/[,\s]+/)) {
		const t = piece.trim().toLowerCase();
		if (t && !out.includes(t)) out.push(t);
	}
	return out;
}

/** Whether a search can run: **at least one** tag OR one content-type (§6 — no unfiltered global
 *  peer list). Mirrors the backend's trust-boundary check. */
export function canSearch(tags: string[], contentTypes: string[]): boolean {
	return tags.length > 0 || contentTypes.length > 0;
}

/** Toggle a content-type in the selection (OR semantics across the selected set). */
export function toggleContentType(selected: string[], value: string): string[] {
	return selected.includes(value) ? selected.filter((v) => v !== value) : [...selected, value];
}

/** QURATOR-44: page size for Discover result pagination (replaces the old "showing first N"). */
export const DISCOVER_PAGE_SIZE = 10;

/** QURATOR-44: slice a ranked result set into pages of DISCOVER_PAGE_SIZE. Pure so the pagination
 *  math is unit-tested without a DOM. Page numbers are 1-based. */
export function pageItems<T>(items: T[], page: number): T[] {
	const start = (Math.max(1, page) - 1) * DISCOVER_PAGE_SIZE;
	return items.slice(start, start + DISCOVER_PAGE_SIZE);
}

/** The total page count for a result set (at least 1 so the control never shows "page 1 of 0"). */
export function pageCount(total: number): number {
	return Math.max(1, Math.ceil(total / DISCOVER_PAGE_SIZE));
}

/** QURATOR-70 — the tag-autocomplete suggestion list. Pure so the filter math is unit-tested
 *  without a DOM. Given the full set of observed tags, the already-selected tags, and what the user
 *  has typed, returns the unselected observed tags whose lowercased form contains the typed stem
 *  (case-insensitive substring, matching the Rust single-term fuzzy rule). Capped to keep the
 *  dropdown short. */
export function suggestTags(observed: string[], selected: string[], typed: string, cap = 8): string[] {
	const stem = typed.trim().toLowerCase();
	if (!stem) return [];
	return observed
		.filter((t) => !selected.includes(t))
		.filter((t) => t.toLowerCase().includes(stem))
		.slice(0, cap);
}

/** Why-matched badge (owner sign-off 2026-08-15) — the axis a Discover hit surfaced on, inferred
 *  client-side from the terms THAT SEARCH RAN WITH and the hit's own teaser fields (the Rust
 *  `PeerSearchHit` carries no match-reason field). Mirrors `teaser_matches` (hb-net discover.rs):
 *  tags are the strongest signal, content-types are an OR-union alongside any tag rule, and
 *  name/bio fuzzy-matching is a SINGLE-term rule (two+ tag terms are strict AND-on-tags, so a
 *  multi-term hit that matched did so on tags or not at all). `null` = no confident reason — the
 *  caller omits the badge rather than guessing. */
export type MatchReason = 'tag' | 'content-type' | 'name' | 'bio' | null;

export function matchReason(
	hit: { tags: string[]; content_types: string[]; display_name: string; bio: string | null },
	tags: string[],
	contentTypes: string[],
): MatchReason {
	const terms = tags.map((t) => t.trim().toLowerCase()).filter(Boolean);
	const types = contentTypes.map((t) => t.trim().toLowerCase()).filter(Boolean);
	// "Appears in" = case-insensitive substring, covering both the backend's exact-tag tier and
	// the single-term fuzzy tier in one check.
	const inAny = (haystacks: string[], needle: string) =>
		haystacks.some((h) => h.toLowerCase().includes(needle));
	if (terms.some((t) => inAny(hit.tags, t))) return 'tag';
	// Codex review (2026-08-15): `substring_in_teaser` folds content_types into the SAME single-term
	// haystack as name/bio/tags, so a lone typed term like "video" can match via content_types even
	// when no type chip is selected — the selected-chip check above alone missed that axis.
	const ctCandidates = terms.length === 1 ? [...types, ...terms] : types;
	if (ctCandidates.some((t) => inAny(hit.content_types, t))) return 'content-type';
	// Name/bio apply to single-term searches only, per the backend rule above.
	if (terms.length === 1) {
		const term = terms[0];
		if (hit.display_name.toLowerCase().includes(term)) return 'name';
		if ((hit.bio ?? '').toLowerCase().includes(term)) return 'bio';
	}
	return null;
}
