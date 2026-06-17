import { describe, expect, it } from 'vitest';
import type { CachedPeer } from './types.js';

// ── Helpers ──────────────────────────────────────────────────────────────────

function makePeer(npub: string, display_name?: string): CachedPeer {
	return {
		npub,
		browse_key_hex: undefined,
		petname: undefined,
		profile: display_name ? { display_name, bio: undefined, tags: [], since: undefined,
			est_size: undefined, languages: [], contact_hint: undefined, email: undefined,
			location: undefined, social_links: [], willing_to: [], content_types: [], updated: '' } : undefined,
		collections: [], online: false,
		last_fetched: '', local_tags: [],
	};
}

function shortId(hb_id: string) {
	return hb_id.length > 16 ? hb_id.slice(0, 8) + '…' + hb_id.slice(-4) : hb_id;
}

// ── Issue: senderName resolution ─────────────────────────────────────────────
// Mirrors the senderName function as it was before the fix (contacts only).

function senderNameBuggy(
	hb_id: string,
	myId: string,
	contacts: CachedPeer[],
): string {
	if (hb_id === myId) return 'You';
	const contact = contacts.find(c => c.npub === hb_id);
	if (contact?.profile?.display_name) return contact.profile.display_name;
	return shortId(hb_id);
}

// Mirrors the fixed senderName that also checks the fetched-profile cache.
function senderNameFixed(
	hb_id: string,
	myId: string,
	contacts: CachedPeer[],
	profileCache: Record<string, string>,
): string {
	if (hb_id === myId) return 'You';
	const contact = contacts.find(c => c.npub === hb_id);
	if (contact?.profile?.display_name) return contact.profile.display_name;
	if (profileCache[hb_id]) return profileCache[hb_id];
	return shortId(hb_id);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe('chat senderName — unfollowed user shown as hb_id (bug)', () => {
	const myId = 'hb1_me00000000';
	const contacts = [makePeer('hb1_alice', 'AliceHoarder')];

	it('buggy: known contact resolves correctly', () => {
		expect(senderNameBuggy('hb1_alice', myId, contacts)).toBe('AliceHoarder');
	});

	it('buggy: unfollowed sender falls back to shortened hb_id', () => {
		// This is the bug — the full id "hb1_stranger0000XYZ" is shown
		// shortened but still not a human name.
		const stranger = 'hb1_stranger000ABCD';
		const name = senderNameBuggy(stranger, myId, contacts);
		expect(name).toBe(shortId(stranger));   // not a display name
		expect(name).not.toBe('Stranger Name'); // confirms bug
	});
});

describe('chat senderName — fixed with profile cache', () => {
	const myId = 'hb1_me00000000';
	const contacts = [makePeer('hb1_alice', 'AliceHoarder')];

	it('still resolves contacts normally', () => {
		expect(senderNameFixed('hb1_alice', myId, contacts, {})).toBe('AliceHoarder');
	});

	it('resolves self as "You"', () => {
		expect(senderNameFixed(myId, myId, contacts, {})).toBe('You');
	});

	it('uses fetched profile cache for unfollowed sender', () => {
		const cache = { 'hb1_stranger000ABCD': 'Stranger Name' };
		const name = senderNameFixed('hb1_stranger000ABCD', myId, contacts, cache);
		expect(name).toBe('Stranger Name');
	});

	it('falls back to shortId when cache is empty for unknown sender', () => {
		const name = senderNameFixed('hb1_stranger000ABCD', myId, contacts, {});
		expect(name).toBe(shortId('hb1_stranger000ABCD'));
	});
});
