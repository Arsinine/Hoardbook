import { describe, expect, it } from 'vitest';
import {
	ALPHABET,
	contactSortName,
	sortKeyForPeer,
	letterForPeer,
	groupByLetter,
	groupByGroups,
	onlineBucket,
	matchesQuery,
	presentSectionKeys,
} from './contacts-view.js';
import type { CachedPeer, Collection, Group } from './types.js';

function makePeer(overrides: Partial<CachedPeer> & { npub: string }): CachedPeer {
	return {
		browse_key_hex: undefined,
		petname: undefined,
		profile: undefined,
		collections: [],
		online: false,
		last_fetched: '',
		local_tags: [],
		...overrides,
	};
}

function makeCollection(overrides: Partial<Collection> = {}): Collection {
	return {
		slug: 'films',
		path_alias: 'films',
		item_count: 0,
		total_bytes: 0,
		content_types: [],
		tags: [],
		languages: [],
		last_updated: '',
		listing: [],
		...overrides,
	};
}

function makeGroup(name: string, pubkeys: string[]): Group {
	return { name, pubkeys };
}

describe('contactSortName', () => {
	it('petname wins over display_name', () => {
		const p = makePeer({ npub: 'npub1a', petname: 'Nick', profile: { display_name: 'Real Name' } as any });
		expect(contactSortName(p)).toBe('Nick');
	});

	it('falls back to display_name when no petname', () => {
		const p = makePeer({ npub: 'npub1a', profile: { display_name: 'Real Name' } as any });
		expect(contactSortName(p)).toBe('Real Name');
	});

	it('a whitespace-only petname is skipped, falling to display_name', () => {
		const p = makePeer({ npub: 'npub1a', petname: '   ', profile: { display_name: 'Real Name' } as any });
		expect(contactSortName(p)).toBe('Real Name');
	});

	it('both blank yields null', () => {
		const p = makePeer({ npub: 'npub1a', petname: '  ', profile: { display_name: '  ' } as any });
		expect(contactSortName(p)).toBeNull();
	});

	it('no profile at all yields null', () => {
		const p = makePeer({ npub: 'npub1a' });
		expect(contactSortName(p)).toBeNull();
	});

	it('a literal empty-string display_name (legacy/adversarial teaser) yields null, not ""', () => {
		// R1 only guards publish — a stored/adversarial teaser can still carry display_name: "".
		const p = makePeer({ npub: 'npub1a', petname: '', profile: { display_name: '' } as any });
		expect(contactSortName(p)).toBeNull();
	});
});

describe('sortKeyForPeer', () => {
	it('is case-insensitive', () => {
		const a = makePeer({ npub: 'npub1a', petname: 'alice' });
		const b = makePeer({ npub: 'npub1b', petname: 'Alice' });
		expect(sortKeyForPeer(a)).toBe(sortKeyForPeer(b));
	});

	it('sorts nameless peers after every named peer', () => {
		const named = makePeer({ npub: 'npub1zzz', petname: 'Zed' });
		const nameless = makePeer({ npub: 'npub1aaa' });
		expect(sortKeyForPeer(named) < sortKeyForPeer(nameless)).toBe(true);
	});

	it('sorts nameless peers among themselves by npub', () => {
		const a = makePeer({ npub: 'npub1aaa' });
		const b = makePeer({ npub: 'npub1zzz' });
		expect(sortKeyForPeer(a) < sortKeyForPeer(b)).toBe(true);
	});
});

describe('letterForPeer', () => {
	it('"anna" -> A', () => {
		expect(letterForPeer(makePeer({ npub: 'npub1a', petname: 'anna' }))).toBe('A');
	});

	it('a non-ASCII leading letter falls to #', () => {
		expect(letterForPeer(makePeer({ npub: 'npub1a', petname: 'Örn' }))).toBe('#');
	});

	it('a leading digit falls to #', () => {
		expect(letterForPeer(makePeer({ npub: 'npub1a', petname: '123archive' }))).toBe('#');
	});

	it('a leading underscore falls to #', () => {
		expect(letterForPeer(makePeer({ npub: 'npub1a', petname: '_archive' }))).toBe('#');
	});

	it('a nameless peer falls to #', () => {
		expect(letterForPeer(makePeer({ npub: 'npub1a' }))).toBe('#');
	});
});

