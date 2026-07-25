// M17 W4 — "Share my code" in the composer (grant leg). ONE pure helper owns the cursor-aware
// draft insert (the exact copy for the tooltip lives here too, so owner-rewording touches one
// pinned place). "Insert-then-send is already two deliberate acts" — so there is no confirm modal
// (the owner may override; we ship the no-modal default) and the helper NEVER sends: it only
// returns the spliced draft and the new caret position. Sending is the Send button's job.
//
// Headline acceptance: click inserts the exact code into the draft at the cursor, without sending.

/** The blunt tooltip line carried by the composer's "Share my code" affordance. Lifts the
 *  onboarding copy register: this is a grant — anyone holding the code can decrypt your listings. */
export const SHARE_MY_CODE_WARNING =
	'Your share code grants browse access to your public collections. Anyone holding it can decrypt your listings.';

/** Pure cursor-aware draft insert. Splices `insert` into `text` over the `[start, end)` selection
 *  (or at `start` when `start === end`) and returns the new draft plus the caret position to
 *  restore (`start + insert.length`). Fully unit-testable — no DOM, no Tauri, no network.
 *
 *  Defensive on bad bounds: a `NaN` or out-of-range `start`/`end` clamps to the end of the draft
 *  (append) so a stale ref never silently truncates the user's text. */
export function insertAtCursor(
	text: string,
	insert: string,
	start: number,
	end: number,
): { value: string; cursor: number } {
	const len = text.length;
	const s = Number.isFinite(start) && start >= 0 && start <= len ? start : len;
	const e = Number.isFinite(end) && end >= 0 && end <= len ? end : len;
	const lo = Math.min(s, e);
	const hi = Math.max(s, e);
	const value = text.slice(0, lo) + insert + text.slice(hi);
	return { value, cursor: lo + insert.length };
}
