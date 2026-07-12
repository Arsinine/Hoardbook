import { describe, it, expect } from 'vitest';
import { peerPreview, peersWithHistory, relativeTime } from './chat-preview.js';
import type { ReceivedMessage } from './types.js';

const msg = (from: string, to: string, content: string, sent_at: string): ReceivedMessage => ({ from, to, content, sent_at });

describe('chat-preview — peerPreview', () => {
	it('picks the newest across inbox+sent and prefixes outgoing with "You: "', () => {
		const inbox = [msg('them', 'me', 'hi', '2026-07-12T10:00:00Z')];
		const sent = [msg('me', 'them', 'later reply', '2026-07-12T11:00:00Z')];
		const p = peerPreview(inbox, sent, 'them');
		expect(p).toEqual({ text: 'You: later reply', time: '2026-07-12T11:00:00Z', outgoing: true });
	});

	it('incoming latest has no prefix', () => {
		const inbox = [msg('them', 'me', 'newest incoming', '2026-07-12T12:00:00Z')];
		const sent = [msg('me', 'them', 'older', '2026-07-12T09:00:00Z')];
		expect(peerPreview(inbox, sent, 'them')?.text).toBe('newest incoming');
	});

	it('truncates to 48 chars and collapses newlines to spaces', () => {
		const long = 'line one\nline two that keeps going and going well past the limit for sure';
		const p = peerPreview([msg('them', 'me', long, '2026-07-12T10:00:00Z')], [], 'them');
		expect(p?.text).toBe('line one line two that keeps going and going wel…');
		expect(p?.text.length).toBe(49); // 48 + ellipsis
	});

	it('is null when there is no history with the peer', () => {
		expect(peerPreview([msg('other', 'me', 'x', '2026-07-12T10:00:00Z')], [], 'them')).toBeNull();
	});
});

describe('chat-preview — peersWithHistory', () => {
	it('collects both senders and recipients', () => {
		const inbox = [msg('a', 'me', 'x', '2026-07-12T10:00:00Z')];
		const sent = [msg('me', 'b', 'y', '2026-07-12T10:00:00Z')];
		expect([...peersWithHistory(inbox, sent)].sort()).toEqual(['a', 'b']);
	});
});

describe('chat-preview — relativeTime', () => {
	const now = new Date('2026-07-12T12:00:00Z');
	it('buckets by recency', () => {
		expect(relativeTime('2026-07-12T11:59:30Z', now)).toBe('now');
		expect(relativeTime('2026-07-12T11:58:00Z', now)).toBe('2m');
		expect(relativeTime('2026-07-12T09:00:00Z', now)).toBe('3h');
		// > 7 days → "Mon D"
		expect(relativeTime('2026-06-20T09:00:00Z', now)).toMatch(/[A-Z][a-z]{2}\s\d+/);
	});
});
