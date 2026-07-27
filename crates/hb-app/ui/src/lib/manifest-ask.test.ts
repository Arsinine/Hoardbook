// M17 W7.1a — the ask must leave a trace (requester side). Pure-helper coverage mirroring
// manifest-fulfil.test.ts + the announce-cooldown boundary test (topic.rs:1858). The state machine:
//   unasked  ──click + publish ok──▶  asked (cooldown active)
//   asked    ──clock advances 60m──▶  asked (cooldown over)
//   asked    ──re-ask click────────▶  asked (sent_at updated, cooldown restarts)
//   *        ──publish fails───────▶  unasked (record never written — covered Rust-side)
import { describe, expect, it } from 'vitest';
import {
	MANIFEST_ASK_COOLDOWN_SECS,
	MANIFEST_ASKED_LINE,
	MANIFEST_ASK_AGAIN_LABEL,
	MANIFEST_ASK_AGAIN_COOLDOWN_TIP,
	MANIFEST_ASK_FAILED_LINE,
	MANIFEST_OPEN_CHAT_LABEL,
	askCooldownRemaining,
	deriveManifestAskState,
	manifestAskKey,
} from './manifest-ask.js';
import type { ManifestAsk } from './api.js';

const NOW_MS = Date.UTC(2026, 5, 15, 12, 0, 0); // 2026-06-15T12:00:00Z — fixed, deterministic
const NOW = new Date(NOW_MS);
function iso(offsetMs: number): string {
	return new Date(NOW_MS + offsetMs).toISOString();
}
function entry(offsetMs: number, fingerprint = 'fp-1'): ManifestAsk {
	return { sent_at: iso(offsetMs), fingerprint_seen: fingerprint };
}

describe('askCooldownRemaining — mirrors hb-core::announce_cooldown_remaining (topic.rs:847)', () => {
	it('undefined lastSentAt ⇒ 0 (never asked ⇒ no cooldown)', () => {
		expect(askCooldownRemaining(undefined, NOW)).toBe(0);
	});

	it('empty string ⇒ 0 (defensive — treat as never asked)', () => {
		expect(askCooldownRemaining('', NOW)).toBe(0);
	});

	it('malformed sent_at ⇒ 0 (a corrupted file must not blank the block forever)', () => {
		expect(askCooldownRemaining('not-a-date', NOW)).toBe(0);
	});

	it('just sent ⇒ full cooldown remaining', () => {
		expect(askCooldownRemaining(iso(0), NOW)).toBe(MANIFEST_ASK_COOLDOWN_SECS);
	});

	it('1 second inside the window ⇒ COOLDOWN - 1 remaining', () => {
		// sent_at 1s ago ⇒ 1s elapsed ⇒ COOLDOWN - 1 remaining.
		const sentAt = iso(-1000);
		expect(askCooldownRemaining(sentAt, NOW)).toBe(MANIFEST_ASK_COOLDOWN_SECS - 1);
	});

	it('exactly at the window boundary ⇒ 0 (cooldown over)', () => {
		const sentAt = iso(-MANIFEST_ASK_COOLDOWN_SECS * 1000); // exactly 60 min ago
		expect(askCooldownRemaining(sentAt, NOW)).toBe(0);
	});

	it('past the window ⇒ 0 (saturates, never negative)', () => {
		const sentAt = iso(-(MANIFEST_ASK_COOLDOWN_SECS + 100) * 1000); // well past
		expect(askCooldownRemaining(sentAt, NOW)).toBe(0);
	});

	it('clock rollback (now < sent_at) ⇒ full cooldown, never a bypass', () => {
		// Mirrors the announce-cooldown rollback test (topic.rs:1867): a clock that has rolled back
		// reads as MORE throttled, never less. Underflow would be a bypass; we clamp to the full window.
		const future = iso(60_000); // sent_at 1 minute in the future
		expect(askCooldownRemaining(future, NOW)).toBe(MANIFEST_ASK_COOLDOWN_SECS);
	});
});

