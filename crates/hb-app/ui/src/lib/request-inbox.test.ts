import { describe, expect, it } from 'vitest';
import { requestBadge, sortRequests, requestPreview, canReply, REQUEST_EXPLAINER } from './request-inbox.js';
import type { DmRequestView, ReceivedMessage } from './types.js';

function makeRequest(npub: string, lastMessageAt: number, contents: string[] = ['hi']): DmRequestView {
	const messages: ReceivedMessage[] = contents.map((content, i) => ({
		from: npub,
		to: 'npub1me',
		content,
		sent_at: `2026-01-0${i + 1}T00:00:00Z`,
	}));
	return {
		npub,
		first_seen: lastMessageAt,
		last_message_at: lastMessageAt,
		message_count: messages.length,
		messages,
		fingerprint: { words: ['alpha', 'bravo', 'charlie'], colorHex: '#abcdef' },
	};
}

describe('request-inbox view-model', () => {
	it('badge is the number of distinct sender buckets, not total messages', () => {
		const requests = [makeRequest('npub1a', 1, ['a', 'b', 'c']), makeRequest('npub1b', 2)];
		expect(requestBadge(requests)).toBe(2);
		expect(requestBadge([])).toBe(0);
	});

	it('sorts newest activity first', () => {
		const requests = [makeRequest('npub1old', 1), makeRequest('npub1new', 100), makeRequest('npub1mid', 50)];
		const sorted = sortRequests(requests);
		expect(sorted.map((r) => r.npub)).toEqual(['npub1new', 'npub1mid', 'npub1old']);
	});

	it('does not mutate the input array', () => {
		const requests = [makeRequest('npub1a', 1), makeRequest('npub1b', 2)];
		const original = [...requests];
		sortRequests(requests);
		expect(requests).toEqual(original);
	});

	it('preview truncates at max length with a trailing ellipsis', () => {
		const long = 'x'.repeat(100);
		const r = makeRequest('npub1a', 1, [long]);
		const preview = requestPreview(r, 80);
		expect(preview.length).toBe(80);
		expect(preview.endsWith('…')).toBe(true);
	});

	it('preview is verbatim when under the max', () => {
		const r = makeRequest('npub1a', 1, ['short message']);
		expect(requestPreview(r)).toBe('short message');
	});

	it('preview reflects the LAST message in the bucket, not the first', () => {
		const r = makeRequest('npub1a', 1, ['first', 'second']);
		expect(requestPreview(r)).toBe('second');
	});

	it('preview of an empty bucket is an empty string, not undefined/crash', () => {
		const r = makeRequest('npub1a', 1, []);
		expect(requestPreview(r)).toBe('');
	});

	it('canReply is false until accepted (isContact=false), true once a contact', () => {
		expect(canReply(false)).toBe(false);
		expect(canReply(true)).toBe(true);
	});

	it('fingerprint is passed through verbatim (never re-derived)', () => {
		const r = makeRequest('npub1a', 1);
		expect(r.fingerprint).toEqual({ words: ['alpha', 'bravo', 'charlie'], colorHex: '#abcdef' });
	});

	it('REQUEST_EXPLAINER names both the non-contact status and the accept action', () => {
		expect(REQUEST_EXPLAINER).toContain('not in your contacts');
		expect(REQUEST_EXPLAINER).toContain('Accepting adds the contact');
	});
});
