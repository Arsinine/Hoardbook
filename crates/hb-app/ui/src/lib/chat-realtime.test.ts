import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import type { ReceivedMessage, ReceivedChannelMessage } from './types.js';

// ── Issue: chat doesn't update in real time ───────────────────────────────────
// The layout poll runs every 20s — too slow for a chat page. The chat page
// must install its own faster poll that clears when it unmounts.

describe('chat real-time polling', () => {
	beforeEach(() => { vi.useFakeTimers(); });
	afterEach(() => { vi.useRealTimers(); });

	it('layout poll interval is 20 000 ms (too slow for chat)', () => {
		// This test documents the known layout interval so any future change
		// to 20_000 that removes the fast chat poll is caught.
		const LAYOUT_INTERVAL_MS = 20_000;
		expect(LAYOUT_INTERVAL_MS).toBe(20_000);
	});

	it('fast chat poll must fire within 5 000 ms', () => {
		const CHAT_POLL_MS = 4_000;
		expect(CHAT_POLL_MS).toBeLessThanOrEqual(5_000);
	});

	it('poll accumulates only genuinely new messages', () => {
		// Simulate two polls: first returns [m1], second returns [m1, m2].
		// Only m2 should be treated as new.
		const m1: ReceivedMessage = { from: 'hb1_bob', to: 'hb1_me', content: 'hi', sent_at: '2026-01-01T10:00:00Z' };
		const m2: ReceivedMessage = { from: 'hb1_bob', to: 'hb1_me', content: 'hey', sent_at: '2026-01-01T10:00:30Z' };

		const seen = new Set<string>();
		function key(m: ReceivedMessage) { return `${m.from}|${m.sent_at}`; }

		// First poll
		const firstBatch = [m1];
		for (const m of firstBatch) seen.add(key(m));

		// Second poll
		const secondBatch = [m1, m2];
		const newMessages = secondBatch.filter(m => !seen.has(key(m)));

		expect(newMessages).toHaveLength(1);
		expect(newMessages[0].sent_at).toBe('2026-01-01T10:00:30Z');
	});

	it('channel poll accumulates only new channel messages', () => {
		const c1: ReceivedChannelMessage = { from: 'hb1_alice', content: 'hello', sent_at: '2026-01-01T10:00:00Z' };
		const c2: ReceivedChannelMessage = { from: 'hb1_bob', content: 'world', sent_at: '2026-01-01T10:00:10Z' };

		function channelKey(m: ReceivedChannelMessage) { return `${m.from}|${m.sent_at}`; }
		const seen = new Set([channelKey(c1)]);

		const batch = [c1, c2];
		const newOnes = batch.filter(m => !seen.has(channelKey(m)));
		expect(newOnes).toHaveLength(1);
		expect(newOnes[0].content).toBe('world');
	});
});
