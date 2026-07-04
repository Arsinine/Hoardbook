import { describe, it, expect } from 'vitest';
import {
	joinConsentCopy,
	canJoin,
	contactBadge,
	memberCountLabel,
	isDissolved,
	rosterLabel,
	PUBLIC_JOIN_CONSENT,
	PRIVATE_JOIN_CONSENT,
	NO_UNLOCK_NOTE,
	TOPIC_ROOTS,
	composeTopicPath,
	splitTopicPath,
	subPathLabel,
	groupTopicsByRoot,
} from './topics-view.js';
import type { CachedPeer } from './types.js';

describe('topics-view (M11)', () => {
	it('shows the public consent copy for a public Topic, the durable-record copy for a private one', () => {
		expect(joinConsentCopy(false)).toBe(PUBLIC_JOIN_CONSENT);
		expect(joinConsentCopy(true)).toBe(PRIVATE_JOIN_CONSENT);
		// The public copy makes the visibility explicit; the private copy names the durable record.
		expect(PUBLIC_JOIN_CONSENT.toLowerCase()).toContain('anyone who joins');
		expect(PRIVATE_JOIN_CONSENT.toLowerCase()).toContain('durable');
		expect(PRIVATE_JOIN_CONSENT.toLowerCase()).toContain('membership record');
	});

	it('F12: the join gate requires an explicit acknowledgment', () => {
		expect(canJoin(false)).toBe(false);
		expect(canJoin(true)).toBe(true);
	});

	it('badges only Topic-sourced contacts (manual adds get no badge)', () => {
		expect(contactBadge('Topic')).toBe('Topic');
		expect(contactBadge('Manual')).toBeNull();
		expect(contactBadge(undefined)).toBeNull(); // a pre-M11 contact ⇒ Manual ⇒ no badge
	});

	it('renders the member count as an approximate estimate, never a hard number', () => {
		expect(memberCountLabel(1)).toBe('~1 member (estimate)');
		expect(memberCountLabel(5)).toBe('~5 members (estimate)');
		expect(memberCountLabel(0)).toBe('~0 members (estimate)');
		expect(memberCountLabel(-3)).toBe('~0 members (estimate)'); // clamps junk
	});

	it('derives dissolution from an empty roster', () => {
		expect(isDissolved(0)).toBe(true);
		expect(isDissolved(2)).toBe(false);
	});

	it('the no-unlock note states INV-2 plainly', () => {
		expect(NO_UNLOCK_NOTE.toLowerCase()).toContain('does not unlock');
		expect(NO_UNLOCK_NOTE.toLowerCase()).toContain('share code');
	});

	it('rosterLabel_maps_known_npub_to_petname', () => {
		const contacts: CachedPeer[] = [
			{
				npub: 'npub1alice',
				petname: 'Al',
				profile: { display_name: 'Alice', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
				collections: [],
				online: false,
				last_fetched: '',
				local_tags: [],
			},
		];
		expect(rosterLabel('npub1alice', contacts)).toBe('Al');
	});

	it('rosterLabel_falls_back_to_short_npub_when_unknown', () => {
		const npub = 'npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqabcd';
		expect(rosterLabel(npub, [])).toBe(`${npub.slice(0, 8)}…${npub.slice(-4)}`);
	});
});

describe('topics-view — W4 public Topic paths', () => {
	it('offers the six fixed-root categories (a bad root is unrepresentable in the picker)', () => {
		expect([...TOPIC_ROOTS]).toEqual(['video', 'audio', 'image', 'text', 'software', 'other']);
	});

	it('composes root + freeform sub-path, dropping slash junk', () => {
		expect(composeTopicPath('video', 'animation/anime')).toBe('video/animation/anime');
		expect(composeTopicPath('video', '  /animation//anime/ ')).toBe('video/animation/anime');
		expect(composeTopicPath('audio', '')).toBe('audio'); // a bare category is valid
	});

	it('splits a path + extracts the sub-path label', () => {
		expect(splitTopicPath('video/animation/anime')).toEqual(['video', 'animation', 'anime']);
		expect(subPathLabel('video/animation/anime')).toBe('animation/anime');
		expect(subPathLabel('video')).toBe('');
	});

	it('groups discovery results into a tree by root category (ordered by TOPIC_ROOTS)', () => {
		const topics = [
			{ name: 'audio/lossless' },
			{ name: 'video/animation/anime' },
			{ name: 'video/films' },
		];
		const tree = groupTopicsByRoot(topics);
		expect(tree.map((g) => g.root)).toEqual(['video', 'audio']); // video before audio (root order)
		expect(tree[0].topics.map((t) => t.name)).toEqual(['video/animation/anime', 'video/films']);
		expect(tree[1].topics.map((t) => t.name)).toEqual(['audio/lossless']);
	});
});
