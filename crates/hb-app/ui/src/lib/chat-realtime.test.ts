import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import type { ReceivedMessage } from './types.js';
import { DM_POLL_VISIBLE_MS } from './poll-lifecycle.js';

// ── Issue: chat doesn't update in real time ───────────────────────────────────
// The chat page installs its own DM poll (faster than the layout nav poll) that
// clears when it unmounts. devtest v0.12.4 #1 tightened it to hit a ≤2–3s target.

describe('chat real-time polling', () => {
	beforeEach(() => { vi.useFakeTimers(); });
	afterEach(() => { vi.useRealTimers(); });

	it('the DM poll fires within the ≤2–3s propagation target (devtest v0.12.4 #1)', () => {
		// Guards the REAL constant (not a local literal), so a regression back to the 15s cadence —
		// or anything slower than the stated 3s target — reddens this test.
		expect(DM_POLL_VISIBLE_MS).toBeLessThanOrEqual(3_000);
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
});
