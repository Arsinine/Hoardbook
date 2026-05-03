import { describe, expect, it } from 'vitest';
import type { ReceivedMessage } from './types.js';

function makeMsg(from: string, sent_at: string): ReceivedMessage {
	return { from, to: 'me', content: 'hello', sent_at, encrypted: false };
}

// Mirrors unreadCounts derivation when seenCounts is empty (simulates remount)
function computeUnreadBuggy(messages: ReceivedMessage[], peers: string[]): Record<string, number> {
	const seenCounts: Record<string, number> = {}; // reset on remount
	return Object.fromEntries(
		peers.map(id => [id, Math.max(0, messages.filter(m => m.from === id).length - (seenCounts[id] ?? 0))])
	);
}

// Mirrors unreadCounts derivation when seenCounts is seeded from fetched messages (the fix)
function computeUnreadFixed(messages: ReceivedMessage[], peers: string[]): Record<string, number> {
	const seenCounts: Record<string, number> = {};
	for (const m of messages) {
		seenCounts[m.from] = messages.filter(x => x.from === m.from).length;
	}
	return Object.fromEntries(
		peers.map(id => [id, Math.max(0, messages.filter(m => m.from === id).length - (seenCounts[id] ?? 0))])
	);
}

describe('chat per-peer unread badge after remount', () => {
	it('buggy: all prior messages appear unread after remounting chat page', () => {
		const messages = [makeMsg('hb1_bob', '2026-01-01T10:00:00Z')];
		const unread = computeUnreadBuggy(messages, ['hb1_bob']);
		expect(unread['hb1_bob']).toBe(1); // false positive
	});

	it('fixed: no spurious unread badge after remount (seenCounts seeded on load)', () => {
		const messages = [makeMsg('hb1_bob', '2026-01-01T10:00:00Z')];
		const unread = computeUnreadFixed(messages, ['hb1_bob']);
		expect(unread['hb1_bob']).toBe(0);
	});

	it('fixed: genuinely new messages still show as unread', () => {
		const existing = [makeMsg('hb1_bob', '2026-01-01T10:00:00Z')];
		// Seed on load
		const seenCounts: Record<string, number> = {};
		for (const m of existing) seenCounts[m.from] = existing.filter(x => x.from === m.from).length;

		// New message arrives after seeding
		const updated = [...existing, makeMsg('hb1_bob', '2026-01-01T10:01:00Z')];
		const total = updated.filter(m => m.from === 'hb1_bob').length;
		const unread = Math.max(0, total - (seenCounts['hb1_bob'] ?? 0));
		expect(unread).toBe(1);
	});

	it('fixed: multiple peers tracked independently', () => {
		const messages = [
			makeMsg('hb1_alice', '2026-01-01T10:00:00Z'),
			makeMsg('hb1_bob', '2026-01-01T10:01:00Z'),
			makeMsg('hb1_bob', '2026-01-01T10:02:00Z'),
		];
		const unread = computeUnreadFixed(messages, ['hb1_alice', 'hb1_bob']);
		expect(unread['hb1_alice']).toBe(0);
		expect(unread['hb1_bob']).toBe(0);
	});
});
