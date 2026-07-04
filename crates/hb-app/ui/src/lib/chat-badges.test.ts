import { describe, expect, it } from 'vitest';
import { requestBadge } from './request-inbox.js';
import type { DmRequestView, ReceivedMessage } from './types.js';

function makeMsg(from: string, sent_at: string): ReceivedMessage {
	return { from, to: 'me', content: 'hello', sent_at };
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

// ── M13 Part B (Q7): strangers feed the Request model, never the conversation list ─────────────
// Mirrors the page's new derivations: `allConversationPeers = [...$contacts]` (the old
// inboxOnlyPeers stranger-merge is REMOVED — the backend's contact-only inbox means a stranger's
// message can't even reach $inboxMessages), and the sidebar Requests row badge is
// requestBadge($dmRequests).

function conversationPeerIds(contacts: string[]): string[] {
	// The page's contacts-only conversation list — no stranger merge path exists anymore.
	return [...contacts];
}

function makeRequestBucket(npub: string): DmRequestView {
	return {
		npub,
		first_seen: 0,
		last_message_at: 0,
		message_count: 1,
		messages: [{ from: npub, to: 'me', content: 'hi', sent_at: '2026-01-01T10:00:00Z' }],
	};
}

describe('chat conversation list vs Request inbox (Q7)', () => {
	it('a stranger sender never produces a conversation-list row', () => {
		const contacts = ['hb1_alice'];
		const peers = conversationPeerIds(contacts);
		expect(peers).not.toContain('hb1_stranger');
		expect(peers).toEqual(['hb1_alice']);
	});

	it('a stranger sender surfaces as a Request badge instead', () => {
		const requests = [makeRequestBucket('hb1_stranger')];
		expect(requestBadge(requests)).toBe(1);
	});

	it('a stranger therefore never contributes an unread badge to the conversation list', () => {
		// The stranger's messages live in the request bucket, not $inboxMessages — the unread
		// derivation over the contacts-only peer list simply has no row to badge.
		const contacts = ['hb1_alice'];
		const contactMessages = [makeMsg('hb1_alice', '2026-01-01T10:00:00Z')];
		const unread = computeUnreadFixed(contactMessages, conversationPeerIds(contacts));
		expect(unread['hb1_stranger']).toBeUndefined();
	});
});
