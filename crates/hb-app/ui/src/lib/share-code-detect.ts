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

/** Upper bound on the MESSAGE TEXT this module will scan (security audit #22). A received DM's content
 *  is peer-controlled with no upstream content-length cap (the Rust DM cache caps message COUNT), so a
 *  hostile ~65 KB paste would otherwise reach the scan and the over-long-token slice recovery would
 *  multiply it ~63× into tens of thousands of candidates — each one an awaited validate IPC call in the
 *  chat route. 8,192 = 2× the 4,096-char acceptance message; a real code is ≤120 chars in prose, so no
 *  legitimate text is lost. Over-long text is REJECTED (detects nothing, never throws, never truncated —
 *  truncation could manufacture a code-shaped prefix an attacker positions at the cut). */
const MAX_TEXT_LEN = 8192;

/** Upper bound on the TOTAL candidate strings ONE message may yield (security audit #22, residual
 *  half). MAX_TEXT_LEN alone does not close the finding: an 8,192-char message still fits ~39
 *  over-long bech32 tokens (each `npub1` + 200 chars + separator = 205), and every over-long token
 *  runs the slice-recovery loop 63× (slices 120→58, each passing the cheap gate) — ~2,500
 *  candidates, each an awaited `validate_share_code` IPC round-trip in the chat route. A real
 *  message carries ONE share code (spec W3: first VALID candidate per message gets the card), so a
 *  small cap costs nothing legitimate; 16 is the finding's own suggested figure and still leaves
 *  room for a prose message with several codes. Hitting the cap DEGRADES BY REJECTING, never by
 *  truncating into a guess — candidates past the cap are simply not considered (no card renders),
 *  matching MAX_TEXT_LEN's stance: a cap that instead kept scanning with re-cut slices could
 *  manufacture a code-shaped prefix an attacker positions at the cut. Accepted cost: an over-long
 *  token whose valid slice sits deeper than the cap (e.g. two abutting codes whose back code is
 *  the checksum-valid one) no longer recovers — it renders as plain text. */
const MAX_CANDIDATES = 16;

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
	// Walk candidates in order of appearance; return the first one the caller's checksum validation
	// accepts. A later invalid candidate never shadows an earlier valid one, and an earlier INVALID
	// candidate is skipped (so "first VALID" holds, not "first"). The walk yields only strings that
	// passed the cheap gate, so `validate` is never handed noise.
	for (const raw of boundedCandidates(text)) {
		if (validate(raw)) return raw;
	}
	return null;
}

/** Every candidate string `extractShareCodeCandidate` may checksum-test for `text`, in test order,
 *  capped at MAX_CANDIDATES. The route pre-validates exactly these (raw tokens AND the
 *  over-long-token prefix slices) so the slice-recovery path in `extractShareCodeCandidate` actually
 *  has verdicts to consult — the two exports share one walk, so that set cannot drift. */
export function shareCodeCandidates(text: string): string[] {
	return [...boundedCandidates(text)];
}

/** The bounded candidate walk both exported scanners run (single-sourced — the route pre-validates
 *  `shareCodeCandidates`' output and `extractShareCodeCandidate` consults those verdicts, so a
 *  divergent pair would have the recovery path consulting verdicts that were never computed).
 *  Yields only strings that passed the cheap length+charset gate, in the order the scanners test
 *  them: in-window raw tokens as matched; an over-long token's plausible-length prefix slices,
 *  longest first (trim trailing bech32 chars until the slice is within the window — a token
 *  greedily consumed past a code's real end, e.g. two codes run together, is trimmed back before
 *  the checksum decides). Over-long text yields NOTHING (audit #22: reject, never truncate), and
 *  the walk STOPS at MAX_CANDIDATES — later candidates are dropped entire, not re-cut to fit. */
function* boundedCandidates(text: string): Generator<string> {
	if (text.length > MAX_TEXT_LEN) return; // audit #22: reject, never truncate
	const matches = text.match(TOKEN_RE);
	if (!matches) return;
	let count = 0;
	for (const raw of matches) {
		if (raw.length <= MAX_CODE_LEN) {
			if (isShareCodeCandidate(raw)) {
				yield raw;
				if (++count >= MAX_CANDIDATES) return;
			}
			continue;
		}
		// Over-long greedy token: try the plausible-length prefixes (rare; mainly two abutting codes).
		for (let len = MAX_CODE_LEN; len >= MIN_CODE_LEN; len--) {
			const slice = raw.slice(0, len);
			if (isShareCodeCandidate(slice)) {
				yield slice;
				if (++count >= MAX_CANDIDATES) return;
			}
		}
	}
}