describe('deriveManifestAskState — table-driven over the asked/unasked axis', () => {
	it('undefined map ⇒ unasked', () => {
		expect(deriveManifestAskState(undefined, 'npub1a', 'criterion', NOW)).toEqual({ kind: 'unasked' });
	});

	it('null map ⇒ unasked', () => {
		expect(deriveManifestAskState(null, 'npub1a', 'criterion', NOW)).toEqual({ kind: 'unasked' });
	});

	it('missing key ⇒ unasked', () => {
		const asks = { 'npub1a|other': entry(0) };
		expect(deriveManifestAskState(asks, 'npub1a', 'criterion', NOW)).toEqual({ kind: 'unasked' });
	});

	it('different npub, same slug ⇒ unasked (keyed by BOTH npub and slug)', () => {
		const asks = { 'npub1a|criterion': entry(0) };
		expect(deriveManifestAskState(asks, 'npub1b', 'criterion', NOW)).toEqual({ kind: 'unasked' });
	});

	it('different slug, same npub ⇒ unasked', () => {
		const asks = { 'npub1a|criterion': entry(0) };
		expect(deriveManifestAskState(asks, 'npub1a', 'other', NOW)).toEqual({ kind: 'unasked' });
	});

	it('malformed sent_at ⇒ unasked (honest default — a corrupted record must not phantom-render "Asked")', () => {
		const asks = { 'npub1a|criterion': { sent_at: 'garbage', fingerprint_seen: 'fp' } };
		expect(deriveManifestAskState(asks, 'npub1a', 'criterion', NOW)).toEqual({ kind: 'unasked' });
	});

	it('just-asked entry ⇒ asked, cooldown active, cooldownOver=false, relative="now"', () => {
		const asks = { 'npub1a|criterion': entry(0) };
		const state = deriveManifestAskState(asks, 'npub1a', 'criterion', NOW);
		expect(state.kind).toBe('asked');
		if (state.kind === 'asked') {
			expect(state.sentAt).toBe(iso(0));
			expect(state.relative).toBe('now');
			expect(state.cooldownRemaining).toBe(MANIFEST_ASK_COOLDOWN_SECS);
			expect(state.cooldownOver).toBe(false);
		}
	});

	it('asked 30 min ago ⇒ asked, half cooldown remaining, relative="30m"', () => {
		const asks = { 'npub1a|criterion': entry(-30 * 60_000) };
		const state = deriveManifestAskState(asks, 'npub1a', 'criterion', NOW);
		expect(state.kind).toBe('asked');
		if (state.kind === 'asked') {
			expect(state.relative).toBe('30m');
			expect(state.cooldownRemaining).toBe(30 * 60);
			expect(state.cooldownOver).toBe(false);
		}
	});

	it('asked exactly 60 min ago ⇒ asked, cooldownOver=true (Ask again enabled)', () => {
		const asks = { 'npub1a|criterion': entry(-60 * 60_000) };
		const state = deriveManifestAskState(asks, 'npub1a', 'criterion', NOW);
		expect(state.kind).toBe('asked');
		if (state.kind === 'asked') {
			expect(state.cooldownRemaining).toBe(0);
			expect(state.cooldownOver).toBe(true);
		}
	});

	it('asked 2 hours ago ⇒ asked, cooldownOver=true, relative ladder advances to "2h"', () => {
		const asks = { 'npub1a|criterion': entry(-2 * 60 * 60_000) };
		const state = deriveManifestAskState(asks, 'npub1a', 'criterion', NOW);
		expect(state.kind).toBe('asked');
		if (state.kind === 'asked') {
			expect(state.relative).toBe('2h');
			expect(state.cooldownOver).toBe(true);
		}
	});

	it('the state is read from the persisted record, not component-local state — survives a remount', () => {
		// Simulate a remount: the map persists, the component is fresh. The same map ⇒ the same state.
		const asks = { 'npub1a|criterion': entry(-5 * 60_000) };
		const first = deriveManifestAskState(asks, 'npub1a', 'criterion', NOW);
		const second = deriveManifestAskState(asks, 'npub1a', 'criterion', NOW);
		expect(second).toEqual(first);
		expect(second.kind).toBe('asked');
	});
});

