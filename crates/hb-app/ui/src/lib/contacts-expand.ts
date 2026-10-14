// QURATOR-368 slice d — the hover-to-expand column's width cap (pure, so the floor rule is
// provable without layout). Owner rulings, 2026-10-14:
//   the column animates to a FIXED expanded width (rest 300 → 540); the right pane shrinks by
//   the same amount and NEVER goes below its 320px floor; when the window is too narrow for the
//   full expansion to fit above that floor, the expansion is CAPPED to what fits; when even the
//   floor's worth of growth doesn't fit, there is no expansion at all.

/** Rest width of the contact column (px). */
export const CONTACTS_COL_REST = 300;
/** Expanded width the column animates to (px). */
export const CONTACTS_COL_WIDE = 540;
/** The right pane's floor (px) — the pane never shrinks below this (owner ruling). */
export const CONTACTS_PANE_FLOOR = 320;

/**
 * Returns the expanded column width in px for the given rest width of the right pane, or
 * `null` when no expansion fits (the column must stay at rest).
 */
export function expandedColumnWidth(
	restPaneWidth: number,
	rest: number = CONTACTS_COL_REST,
	wide: number = CONTACTS_COL_WIDE,
	floor: number = CONTACTS_PANE_FLOOR,
): number | null {
	if (!Number.isFinite(restPaneWidth) || restPaneWidth <= 0) return null;
	if (restPaneWidth - (wide - rest) >= floor) return wide;
	const capped = rest + (restPaneWidth - floor);
	return capped > rest ? capped : null;
}
