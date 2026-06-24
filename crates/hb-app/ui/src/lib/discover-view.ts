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
