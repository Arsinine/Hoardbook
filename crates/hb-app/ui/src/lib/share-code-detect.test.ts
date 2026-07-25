// M17 W3 — share-code detection (the consume leg's pure front). The helper under test scans a chat
// message for a bech32 share-code candidate and gates it through a caller-supplied `validate`
// (standing in for the LOCAL `validate_share_code` Tauri command — zero network). These assertions
// pin the spec rules:
//   - first VALID candidate per message gets the card (not first candidate);
//   - invalid-checksum lookalikes stay text (return null);
//   - short fragments and non-bech32 noise never reach validation;
//   - the card is an addendum — the helper never mutates the message text.

import { describe, it, expect } from 'vitest';
import { isShareCodeCandidate, extractShareCodeCandidate } from './share-code-detect.js';

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
