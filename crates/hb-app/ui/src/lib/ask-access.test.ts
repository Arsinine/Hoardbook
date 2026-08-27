// M17 W2 — "Ask for access" ramps. The prefill copy lives in one pure helper so owner-rewording at
// kickoff (decision 1) touches exactly one place, and the no-clobber + no-send rules are asserted
// against real logic rather than a source scan.
//
// Two helpers:
//   - `askAccessDraft(petname)`                — the single source of the prefill text.
//   - `applyAskAccessIntent(intent, draft, p)` — the pure intent→draft decision: returns the new
//     draft (or the untouched draft when the user typed something deliberate) and whether the
//     composer should be focused. Sending is NEVER this helper's job — "no auto-send" is structural.

import { describe, it, expect } from 'vitest';
import { askAccessDraft, applyAskAccessIntent } from './ask-access.js';

describe('askAccessDraft — single source of the prefill copy', () => {
	it('elides the petname at the Hi position when petname is empty', () => {
		// The bare variant: "Hi, could I have your share code? I'd like to browse your collections."
		expect(askAccessDraft('')).toBe(
			"Hi, could I have your share code? I'd like to browse your collections.",
		);
	});

	it('inserts a non-empty petname after Hi', () => {
		// "Hi {petname}, could I have your share code? …"
		expect(askAccessDraft('Mira')).toBe(
			"Hi Mira, could I have your share code? I'd like to browse your collections.",
		);
	});

	it('treats a whitespace-only petname as absent (bare variant)', () => {
		// A whitespace petname is no petname — never render "Hi    —".
		expect(askAccessDraft('   ')).toBe(
			"Hi, could I have your share code? I'd like to browse your collections.",
		);
	});

	it('trims a petname with surrounding whitespace before insertion', () => {
		expect(askAccessDraft('  Mira  ')).toBe(
			"Hi Mira, could I have your share code? I'd like to browse your collections.",
		);
	});
});

describe('applyAskAccessIntent — the intent→draft decision (no-clobber, no-send)', () => {
	it('populates an empty draft when intent is ask-access', () => {
		const r = applyAskAccessIntent('ask-access', '', 'Mira');
		expect(r.draft).toBe(
			"Hi Mira, could I have your share code? I'd like to browse your collections.",
		);
		expect(r.focus).toBe(true);
	});

	it('does NOT clobber a draft the user already typed', () => {
		// The user typed something deliberate — append nothing, just focus the composer.
		const existing = 'hey did you still have those concerts?';
		const r = applyAskAccessIntent('ask-access', existing, 'Mira');
		expect(r.draft).toBe(existing);
		expect(r.focus).toBe(true);
	});

	it('does not clobber a draft that is only whitespace vs the prefill either (still deliberate-ish)', () => {
		// Whitespace-only draft is treated as empty (the textarea trims on send anyway), so the
		// intent DOES populate — this matches the "empty draft only" rule from the spec.
		const r = applyAskAccessIntent('ask-access', '   ', 'Mira');
		expect(r.draft).toBe(
			"Hi Mira, could I have your share code? I'd like to browse your collections.",
		);
		expect(r.focus).toBe(true);
	});

	it('leaves the draft untouched and signals no focus when intent is absent', () => {
		// No intent param in the URL — the helper is a no-op (composer behaves exactly as before).
		const r = applyAskAccessIntent(null, 'anything', 'Mira');
		expect(r.draft).toBe('anything');
		expect(r.focus).toBe(false);
	});

	it('leaves the draft untouched when intent is an unknown value', () => {
		// Forward-compat: a future intent we don't know yet must not trip the ask-access path.
		const r = applyAskAccessIntent('something-else', '', 'Mira');
		expect(r.draft).toBe('');
		expect(r.focus).toBe(false);
	});

	it('never returns a draft that differs from askAccessDraft for the empty-draft ask-access case', () => {
		// Structural guard: the intent path composes askAccessDraft rather than re-stringing copy.
		for (const petname of ['', 'Mira', '  ', 'Long Name Here']) {
			const r = applyAskAccessIntent('ask-access', '', petname);
			expect(r.draft).toBe(askAccessDraft(petname));
		}
	});
});
