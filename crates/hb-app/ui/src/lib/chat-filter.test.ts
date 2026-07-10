import { describe, expect, it } from 'vitest';
import { filterConversations, filterTopics, composeRecipientKind, isComposeToSelf } from './chat-filter.js';
import type { CachedPeer, TopicView } from './types.js';

function makePeer(npub: string): CachedPeer {
	return {
		npub,
		browse_key_hex: undefined,
		petname: undefined,
		profile: undefined,
		collections: [],
		online: false,
		last_fetched: '',
		local_tags: [],
	};
}

function makeTopic(name: string): TopicView {
	return { topic_id: name, name, description: '', tags: [], private: false, joined_at: 0 };
}

describe('chat-filter view-model', () => {
	it('empty query returns every conversation unchanged (identity)', () => {
		const peers = [makePeer('npub1a'), makePeer('npub1b')];
		expect(filterConversations(peers, '', () => 'x')).toEqual(peers);
		expect(filterConversations(peers, '   ', () => 'x')).toEqual(peers);
	});

	it('matches the resolved display name case-insensitively', () => {
		const peers = [makePeer('npub1alice'), makePeer('npub1bob')];
		const names: Record<string, string> = { npub1alice: 'AliceHoarder', npub1bob: 'BobArchivist' };
		const result = filterConversations(peers, 'ALICE', (n) => names[n]);
		expect(result.map((p) => p.npub)).toEqual(['npub1alice']);
	});

	it('matches an npub prefix when no name matches', () => {
		const peers = [makePeer('npub1deadbeef'), makePeer('npub1other')];
		const result = filterConversations(peers, 'deadbeef', () => 'Unknown');
		expect(result.map((p) => p.npub)).toEqual(['npub1deadbeef']);
	});

	it('no match yields an empty array, not the full list', () => {
		const peers = [makePeer('npub1a')];
		expect(filterConversations(peers, 'zzz', () => 'Unrelated')).toEqual([]);
	});

	it('empty topic query returns every topic unchanged', () => {
		const topics = [makeTopic('video/anime'), makeTopic('audio/vinyl')];
		expect(filterTopics(topics, '')).toEqual(topics);
	});

	it('filters topics case-insensitively by name', () => {
		const topics = [makeTopic('video/anime'), makeTopic('audio/vinyl')];
		expect(filterTopics(topics, 'ANIME').map((t) => t.topic_id)).toEqual(['video/anime']);
	});

	it('filters topics by description', () => {
		const topics = [
			{ ...makeTopic('video/anime'), description: '90s VHS rips' },
			{ ...makeTopic('audio/vinyl'), description: 'lossless FLAC' },
		];
		expect(filterTopics(topics, 'flac').map((t) => t.topic_id)).toEqual(['audio/vinyl']);
	});

	it('composeRecipientKind recognises a bare npub', () => {
		expect(composeRecipientKind('npub1abc')).toBe('npub');
	});

	it('composeRecipientKind recognises a full hbk share code', () => {
		expect(composeRecipientKind('hbk1abc')).toBe('sharecode');
	});

	it('composeRecipientKind rejects garbage — the backend re-validates authoritatively', () => {
		expect(composeRecipientKind('not-a-key')).toBe('invalid');
		expect(composeRecipientKind('')).toBe('invalid');
	});

	it('composeRecipientKind trims surrounding whitespace before the prefix check', () => {
		expect(composeRecipientKind('  npub1abc  ')).toBe('npub');
	});

	it('isComposeToSelf recognises your own npub or share code (devtest #14)', () => {
		expect(isComposeToSelf('npub1me', 'npub1me', 'hbk1memine')).toBe(true);
		expect(isComposeToSelf('  npub1me  ', 'npub1me', 'hbk1memine')).toBe(true); // trims whitespace
		expect(isComposeToSelf('hbk1memine', 'npub1me', 'hbk1memine')).toBe(true);
		expect(isComposeToSelf('npub1someoneelse', 'npub1me', 'hbk1memine')).toBe(false);
		expect(isComposeToSelf('', 'npub1me', 'hbk1memine')).toBe(false);
	});
});
