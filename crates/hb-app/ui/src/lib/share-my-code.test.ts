// M17 W4 — "Share my code" grant leg. The cursor-aware insert is pure (no DOM, no Tauri), so the
// "inserts the exact code at the cursor without sending" rule is asserted against real logic rather
// than a source scan. The tooltip copy is pinned verbatim from one source so owner-rewording
// touches exactly one place.
//
//   - `SHARE_MY_CODE_WARNING` — the single source of the tooltip line.
//   - `insertAtCursor(text, insert, start, end)` — the cursor-aware splice + caret result.

import { describe, it, expect } from 'vitest';
import { SHARE_MY_CODE_WARNING, insertAtCursor, withdrawInsert } from './share-my-code.js';

describe('withdrawInsert — the grant does not ride a conversation switch', () => {
	const CODE = 'hbk1abcdefgh';

	it('removes the inserted code and keeps the typed text', () => {
		expect(withdrawInsert(`here you go ${CODE} enjoy`, CODE)).toBe('here you go enjoy');
	});

	it('empties a draft that was nothing but the code', () => {
		expect(withdrawInsert(CODE, CODE)).toBe('');
		expect(withdrawInsert(`  ${CODE}  `, CODE)).toBe('');
	});

	it('removes only the FIRST occurrence (one click, one grant)', () => {
		const out = withdrawInsert(`${CODE} and ${CODE}`, CODE);
		expect(out).toBe(`and ${CODE}`);
	});

	it('is a no-op when the code is not in the draft', () => {
		expect(withdrawInsert('just a message', CODE)).toBe('just a message');
		expect(withdrawInsert('', CODE)).toBe('');
	});

	it('round-trips insertAtCursor: insert then withdraw restores the original draft', () => {
		for (const [text, pos] of [
			['', 0],
			['hello', 0],
			['hello', 5],
			['hello world', 6],
		] as const) {
			const { value } = insertAtCursor(text, CODE, pos, pos);
			expect(withdrawInsert(value, CODE)).toBe(text);
		}
	});
});

describe('SHARE_MY_CODE_WARNING — single source of the tooltip copy', () => {
	it('is pinned verbatim (owner may reword — this is the one place that changes)', () => {
		expect(SHARE_MY_CODE_WARNING).toBe(
			'Your share code grants browse access to your public collections. Anyone holding it can decrypt your listings.',
		);
	});

	it('carries the blunt grant warning (onboarding register: holding the code = access)', () => {
		// Regression guard: the copy must state the consequence — a holder can decrypt listings.
		const lower = SHARE_MY_CODE_WARNING.toLowerCase();
		expect(lower).toContain('share code');
		expect(lower).toContain('decrypt');
		expect(lower).toContain('listings');
	});
});

describe('insertAtCursor — cursor-aware draft insert (never sends)', () => {
	it('inserts into an empty draft and places the caret after the code', () => {
		// The empty-draft case: the composer affordance must still work (the spec calls out sharing
		// into an empty draft — "do NOT disable on empty draft").
		const r = insertAtCursor('', 'hbk1abcd', 0, 0);
		expect(r.value).toBe('hbk1abcd');
		expect(r.cursor).toBe(8);
	});

	it('inserts at the start of a non-empty draft', () => {
		const r = insertAtCursor('hello', 'hbk1abcd', 0, 0);
		expect(r.value).toBe('hbk1abcdhello');
		expect(r.cursor).toBe(8); // caret lands right after the inserted code
	});

	it('inserts in the middle of a draft (respects the cursor position)', () => {
		// "The draft insert respects cursor position": caret at position 7 → code splices between
		// "here is " and " my code".
		const r = insertAtCursor('here is  my code', 'hbk1abcd', 8, 8);
		expect(r.value).toBe('here is hbk1abcd my code');
		expect(r.cursor).toBe(16); // 8 (lo) + 8 (insert length)
	});

	it('inserts at the end of a draft', () => {
		const r = insertAtCursor('here is ', 'hbk1abcd', 8, 8);
		expect(r.value).toBe('here is hbk1abcd');
		expect(r.cursor).toBe(16);
	});

	it('replaces a selection range [start, end) with the inserted code', () => {
		// The replace-on-selection case: the affordance pastes over a highlighted span, matching how
		// a textarea replaces selected text on input. 'swap [this] out': [this] spans [5,11).
		const r = insertAtCursor('swap [this] out', 'hbk1abcd', 5, 11); // replaces "[this]"
		expect(r.value).toBe('swap hbk1abcd out');
		expect(r.cursor).toBe(13); // 5 (lo) + 8 (insert length)
	});

	it('normalizes a reversed selection (start > end) to the lo/hi span', () => {
		// A textarea can report start > end when the user selected right-to-left. The splice must
		// still cover the same span as the forward selection.
		const forward = insertAtCursor('swap [this] out', 'hbk1abcd', 6, 11);
		const reverse = insertAtCursor('swap [this] out', 'hbk1abcd', 11, 6);
		expect(reverse.value).toBe(forward.value);
		expect(reverse.cursor).toBe(forward.cursor);
	});

	it('returns the draft unchanged (caret after insert of length 0) for an empty insert', () => {
		// A zero-length insert must not corrupt the draft — the caret advances by 0.
		const r = insertAtCursor('hello', '', 2, 2);
		expect(r.value).toBe('hello');
		expect(r.cursor).toBe(2);
	});

	it('defensively appends at the end when start/end are NaN (stale ref guard)', () => {
		// A stale textarea ref (unmounted, or focus lost mid-click) can hand back NaN bounds. The
		// helper must NOT silently truncate — it appends at the end so the user's text is preserved.
		const r = insertAtCursor('hello', 'hbk1abcd', NaN, NaN);
		expect(r.value).toBe('hellohbk1abcd');
		expect(r.cursor).toBe(13);
	});

	it('defensively clamps out-of-range bounds to the draft length', () => {
		// Bounds beyond the string length clamp to len (append), never splice past the end.
		const r = insertAtCursor('hi', 'hbk1abcd', 99, 99);
		expect(r.value).toBe('hihbk1abcd');
		expect(r.cursor).toBe(10);
	});

	it('clamps negative bounds to the end (out-of-range → append, per spec)', () => {
		// Per the spec: out-of-range/NaN bounds append at the end defensively. Negative bounds are
		// out-of-range, so they clamp to len (append) — NOT to 0 (which would insert at the start).
		// The invariant is "a stale ref never silently truncates the user's text", and append-up is
		// the least-surprising choice.
		const r = insertAtCursor('hi', 'hbk1abcd', -5, -5);
		expect(r.value).toBe('hihbk1abcd');
		expect(r.cursor).toBe(10);
	});

	it('caret result always equals lo + insert.length (the position to restore)', () => {
		// Structural guard: the returned caret is always `lo + insert.length`, where lo is the
		// clamped lower bound, so the caller's `setSelectionRange(cursor, cursor)` is always correct.
		for (const [text, ins, s, e] of [
			['', 'x', 0, 0],
			['abc', 'x', 1, 1],
			['abc', 'x', 1, 2],
			['abc', 'xy', 3, 3],
		] as const) {
			const r = insertAtCursor(text, ins, s, e);
			const clamp = (n: number) =>
				Number.isFinite(n) && n >= 0 && n <= text.length ? n : text.length;
			const lo = Math.min(clamp(s), clamp(e));
			expect(r.cursor).toBe(lo + ins.length);
		}
	});
});
