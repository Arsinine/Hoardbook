// M17 W3 — share-code detection (the consume leg's pure front). The helper under test scans a chat
// message for a bech32 share-code candidate and gates it through a caller-supplied `validate`
// (standing in for the LOCAL `validate_share_code` Tauri command — zero network). These assertions
// pin the spec rules:
//   - first VALID candidate per message gets the card (not first candidate);
//   - invalid-checksum lookalikes stay text (return null);
//   - short fragments and non-bech32 noise never reach validation;
//   - the card is an addendum — the helper never mutates the message text.

import { describe, it, expect } from 'vitest';
import { isShareCodeCandidate, extractShareCodeCandidate, shareCodeCandidates } from './share-code-detect.js';

// Two golden npubs from fingerprint_vectors.json (real bech32, valid checksum) — 63 chars each.
const GOLDEN_NPUB_A = 'npub10xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqpkge6d';
const GOLDEN_NPUB_B = 'npub1ccz8l9zpa47k6vz9gphftsrumpw80rjt3nhnefat4symjhrsnmjs38mnyd';
// A stand-in full hbk code: prefix + 86 bech32 chars (a real hbk1 is 90 chars total).
const GOLDEN_HBK = 'hbk1' + '0'.repeat(86);

describe('isShareCodeCandidate — cheap length+charset pre-checksum gate', () => {
	it('accepts a bare npub1 of valid length', () => {
		expect(isShareCodeCandidate(GOLDEN_NPUB_A)).toBe(true);
	});
	it('accepts a full hbk1 of valid length', () => {
		expect(isShareCodeCandidate(GOLDEN_HBK)).toBe(true);
	});
	it('rejects a short fragment (stray "npub1" in prose)', () => {
		expect(isShareCodeCandidate('npub1')).toBe(false);
		expect(isShareCodeCandidate('npub1abc')).toBe(false);
	});
	it('rejects a non-prefixed string', () => {
		expect(isShareCodeCandidate('not a code at all, just prose')).toBe(false);
		expect(isShareCodeCandidate('1' + 'a'.repeat(60))).toBe(false);
	});
	it('rejects an over-long token', () => {
		expect(isShareCodeCandidate('npub1' + 'a'.repeat(200))).toBe(false);
	});
	it('rejects upper-case / non-bech32 chars in the data part', () => {
		// bech32 data is lower-case + 0-9; a capitalised "NPUB1…" is not a valid token shape.
		expect(isShareCodeCandidate('NPUB1' + 'A'.repeat(58))).toBe(false);
	});
});

describe('extractShareCodeCandidate — first VALID candidate per message', () => {
	// The `validate` callback is the seam for the LOCAL Tauri `validate_share_code` — pure, no network.
	const alwaysValid = (_: string) => true;
	const neverValid = (_: string) => false;
	const onlyGoldenA = (c: string) => c === GOLDEN_NPUB_A;

	it('returns the code when the message contains one valid candidate', () => {
		const msg = `Here's my code: ${GOLDEN_NPUB_A} — ping me!`;
		expect(extractShareCodeCandidate(msg, alwaysValid)).toBe(GOLDEN_NPUB_A);
	});
	it('returns null when there is no candidate (plain prose stays plain text)', () => {
		expect(extractShareCodeCandidate('hey did you still have those concerts?', alwaysValid)).toBeNull();
		expect(extractShareCodeCandidate('', alwaysValid)).toBeNull();
	});
	it('returns null when the candidate fails checksum validation (lookalike stays text)', () => {
		// `neverValid` simulates a checksum-invalid lookalike: the token shape passes the cheap gate
		// but validate_share_code returns false, so no card renders.
		const msg = `my code is ${GOLDEN_NPUB_A}`;
		expect(extractShareCodeCandidate(msg, neverValid)).toBeNull();
	});
	it('returns the FIRST VALID candidate when several candidates appear (spec: first valid per message)', () => {
		// An earlier INVALID candidate does not shadow a later VALID one — "first VALID", not "first".
		const msg = `two codes: ${GOLDEN_NPUB_B} then ${GOLDEN_NPUB_A}`;
		expect(extractShareCodeCandidate(msg, onlyGoldenA)).toBe(GOLDEN_NPUB_A);
	});
	it('returns the first candidate when both are valid (first-wins, no clutter)', () => {
		// Spec W3 / Owner decision #2: a message with multiple valid codes renders ONE card — the
		// first valid candidate. (The message text always renders verbatim above it.)
		const msg = `${GOLDEN_NPUB_A} and also ${GOLDEN_NPUB_B}`;
		expect(extractShareCodeCandidate(msg, alwaysValid)).toBe(GOLDEN_NPUB_A);
	});
	it('captures a code surrounded by punctuation (greedy charset stops at non-bech32)', () => {
		// The token regex is greedy on bech32 charset only — a trailing comma/paren/space ends it.
		expect(extractShareCodeCandidate(`(${GOLDEN_NPUB_A})`, alwaysValid)).toBe(GOLDEN_NPUB_A);
		expect(extractShareCodeCandidate(`>${GOLDEN_NPUB_A}<`, alwaysValid)).toBe(GOLDEN_NPUB_A);
	});
	it('a code preceded by a non-bech32 letter still resolves (the regex is unanchored)', () => {
		// "Xhbk1…" — the regex finds hbk1 starting at index 1 (it is not anchored to a word boundary).
		// This is fine: the checksum gate is the real guard, and a real code embedded in prose is
		// surrounded by whitespace/punctuation, not mashed into a word. If "Xhbk1<valid>" were typed,
		// the card would render — the user sees a valid code. The spec rule ("surrounded by
		// whitespace/punctuation") is a prose observation, not a security boundary.
		expect(extractShareCodeCandidate('X' + GOLDEN_HBK, alwaysValid)).toBe(GOLDEN_HBK);
	});
	it('handles a 4096-char message with three codes (one card + full verbatim text)', () => {
		// Acceptance criterion: a 4096-char message carrying three codes yields exactly one candidate.
		const padding = 'x'.repeat(1000);
		const msg = `${padding} ${GOLDEN_NPUB_A} ${padding} ${GOLDEN_NPUB_B} ${padding} ${GOLDEN_HBK} ${padding}`;
		expect(msg.length).toBeGreaterThan(4096);
		expect(extractShareCodeCandidate(msg, alwaysValid)).toBe(GOLDEN_NPUB_A);
	});
	it('validate is never called with a candidate that fails the cheap gate (short fragments skipped)', () => {
		// Structural guard: the cheap gate runs before validate, so validate is never handed noise.
		const seen: string[] = [];
		const validate = (c: string) => { seen.push(c); return false; };
		extractShareCodeCandidate('npub1 short fragment and ' + GOLDEN_NPUB_A, validate);
		// Only the length-windowed candidate reached validate; the short "npub1 short…" did not.
		expect(seen).toEqual([GOLDEN_NPUB_A]);
	});
});