describe('groupByLetter', () => {
	it('orders sections A..Z then #, omitting empty letters', () => {
		const peers = [
			makePeer({ npub: 'npub1z', petname: 'Zed' }),
			makePeer({ npub: 'npub1a', petname: 'Anna' }),
			makePeer({ npub: 'npub1none' }),
		];
		const sections = groupByLetter(peers);
		expect(sections.map((s) => s.key)).toEqual(['A', 'Z', '#']);
	});

	it('sorts within a section', () => {
		const peers = [
			makePeer({ npub: 'npub1b', petname: 'Bob' }),
			makePeer({ npub: 'npub1a', petname: 'Bea' }),
		];
		const sections = groupByLetter(peers);
		expect(sections[0].key).toBe('B');
		expect(sections[0].peers.map((p) => p.npub)).toEqual(['npub1a', 'npub1b']);
	});

	it('the # section trails and holds every nameless peer', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Anna' }),
			makePeer({ npub: 'npub1zzz' }),
			makePeer({ npub: 'npub1aaa' }),
		];
		const sections = groupByLetter(peers);
		expect(sections[sections.length - 1].key).toBe('#');
		expect(sections[sections.length - 1].peers.map((p) => p.npub)).toEqual(['npub1aaa', 'npub1zzz']);
	});

	it('anchorIds are sec-<letter> and sec-hash', () => {
		const peers = [makePeer({ npub: 'npub1a', petname: 'Anna' }), makePeer({ npub: 'npub1n' })];
		const sections = groupByLetter(peers);
		expect(sections.find((s) => s.key === 'A')?.anchorId).toBe('sec-A');
		expect(sections.find((s) => s.key === '#')?.anchorId).toBe('sec-hash');
	});

	it('an empty peer list yields no sections', () => {
		expect(groupByLetter([])).toEqual([]);
	});
});

describe('groupByGroups', () => {
	it('a peer in two groups appears in both sections (multi-membership)', () => {
		const peer = makePeer({ npub: 'npub1a', petname: 'Anna' });
		const groups = [makeGroup('Friends', ['npub1a']), makeGroup('Work', ['npub1a'])];
		const sections = groupByGroups([peer], groups);
		expect(sections.map((s) => s.label)).toEqual(['Friends', 'Work']);
		expect(sections[0].peers.map((p) => p.npub)).toEqual(['npub1a']);
		expect(sections[1].peers.map((p) => p.npub)).toEqual(['npub1a']);
	});

	it('Ungrouped trails after every group section', () => {
		const grouped = makePeer({ npub: 'npub1a', petname: 'Anna' });
		const solo = makePeer({ npub: 'npub1b', petname: 'Bob' });
		const groups = [makeGroup('Friends', ['npub1a'])];
		const sections = groupByGroups([grouped, solo], groups);
		expect(sections.map((s) => s.label)).toEqual(['Friends', 'Ungrouped']);
		expect(sections[1].peers.map((p) => p.npub)).toEqual(['npub1b']);
		expect(sections[1].anchorId).toBe('sec-grp-ungrouped');
	});

	it('a group with zero visible members is omitted entirely', () => {
		const peer = makePeer({ npub: 'npub1a', petname: 'Anna' });
		const groups = [makeGroup('Empty', ['npub1nobody']), makeGroup('Friends', ['npub1a'])];
		const sections = groupByGroups([peer], groups);
		expect(sections.map((s) => s.label)).toEqual(['Friends']);
	});

	it('preserves the input group order', () => {
		const a = makePeer({ npub: 'npub1a', petname: 'Anna' });
		const b = makePeer({ npub: 'npub1b', petname: 'Bob' });
		const groups = [makeGroup('Zed', ['npub1b']), makeGroup('Alpha', ['npub1a'])];
		const sections = groupByGroups([a, b], groups);
		expect(sections.map((s) => s.label)).toEqual(['Zed', 'Alpha']);
	});

	it('a peer in no group appears only under Ungrouped', () => {
		const peer = makePeer({ npub: 'npub1a', petname: 'Anna' });
		const sections = groupByGroups([peer], []);
		expect(sections.map((s) => s.label)).toEqual(['Ungrouped']);
	});

	it('group anchors are sec-grp-<index> by input-array index', () => {
		const a = makePeer({ npub: 'npub1a', petname: 'Anna' });
		const groups = [makeGroup('Empty', []), makeGroup('Friends', ['npub1a'])];
		const sections = groupByGroups([a], groups);
		expect(sections[0].anchorId).toBe('sec-grp-1');
	});
});

