import { describe, expect, it } from 'vitest';
import { suggestGroupNames } from './group-suggest.js';
import type { CachedPeer, Profile } from './types.js';

function makeProfile(overrides: Partial<Profile> = {}): Profile {
	return {
		display_name: '',
		tags: [],
		languages: [],
		social_links: [],
		willing_to: [],
		content_types: [],
		updated: '',
		...overrides,
	};
}

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

describe('suggestGroupNames', () => {
	it('suggests shared interest tags verbatim, highest-priority source first', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime', 'vinyl'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime', 'books'] }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['anime', 'Marisol & Kestrel']);
	});

	it('falls back to content_types when nobody has hand-written tags', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ content_types: ['video', 'audio'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ content_types: ['video', 'image'] }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['video', 'Marisol & Kestrel']);
	});

	it('mixes tags and content_types, tags first (priority order)', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['retro'], content_types: ['video'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['retro'], content_types: ['video'] }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['retro', 'video', 'Marisol & Kestrel']);
	});

	it('offers shared location only when every peer has the same location (geography case)', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ location: 'Tokyo' }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ location: 'Tokyo' }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['Tokyo', 'Marisol & Kestrel']);
	});

	it('location is string-equality only — different locations contribute nothing', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ location: 'Tokyo' }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ location: 'Osaka' }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['Marisol & Kestrel']);
	});

	it('offers languages only when nothing above matched (weakest signal, last)', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ languages: ['en', 'ja'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ languages: ['en', 'de'] }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['en', 'Marisol & Kestrel']);
	});

	it('languages are NOT offered when a higher-priority source matched', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime'], languages: ['en'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime'], languages: ['en'] }) }),
		];
		const result = suggestGroupNames(peers);
		expect(result).toContain('anime');
		expect(result).not.toContain('en');
	});

	it('returns at most 3 suggestions, fallback always last', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime', 'vinyl', 'books'], content_types: ['video', 'audio'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime', 'vinyl', 'books'], content_types: ['video', 'audio'] }) }),
		];
		const result = suggestGroupNames(peers);
		expect(result.length).toBeLessThanOrEqual(3);
		expect(result[result.length - 1]).toBe('Marisol & Kestrel');
	});

	it('de-duplicates case-insensitively, keeping first-seen casing', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['Anime'], content_types: ['ANIME'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime'], content_types: ['anime'] }) }),
		];
		const result = suggestGroupNames(peers);
		expect(result[0]).toBe('Anime');
		expect(result).toEqual(['Anime', 'Marisol & Kestrel']);
	});

	it('never_a_default_pre_fill: returns suggestions the caller must not auto-apply — list ends with the petname fallback, never empty', () => {
		// No shared anything: the ONLY entry is the petname fallback. A caller that pre-fills would
		// silently name the group by accident, so the fallback is a suggestion, not a default.
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['x'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['y'] }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['Marisol & Kestrel']);
	});

	it('handles a missing profile on any peer without throwing — that source contributes nothing', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel' }), // no profile at all
		];
		expect(() => suggestGroupNames(peers)).not.toThrow();
		expect(suggestGroupNames(peers)).toEqual(['Marisol & Kestrel']);
	});

	it('ignores empty-string and whitespace-only tag/location values', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime', '   ', ''], location: '   ' }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime', '  '], location: '' }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['anime', 'Marisol & Kestrel']);
	});

	it('is deterministic for a given input — same input yields identical output across calls (no Set-iteration flicker)', () => {
		// Many tags to maximize the chance of ordering instability from any internal Set use.
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['zeta', 'alpha', 'mu', 'beta', 'gamma'], content_types: ['video', 'audio', 'image'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['zeta', 'alpha', 'mu', 'beta', 'gamma'], content_types: ['video', 'audio', 'image'] }) }),
		];
		const first = suggestGroupNames(peers);
		for (let i = 0; i < 20; i++) {
			expect(suggestGroupNames(peers)).toEqual(first);
		}
	});

	describe('petname fallback', () => {
		it('two named peers → "A & B"', () => {
			const peers = [makePeer({ npub: 'npub1a', petname: 'Marisol' }), makePeer({ npub: 'npub1b', petname: 'Kestrel' })];
			expect(suggestGroupNames(peers)).toEqual(['Marisol & Kestrel']);
		});

		it('three or more peers → "Lead +N"', () => {
			const peers = [
				makePeer({ npub: 'npub1a', petname: 'Marisol' }),
				makePeer({ npub: 'npub1b', petname: 'Kestrel' }),
				makePeer({ npub: 'npub1c', petname: 'Dorjan' }),
			];
			expect(suggestGroupNames(peers)).toEqual(['Marisol +2']);
		});

		it('falls back to display_name when a peer has no petname', () => {
			const peers = [
				makePeer({ npub: 'npub1a', petname: 'Marisol' }),
				makePeer({ npub: 'npub1b', profile: makeProfile({ display_name: 'Kestrel' }) }),
			];
			expect(suggestGroupNames(peers)).toEqual(['Marisol & Kestrel']);
		});

		it('a nameless peer is skipped for the lead name but still counted in +N', () => {
			const peers = [
				makePeer({ npub: 'npub1a', petname: 'Marisol' }),
				makePeer({ npub: 'npub1b' }), // nameless
				makePeer({ npub: 'npub1c' }), // nameless
			];
			expect(suggestGroupNames(peers)).toEqual(['Marisol +2']);
		});
	});

	it('an empty peer list yields an empty result', () => {
		expect(suggestGroupNames([])).toEqual([]);
	});

	it('scales to N peers (W5 multi-select reuse) — intersection narrows, +N grows', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime', 'vinyl'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime'] }) }),
			makePeer({ npub: 'npub1c', petname: 'Dorjan', profile: makeProfile({ tags: ['anime'] }) }),
		];
		expect(suggestGroupNames(peers)).toEqual(['anime', 'Marisol +2']);
	});
});