describe('security audit #22 residual — the candidate cap (bounded set, both scanners agree)', () => {
		const alwaysValid = (_: string) => true;
		// MAX_TEXT_LEN alone does not close #22: an 8,192-char message still fits ~39 over-long
		// bech32 tokens and each emits 63 slices — ~2,500 awaited validate IPC calls in the route.
		// These pin the residual fix: a hard cap on total candidates per message, enforced on the
		// ONE walk both scanners share, degrading by REJECTION (later candidates dropped entire,
		// no card) — never by truncating a token into a code-shaped guess.
		const hostileTokens = (n: number, tokenLen: number) =>
			('npub1' + 'a'.repeat(tokenLen - 4) + ' ').repeat(n);

		it('shareCodeCandidates caps total candidates for adversarial many-token input', () => {
			// 20 over-long tokens × 63 slices each = 1,260 unbounded candidates, under MAX_TEXT_LEN.
			const hostile = hostileTokens(20, 200);
			expect(hostile.length).toBeLessThanOrEqual(8192);
			const candidates = shareCodeCandidates(hostile);
			expect(candidates.length).toBe(16);
			// Every returned candidate is a whole, in-window slice — never a re-cut fragment.
			for (const c of candidates) {
				expect(c.length).toBeGreaterThanOrEqual(58);
				expect(c.length).toBeLessThanOrEqual(120);
				expect(c.startsWith('npub1')).toBe(true);
			}
		});

		it('extractShareCodeCandidate checksum-tests EXACTLY the set shareCodeCandidates returned', () => {
			// The route pre-validates `shareCodeCandidates(text)` and extract consults those verdicts —
			// a candidate tested by one and not the other is the drift fault shape (verdicts consulted
			// that were never computed, or computed and never consulted). Pin the equivalence on the
			// adversarial input.
			const hostile = hostileTokens(20, 200);
			const expected = shareCodeCandidates(hostile);
			const seen: string[] = [];
			const validate = (c: string) => { seen.push(c); return false; };
			expect(extractShareCodeCandidate(hostile, validate)).toBeNull();
			expect(seen).toEqual(expected); // same strings, same order, same cap
		});

		it('validate is called at most MAX_CANDIDATES times for a single message (the IPC bound)', () => {
			const hostile = hostileTokens(20, 200);
			const seen: string[] = [];
			const validate = (c: string) => { seen.push(c); return false; };
			extractShareCodeCandidate(hostile, validate);
			expect(seen.length).toBe(16);
		});

		it('a second over-long token past the cap is dropped ENTIRE, not sliced to fit the cap', () => {
			// Reject-don't-truncate at token granularity: token 1 (63 slices) fills the cap at slice
			// 16; token 2 contributes nothing. A hypothetical "keep scanning but re-cut" design would
			// manufacture a shorter code-shaped prefix of token 2 — this pins that it does not.
			const token1 = 'npub1' + 'a'.repeat(196); // 200 chars > MAX_CODE_LEN → 63 slices
			const token2 = 'npub1' + 'q'.repeat(196);
			const msg = `${token1} ${token2}`;
			const candidates = shareCodeCandidates(msg);
			expect(candidates.length).toBe(16);
			for (const c of candidates) expect(c.startsWith('npub1a')).toBe(true); // all from token1
		});

		it('the happy path is untouched: one real code in prose still renders the card', () => {
			const msg = `Here's my code: ${GOLDEN_NPUB_A} — ping me!`;
			expect(extractShareCodeCandidate(msg, alwaysValid)).toBe(GOLDEN_NPUB_A);
			expect(shareCodeCandidates(msg)).toEqual([GOLDEN_NPUB_A]);
		});
});

