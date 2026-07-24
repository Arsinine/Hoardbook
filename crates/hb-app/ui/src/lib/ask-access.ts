// M17 W2 — "Ask for access" ramps. ONE pure helper owns the prefill copy (owner may reword at
// kickoff — decision 1 — which is exactly why the copy lives in one pinned place). The
// intent→draft decision is also pure so the no-clobber and no-send rules are unit-testable
// without a DOM. Sending is NEVER this helper's job: it only returns the text to place in the
// composer (or, when the user typed something deliberate, the untouched text) and whether the
// composer should be focused. "No auto-send" is structural — the helper has no network surface.
//
// Headline failure mode 5: "the intent populates an empty draft only; nothing is ever published
// without the user's Send."

/** The prefill text for the "Ask for access" ramp. Petname is inserted after "Hi" when present,
 *  elided to the bare "Hi —" form when absent. A whitespace-only petname is treated as absent. */
export function askAccessDraft(petname: string): string {
	const name = petname.trim();
	if (name) return `Hi ${name} — could I have your share code? I'd like to browse your collections.`;
	return `Hi — could I have your share code? I'd like to browse your collections.`;
}

/** The pure intent→draft decision. Given the URL `intent` param value, the current composer draft,
 *  and the peer's petname, returns the draft to apply and whether to focus the composer.
 *
 *  - `ask-access` + empty draft → populate from `askAccessDraft`, focus the composer.
 *  - `ask-access` + non-empty draft → DO NOT clobber (the user typed something deliberate); just
 *    focus so they can review/send. (Whitespace-only drafts are treated as empty — the textarea
 *    trims on send anyway, so populating is correct and matches "empty draft only".)
 *  - any other / absent intent → no-op (the composer behaves exactly as before).
 *
 *  Never sends. The caller assigns `.draft` to the composer state variable; Send is a separate
 *  button-driven path. */
export function applyAskAccessIntent(
	intent: string | null | undefined,
	existingDraft: string,
	petname: string,
): { draft: string; focus: boolean } {
	if (intent !== 'ask-access') return { draft: existingDraft, focus: false };
	if (existingDraft.trim() !== '') return { draft: existingDraft, focus: true };
	return { draft: askAccessDraft(petname), focus: true };
}
