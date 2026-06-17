import { describe, expect, it } from 'vitest';
import type { CachedPeer } from './types.js';

function makePeer(npub: string, local_tags: string[]): CachedPeer {
	return {
		npub,
		browse_key_hex: undefined,
		petname: undefined,
		profile: undefined,
		collections: [],
		online: false,
		last_fetched: '2026-01-01T00:00:00Z',
		local_tags,
	};
}

// Mirrors the buggy handleRefresh in contacts/+page.svelte (pre-fix)
function refreshBuggy(contacts: CachedPeer[], npub: string, updated: CachedPeer): CachedPeer[] {
	return contacts.map(c => c.npub === npub ? updated : c);
}

// Mirrors the fixed handleRefresh
function refreshFixed(contacts: CachedPeer[], npub: string, updated: CachedPeer): CachedPeer[] {
	return contacts.map(c => c.npub === npub ? { ...updated, local_tags: c.local_tags } : c);
}

describe('contact handleRefresh + tag filter', () => {
	it('buggy: refresh drops local_tags', () => {
		const original = makePeer('hb1_alice', ['anime', 'books']);
		const fromRelay = makePeer('hb1_alice', []); // relay returns empty local_tags
		const result = refreshBuggy([original], 'hb1_alice', fromRelay);
		expect(result[0].local_tags).toEqual([]); // tags silently wiped
	});

	it('buggy: contact disappears from tag-filtered list after refresh', () => {
		const original = makePeer('hb1_alice', ['anime']);
		const fromRelay = makePeer('hb1_alice', []);
		const contacts = refreshBuggy([original], 'hb1_alice', fromRelay);
		const filtered = contacts.filter(c => c.local_tags?.includes('anime'));
		expect(filtered).toHaveLength(0); // contact lost from filtered view
	});

	it('fixed: refresh preserves local_tags', () => {
		const original = makePeer('hb1_alice', ['anime', 'books']);
		const fromRelay = makePeer('hb1_alice', []);
		const result = refreshFixed([original], 'hb1_alice', fromRelay);
		expect(result[0].local_tags).toEqual(['anime', 'books']);
	});

	it('fixed: contact stays in tag-filtered list after refresh', () => {
		const original = makePeer('hb1_alice', ['anime']);
		const fromRelay = makePeer('hb1_alice', []);
		const contacts = refreshFixed([original], 'hb1_alice', fromRelay);
		const filtered = contacts.filter(c => c.local_tags?.includes('anime'));
		expect(filtered).toHaveLength(1);
		expect(filtered[0].npub).toBe('hb1_alice');
	});

	it('fixed: other contacts are unaffected', () => {
		const alice = makePeer('hb1_alice', ['anime']);
		const bob = makePeer('hb1_bob', ['books']);
		const fromRelay = makePeer('hb1_alice', []);
		const result = refreshFixed([alice, bob], 'hb1_alice', fromRelay);
		expect(result[1].local_tags).toEqual(['books']); // bob untouched
	});
});
