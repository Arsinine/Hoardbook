// M17 W3 — share-code detection. The consume leg of a received share code: scan a chat message's
// text for a bech32 share-code candidate (`hbk1…` or `npub1…`) and surface the FIRST one per message
// for card rendering. Detection is the cheap, pure front of a two-stage pipeline:
//
//   1. (here) candidate scan — a regex sweep returning the first plausible token (length-windowed so
//      a short `npub1` fragment or a stray `hbk1` inside a word doesn't fire). Pure, no Tauri.
//   2. (caller) checksum validation — `validate_share_code` + `share_code_info` (both LOCAL Tauri
//      commands, zero network). An invalid checksum ⇒ plain text, no card. The verdict is cached per
//      message id so a history re-render is free (see the chat page wiring).
//
// Headline failure modes guarded here:
//   - "the card renders for any `hbk1`-prefixed substring" → the length window + charset filter out
//     short fragments and non-bech32 noise before the checksum gate ever runs.
//   - "multiple codes clutter the thread" → FIRST valid candidate per message gets the card (spec
//     W3: "First valid candidate per message"). The caller passes `validate(code) → bool` so the
//     "first VALID" selection (not just first candidate) is unit-testable without a DOM.
//   - "the message text disappears" → the card is always an ADDENDUM; this helper returns the code
//     and the caller renders the verbatim text above it. `extractShareCodeCandidate` never mutates
//     or elides the message.

/** Bech32 charset (data part). A real share code's data part is all in this set. */
const BECH32_CHARSET = /^[0-9a-z]+$/;

/**
 * Lower bound on a real share code's TOTAL length. A bare `npub1` is 63 chars; a full `hbk1…` is 90.
 * Use 58 (63 − 5 slack) so a truncated-by-copy/paste tail is still caught by the checksum gate
 * (which will reject it), rather than silently ignored. Shorter fragments (e.g. a stray "npub1" typed
 * in prose) never reach validation.
 */
const MIN_CODE_LEN = 58;
/** Upper bound — anything past this is not a valid bech32 code (npub=63, hbk=90; allow margin to 120). */
const MAX_CODE_LEN = 120;

/** The greedy token a candidate scan considers: an `hbk1`/`npub1` prefix + a run of bech32 chars. */
const TOKEN_RE = /(?:hbk1|npub1)[0-9a-z]+/g;

/** Is `s` a length+charset-plausible share code (the cheap pre-checksum gate)? Pure. */
export function isShareCodeCandidate(s: string): boolean {
	if (s.length < MIN_CODE_LEN || s.length > MAX_CODE_LEN) return false;
	if (!s.startsWith('hbk1') && !s.startsWith('npub1')) return false;
	return BECH32_CHARSET.test(s);
}

/**
 * Scan `text` for the first bech32 share-code candidate, then gate it through `validate` (the
 * caller's LOCAL `validate_share_code` wrapper — no network). Returns the first VALID candidate's
 * raw code string, or `null` when there is none (the message renders as plain text). Pure: it has no
 * Tauri surface of its own — `validate` is a function the caller supplies so this is unit-testable
 * without a DOM. "First VALID per message" is the spec rule (W3: first valid candidate gets the card).
 *
 * The token regex is greedy on the bech32 charset, so a code followed by punctuation or a space is
 * captured cleanly; a code immediately abutting a non-bech32 letter (e.g. inside "Xhbk1…") is not
 * matched at that position (the `hbk1` must start the token). This is the right behaviour: a share
 * code embedded in prose is surrounded by whitespace/punctuation, never mashed into a word.
 */
export function extractShareCodeCandidate(
	text: string,
	validate: (code: string) => boolean,
): string | null {
	const matches = text.match(TOKEN_RE);
	if (!matches) return null;
	// Walk candidates in order of appearance; return the first one that passes the length/charset
	// gate AND the caller's checksum validation. A later invalid candidate never shadows an earlier
	// valid one, and an earlier INVALID candidate is skipped (so "first VALID" holds, not "first").
	for (const raw of matches) {
		// Trim trailing bech32-charset chars until the slice is within the length window — a token
		// greedily consumed past a code's real end (e.g. two codes run together) is trimmed to its
		// plausible window before the checksum decides. If no slice in the window validates, skip.
		if (raw.length <= MAX_CODE_LEN) {
			if (isShareCodeCandidate(raw) && validate(raw)) return raw;
			continue;
		}
		// Over-long greedy token: try the plausible-length prefixes (rare; mainly two abutting codes).
		for (let len = MAX_CODE_LEN; len >= MIN_CODE_LEN; len--) {
			const slice = raw.slice(0, len);
			if (isShareCodeCandidate(slice) && validate(slice)) return slice;
		}
	}
	return null;
}

/** Every candidate string `extractShareCodeCandidate` may checksum-test for `text`, in test order.
 *  The route pre-validates exactly these (raw tokens AND the over-long-token prefix slices) so the
 *  slice-recovery path in `extractShareCodeCandidate` actually has verdicts to consult. */
export function shareCodeCandidates(text: string): string[] {
	const matches = text.match(TOKEN_RE);
	if (!matches) return [];
	const out: string[] = [];
	for (const raw of matches) {
		if (raw.length <= MAX_CODE_LEN) {
			if (isShareCodeCandidate(raw)) out.push(raw);
			continue;
		}
		for (let len = MAX_CODE_LEN; len >= MIN_CODE_LEN; len--) {
			const slice = raw.slice(0, len);
			if (isShareCodeCandidate(slice)) out.push(slice);
		}
	}
	return out;
}