describe('onlineBucket', () => {
	it('keeps only online peers, sorted', () => {
		const peers = [
			makePeer({ npub: 'npub1b', petname: 'Bob', online: true }),
			makePeer({ npub: 'npub1a', petname: 'Alice', online: true }),
			makePeer({ npub: 'npub1c', petname: 'Carl', online: false }),
		];
		expect(onlineBucket(peers).map((p) => p.npub)).toEqual(['npub1a', 'npub1b']);
	});

	it('is empty when nobody is online', () => {
		const peers = [makePeer({ npub: 'npub1a', online: false })];
		expect(onlineBucket(peers)).toEqual([]);
	});
});

describe('matchesQuery', () => {
	it('an empty or whitespace query matches everything (identity)', () => {
		const p = makePeer({ npub: 'npub1a' });
		expect(matchesQuery(p, '')).toBe(true);
		expect(matchesQuery(p, '   ')).toBe(true);
	});

	it('matches by display_name', () => {
		const p = makePeer({ npub: 'npub1a', profile: { display_name: 'Archivebox' } as any });
		expect(matchesQuery(p, 'archive')).toBe(true);
	});

	it('matches by petname', () => {
		const p = makePeer({ npub: 'npub1a', petname: 'Nickname' });
		expect(matchesQuery(p, 'nick')).toBe(true);
	});

	it('matches by an npub substring', () => {
		const p = makePeer({ npub: 'npub1deadbeef' });
		expect(matchesQuery(p, 'deadbeef')).toBe(true);
	});

	it('matches by bio only', () => {
		const p = makePeer({ npub: 'npub1a', profile: { display_name: 'x', bio: '90s anime VHS rips' } as any });
		expect(matchesQuery(p, 'vhs')).toBe(true);
	});

	it('matches by a local tag only', () => {
		const p = makePeer({ npub: 'npub1a', local_tags: ['trusted-trader'] });
		expect(matchesQuery(p, 'trusted')).toBe(true);
	});

	it('matches by a content type only', () => {
		const p = makePeer({ npub: 'npub1a', profile: { display_name: 'x', content_types: ['video'] } as any });
		expect(matchesQuery(p, 'video')).toBe(true);
	});

	it('matches by a collection path_alias only', () => {
		const p = makePeer({ npub: 'npub1a', collections: [makeCollection({ path_alias: 'anime-vhs' })] });
		expect(matchesQuery(p, 'anime-vhs')).toBe(true);
	});

	it('matches by a collection description only', () => {
		const p = makePeer({ npub: 'npub1a', collections: [makeCollection({ description: 'rare 90s tapes' })] });
		expect(matchesQuery(p, 'rare')).toBe(true);
	});

	it('is case-insensitive', () => {
		const p = makePeer({ npub: 'npub1a', profile: { display_name: 'ArchiveBox' } as any });
		expect(matchesQuery(p, 'ARCHIVEBOX')).toBe(true);
	});

	it('returns false when nothing matches', () => {
		const p = makePeer({ npub: 'npub1a', profile: { display_name: 'Archivebox' } as any });
		expect(matchesQuery(p, 'zzz-no-match')).toBe(false);
	});

	it('an undefined profile never throws', () => {
		const p = makePeer({ npub: 'npub1a' });
		expect(() => matchesQuery(p, 'anything')).not.toThrow();
		expect(matchesQuery(p, 'anything')).toBe(false);
	});
});

describe('presentSectionKeys', () => {
	it('returns exactly the section keys present', () => {
		const peers = [makePeer({ npub: 'npub1a', petname: 'Anna' }), makePeer({ npub: 'npub1z', petname: 'Zed' })];
		const sections = groupByLetter(peers);
		expect(presentSectionKeys(sections)).toEqual(new Set(['A', 'Z']));
	});
});

describe('ALPHABET', () => {
	it('is A..Z then #', () => {
		expect(ALPHABET.length).toBe(27);
		expect(ALPHABET[0]).toBe('A');
		expect(ALPHABET[25]).toBe('Z');
		expect(ALPHABET[26]).toBe('#');
	});
});
