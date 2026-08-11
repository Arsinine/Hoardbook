// M23 W6 — bio "more ⌄" overflow decision (pure seam, no DOM).
//
// The face card clamps the bio to 2 visual lines via `-webkit-line-clamp: 2`. Whether a given
// string wraps to a second line depends on card width, font and zoom — NOT on a character count.
// So the decision to show the `more ⌄` control must come from a real layout measurement
// (`scrollHeight > clientHeight` on the clamped element), not a length heuristic.
//
// This module holds only the comparison predicate so it can be unit-tested in isolation. The
// actual DOM read happens in the `bioMeasure` Svelte action on the route page, which feeds the
// live `scrollHeight` / `clientHeight` values into this function.

/** True when the clamped bio is taller than its visible box (i.e. it actually overflows). */
export function bioOverflows(scrollHeight: number, clientHeight: number): boolean {
	// Strictly greater: a bio that exactly fills two lines with no third line clipped has equal
	// heights and should NOT show the control. A sub-pixel rounding margin would hide `more ⌄` for
	// bios that are barely overflowing; the raw comparison is what the browser reports and what the
	// CSS clamp resolves against, so it stays exact.
	return scrollHeight > clientHeight;
}