describe('manifestAskKey — mirrors the Rust manifest_ask_key (store.rs)', () => {
	it('joins npub and slug with a pipe', () => {
		expect(manifestAskKey('npub1a', 'criterion')).toBe('npub1a|criterion');
	});

	it('disambiguates same-slug-different-peer (no clobber)', () => {
		expect(manifestAskKey('npub1a', 'criterion')).not.toBe(manifestAskKey('npub1b', 'criterion'));
	});

	it('disambiguates same-peer-different-slug', () => {
		expect(manifestAskKey('npub1a', 'criterion')).not.toBe(manifestAskKey('npub1a', 'other'));
	});
});

describe('MAS-INV-5 — no new user-facing copy contains "Download"', () => {
	// The invariant sweep (mas-inv5-no-download.test.ts) scans the browse page source. The new copy
	// introduced here MUST NOT trip it. Pin each string at the helper level so a future edit is caught
	// before it reaches the page.
	const FORBIDDEN = /\bdownload\b/i;

	it('MANIFEST_ASKED_LINE never contains "Download"', () => {
		expect(FORBIDDEN.test(MANIFEST_ASKED_LINE('now'))).toBe(false);
		expect(FORBIDDEN.test(MANIFEST_ASKED_LINE('3h'))).toBe(false);
	});

	it('MANIFEST_ASK_AGAIN_LABEL never contains "Download"', () => {
		expect(FORBIDDEN.test(MANIFEST_ASK_AGAIN_LABEL)).toBe(false);
	});

	it('MANIFEST_OPEN_CHAT_LABEL never contains "Download"', () => {
		expect(FORBIDDEN.test(MANIFEST_OPEN_CHAT_LABEL)).toBe(false);
	});

	it('MANIFEST_ASK_AGAIN_COOLDOWN_TIP never contains "Download"', () => {
		expect(FORBIDDEN.test(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(1))).toBe(false);
		expect(FORBIDDEN.test(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(30 * 60))).toBe(false);
		expect(FORBIDDEN.test(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(60 * 60))).toBe(false);
	});

	it('MANIFEST_ASK_FAILED_LINE never contains "Download"', () => {
		expect(FORBIDDEN.test(MANIFEST_ASK_FAILED_LINE('relay unreachable'))).toBe(false);
	});

	it('MANIFEST_ASKED_LINE carries the "waiting for their reply" promise', () => {
		// Positive assertion: the asked-state is honest about what happens next (the reply is a DM; the
		// Open chat link is where it arrives).
		const line = MANIFEST_ASKED_LINE('now').toLowerCase();
		expect(line).toMatch(/waiting for their reply/);
	});

	it('MANIFEST_ASK_FAILED_LINE never says "Asked" — failure is loud, not misrendered as success', () => {
		const line = MANIFEST_ASK_FAILED_LINE('relay unreachable').toLowerCase();
		expect(line).not.toMatch(/\basked\b/);
		// The copy uses a curly apostrophe (U+2019) — match the 'send the request' core, apostrophe-agnostic.
		expect(line).toMatch(/send the request/);
	});
});

describe('MANIFEST_ASK_AGAIN_COOLDOWN_TIP — formats the countdown readably', () => {
	it('< 60 min ⇒ "Ask again in Xm"', () => {
		expect(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(60)).toBe('Ask again in 1m');
		expect(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(30 * 60)).toBe('Ask again in 30m');
	});

	it('exactly 60 min ⇒ "Ask again in 1h"', () => {
		expect(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(60 * 60)).toBe('Ask again in 1h');
	});

	it('90 min ⇒ "Ask again in 1h 30m"', () => {
		expect(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(90 * 60)).toBe('Ask again in 1h 30m');
	});

	it('rounds up (1s remaining ⇒ "1m", not "0m")', () => {
		expect(MANIFEST_ASK_AGAIN_COOLDOWN_TIP(1)).toBe('Ask again in 1m');
	});
});
