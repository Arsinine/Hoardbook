import { describe, expect, it } from 'vitest';
import { latestFromPeer, totalUnread, unreadByPeer } from './unread-view.js';
import type { ReceivedMessage } from './types.js';

function msg(from: string, sentAt: string, to = 'npub1me'): ReceivedMessage {
	return { from, to, content: 'hi', sent_at: sentAt };
}

describe('unread-view — unreadByPeer', () => {
	it('counts a message that arrived after the watermark', () => {
		const inbox = [msg('npub1a', '2026-01-02T00:00:00Z')];
		const watermarks = { npub1a: '2026-01-01T00:00:00Z' };
		expect(unreadByPeer(inbox, watermarks, 'npub1me')).toEqual({ npub1a: 1 });
	});

	it('does not count a message at or before the watermark', () => {
		const inbox = [msg('npub1a', '2026-01-01T00:00:00Z')];
		const watermarks = { npub1a: '2026-01-01T00:00:00Z' };
		expect(unreadByPeer(inbox, watermarks, 'npub1me')).toEqual({});
	});

	it('goes to zero once the watermark advances past the message (reading a conversation)', () => {
		const inbox = [msg('npub1a', '2026-01-02T00:00:00Z')];
		const before = unreadByPeer(inbox, { npub1a: '2026-01-01T00:00:00Z' }, 'npub1me');
		expect(before).toEqual({ npub1a: 1 });
		const after = unreadByPeer(inbox, { npub1a: '2026-01-02T00:00:00Z' }, 'npub1me');
		expect(after).toEqual({});
	});

	it('launching with only already-read messages totals zero', () => {
		const inbox = [
			msg('npub1a', '2026-01-01T00:00:00Z'),
			msg('npub1b', '2026-01-01T00:00:00Z'),
		];
		const watermarks = { npub1a: '2026-01-01T00:00:00Z', npub1b: '2026-01-01T00:00:00Z' };
		expect(totalUnread(unreadByPeer(inbox, watermarks, 'npub1me'))).toBe(0);
	});

	it('never counts my own sent-echo, even with no watermark at all', () => {
		const inbox = [msg('npub1me', '2026-01-02T00:00:00Z')];
		expect(unreadByPeer(inbox, {}, 'npub1me')).toEqual({});
	});

	it('a peer with no watermark counts every message from them', () => {
		const inbox = [msg('npub1a', '2026-01-01T00:00:00Z'), msg('npub1a', '2026-01-02T00:00:00Z')];
		expect(unreadByPeer(inbox, {}, 'npub1me')).toEqual({ npub1a: 2 });
	});
});

describe('unread-view — totalUnread', () => {
	it('sums counts across peers', () => {
		expect(totalUnread({ npub1a: 2, npub1b: 3 })).toBe(5);
	});

	it('is zero for an empty map', () => {
		expect(totalUnread({})).toBe(0);
	});
});

describe('unread-view — latestFromPeer', () => {
	it('returns the max sent_at among messages from that peer', () => {
		const inbox = [
			msg('npub1a', '2026-01-01T00:00:00Z'),
			msg('npub1a', '2026-01-03T00:00:00Z'),
			msg('npub1a', '2026-01-02T00:00:00Z'),
			msg('npub1b', '2026-01-09T00:00:00Z'),
		];
		expect(latestFromPeer(inbox, 'npub1a')).toBe('2026-01-03T00:00:00Z');
	});

	it('is undefined when the peer has no messages', () => {
		expect(latestFromPeer([msg('npub1a', '2026-01-01T00:00:00Z')], 'npub1z')).toBeUndefined();
	});
});