describe('shareCodeCandidates — over-long token slice recovery (two codes, no separator)', () => {
	// Fix #7: two valid codes pasted with no separator are greedily consumed as ONE over-long token.
	// `extractShareCodeCandidate` trims it back to a plausible-length prefix slice, but the route only
	// pre-validates the RAW tokens — so the slice looks up absent/false and no card renders. The fix
	// single-sources the candidate list: the route validates `shareCodeCandidates(text)` (raw tokens
	// AND the prefix slices), so the recovery path has a verdict to consult.
	it('includes an in-window prefix slice for an over-long token (two codes run together)', () => {
		const twoCodes = GOLDEN_NPUB_A + GOLDEN_NPUB_B; // 63 + 63 = 126 chars (> MAX_CODE_LEN=120)
		expect(twoCodes.length).toBeGreaterThan(120);
		const candidates = shareCodeCandidates(twoCodes);
		// The over-long token is not itself a candidate (> MAX); instead each plausible-length prefix
		// slice (120..58) that passes the charset gate appears. Assert at least one in-window slice is
		// present (the recovery path's inputs).
		expect(candidates.length).toBeGreaterThan(0);
		for (const c of candidates) {
			expect(c.length).toBeGreaterThanOrEqual(58);
			expect(c.length).toBeLessThanOrEqual(120);
			expect(c.startsWith('npub1')).toBe(true);
		}
	});

	it('extractShareCodeCandidate returns the first slice that validates (route pre-validates slices)', () => {
		// Build the >120-char over-long token; the first valid-looking prefix slice (validated by the
		// caller) is the code that renders. Before fix #7, the route validated only the raw over-long
		// token (absent/false) and no card rendered.
		const twoCodes = GOLDEN_NPUB_A + GOLDEN_NPUB_B;
		// The 120-char prefix of (npubA + npubB) — accept exactly that slice as the "valid" one.
		const validSlice = twoCodes.slice(0, 120);
		expect(validSlice.length).toBe(120);
		const validate = (c: string) => c === validSlice;
		expect(extractShareCodeCandidate(twoCodes, validate)).toBe(validSlice);
	});
});

describe('security audit #22 — the scan is bounded (attacker-length DM text rejected, never truncated)', () => {
	// A received DM's content is peer-controlled and can reach ~65 KB (the NIP-44 plaintext cap is the
	// ONLY upstream bound — the Rust DM cache caps message COUNT, not content length). The over-long-token
	// slice recovery multiplies such text ~63×: each >120-char token emits 63 candidate slices, and the
	// chat route awaits one validate_share_code IPC call per candidate. Unbounded, 65 KB of hostile text
	// becomes ~34k awaited validations. The bound: text longer than 8,192 chars (2× the 4,096-char
	// acceptance message; a real code is ≤120 chars in prose) is REJECTED — detect nothing, never throw.
	it('rejects an over-long hostile message: no candidates, validate never called, no throw', () => {
		// 200 run-together over-long tokens (each `npub1` + 116 a's + separator = 122 chars).
		const hostile = ('npub1' + 'a'.repeat(116) + ' ').repeat(200);
		expect(hostile.length).toBeGreaterThan(8192);
		const seen: string[] = [];
		const validate = (c: string) => { seen.push(c); return true; };
		expect(extractShareCodeCandidate(hostile, validate)).toBeNull();
		expect(shareCodeCandidates(hostile)).toEqual([]);
		expect(seen).toEqual([]);
	});
	it('a message exactly at the bound still detects (the gate is >, and rejects rather than truncates)', () => {
		// 8,192 chars whose tail carries a real code — under/at the cap, behaviour is unchanged. A code
		// near the start is NOT preferred over one near the end: no truncation, whole-text scan.
		const prose = 'x'.repeat(8192 - GOLDEN_NPUB_A.length - 1);
		const msg = prose + ' ' + GOLDEN_NPUB_A;
		expect(msg.length).toBe(8192);
		expect(extractShareCodeCandidate(msg, () => true)).toBe(GOLDEN_NPUB_A);
		expect(shareCodeCandidates(msg)).toEqual([GOLDEN_NPUB_A]);
	});
});
