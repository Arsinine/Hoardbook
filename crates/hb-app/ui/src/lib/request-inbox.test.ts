import { describe, expect, it } from 'vitest';
import {
	requestBadge,
	sortRequests,
	requestPreview,
	canReply,
	REQUEST_EXPLAINER,
	parseManifestRequest,
	manifestRequestHint,
} from './request-inbox.js';
import type { DmRequestView, ReceivedMessage } from './types.js';
import { shortNpub } from './contact-display.js';

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
		fingerprint: { words: ['alpha', 'bravo', 'charlie', 'delta', 'echo'], colorHex: '#abcdef' },
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
		expect(r.fingerprint).toEqual({ words: ['alpha', 'bravo', 'charlie', 'delta', 'echo'], colorHex: '#abcdef' });
	});

	it('REQUEST_EXPLAINER names both the non-contact status and the accept action', () => {
		expect(REQUEST_EXPLAINER).toContain('not in your contacts');
		expect(REQUEST_EXPLAINER).toContain('Accepting adds the contact');
	});
});

describe('parseManifestRequest / manifestRequestHint (M16 W4 — the "ask by DM" hint)', () => {
	const req = JSON.stringify({ hb: 'manifest_request', slug: 'criterion', fingerprint_seen: 'fp1' });

	it('parses a well-formed manifest request', () => {
		expect(parseManifestRequest(req)).toEqual({ slug: 'criterion', fingerprintSeen: 'fp1' });
	});

	it('carries the optional teaser/mascara fields when present', () => {
		const full = JSON.stringify({
			hb: 'manifest_request',
			slug: 's',
			fingerprint_seen: 'fp',
			teaser_event_id: 'evt',
			mascara_pubkey: 'mp',
		});
		expect(parseManifestRequest(full)).toEqual({
			slug: 's',
			fingerprintSeen: 'fp',
			teaserEventId: 'evt',
			mascaraPubkey: 'mp',
		});
	});

	it('returns null for an ordinary chat message or wrong-tag JSON', () => {
		expect(parseManifestRequest('hey, got the movies?')).toBeNull();
		expect(parseManifestRequest(JSON.stringify({ hb: 'something_else', slug: 'x' }))).toBeNull();
		expect(parseManifestRequest(JSON.stringify({ hb: 'manifest_request' }))).toBeNull(); // no slug
		expect(parseManifestRequest('{ not valid json')).toBeNull();
	});

	it('renders a light human hint, or null for a normal message', () => {
		expect(manifestRequestHint(req)).toBe('Asking for the full list of “criterion”');
		expect(manifestRequestHint('just a normal message')).toBeNull();
	});

	it('the request-row preview shows the hint, not the raw JSON payload', () => {
		const r = makeRequest('npub1a', 1, [req]);
		expect(requestPreview(r)).toBe('Asking for the full list of “criterion”');
	});
});

describe('author_npub (QURATOR-79 carrier 4 — the third-party re-serve ask)', () => {
	// The wire half lives in Rust (chat.rs `build_manifest_request_for_author`): a request CAN name a
	// third-party author, so peer D asks peer C to re-serve a manifest peer A authored from C's cache.
	// This side's job is narrower: the inbox must not DISCARD the field, and the copy must not
	// misrepresent a re-serve ask as a request for the owner's own list.
	const NPUB_A = 'npub1author0000abcdefghij012345678901234567890123456789012345wxyz';
	// The pre-carrier-4 body, byte-shaped as every existing peer emits it (no author key at all).
	const noAuthor = JSON.stringify({ hb: 'manifest_request', slug: 'criterion', fingerprint_seen: 'fp1' });

	it('a request carrying an author parses and exposes it', () => {
		const withAuthor = JSON.stringify({
			hb: 'manifest_request',
			slug: 'criterion',
			fingerprint_seen: 'fp1',
			author_npub: NPUB_A,
		});
		expect(parseManifestRequest(withAuthor)).toEqual({
			slug: 'criterion',
			fingerprintSeen: 'fp1',
			authorNpub: NPUB_A,
		});
	});

	it('a request with NO author parses exactly as before (the case every existing peer sends)', () => {
		// Regression: pre-carrier-4 peers emit no author_npub key at all, and that must keep reading
		// as "the asked peer's own collection" — an author field ABSENT, not empty, not null.
		const parsed = parseManifestRequest(noAuthor);
		expect(parsed).toEqual({ slug: 'criterion', fingerprintSeen: 'fp1' });
		expect(parsed!.authorNpub).toBeUndefined();
	});

	it('an empty-string author normalises to absent, not to a real pin', () => {
		// "Present but blank" must never masquerade as an author: the Rust builder normalises empty to
		// None on the way OUT, and the parse normalises it to undefined on the way IN — so a
		// hand-rolled or corrupted body cannot smuggle a blank pin past either side.
		const blank = JSON.stringify({
			hb: 'manifest_request',
			slug: 'criterion',
			fingerprint_seen: 'fp1',
			author_npub: '',
		});
		const parsed = parseManifestRequest(blank);
		expect(parsed).toEqual({ slug: 'criterion', fingerprintSeen: 'fp1' });
		expect(parsed!.authorNpub).toBeUndefined();
	});

	it('the hint says WHOSE list a re-serve ask is for — it must not read as a request for your own', () => {
		const withAuthor = JSON.stringify({
			hb: 'manifest_request',
			slug: 'criterion',
			fingerprint_seen: 'fp1',
			author_npub: NPUB_A,
		});
		const hint = manifestRequestHint(withAuthor)!;
		// The distinguishing content: it names the re-serve (not an own-collection ask) and carries
		// the author's identity. Pinned as substrings, not a byte-equal string, so rewording the copy
		// stays cheap while the MEANING stays pinned.
		expect(hint).toMatch(/re-serve/i);
		expect(hint).toContain(shortNpub(NPUB_A));
		expect(hint).toContain('criterion');
		// ...and the ordinary ask keeps the exact pre-carrier-4 copy, byte for byte.
		expect(manifestRequestHint(noAuthor)).toBe('Asking for the full list of “criterion”');
	});
});
