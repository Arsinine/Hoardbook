// Per-collection content-type selection logic (HOARDBOOK_SPEC §4 — coarse, fixed, multi-select OR).
// Pure + unit-tested. The home view toggles a type in/out and persists via update_collection_meta.
//
// devtest 2026-06-25 #4: the picker "resolved to one even when multiple were selected" because the
// component computed the next set from a STALE per-row snapshot and wrote it AFTER an await — so two
// quick clicks both read the pre-click set and the second overwrote the first. Deriving each toggle
// from the freshest set (modelled here as a fold) makes multi-selection order- and timing-independent.

/** Toggle `value` in/out of `current` (add if absent, remove if present). Pure; never mutates. */
export function toggleContentType(current: readonly string[], value: string): string[] {
	return current.includes(value) ? current.filter((t) => t !== value) : [...current, value];
}
