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
	sortChannelPostsAscending,
	interleaveChannel,
	resolveTopicParam,
	createPrimaryAction,
	unseenTopicAnnouncements,
	unseenAnnouncementCount,
	newlyArrivedAnnouncements,
	announcementBaseline,
} from './topics-view.js';
import type { CachedPeer, ChannelPost, AnnouncementView, TopicLookup } from './types.js';

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

	// devtest #3: the viewer is never in their own contacts, so their own roster row must resolve via
	// `self` to their published display_name — "Name (you)" — instead of a bare npub.
	it('rosterLabel_labels_the_self_entry_with_display_name_and_you', () => {
		const self = { npub: 'npub1me', display_name: 'Me' };
		expect(rosterLabel('npub1me', [], self)).toBe('Me (you)');
	});

	it('rosterLabel_self_with_blank_name_reads_You', () => {
		expect(rosterLabel('npub1me', [], { npub: 'npub1me', display_name: '  ' })).toBe('You');
		expect(rosterLabel('npub1me', [], { npub: 'npub1me' })).toBe('You');
	});

	it('rosterLabel_self_only_matches_the_own_npub', () => {
		const npub = 'npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqabcd';
		expect(rosterLabel(npub, [], { npub: 'npub1me', display_name: 'Me' })).toBe(`${npub.slice(0, 8)}…${npub.slice(-4)}`);
	});
});

describe('topics-view — devtest #2 announcement alert', () => {
	const s = (topic_id: string, latest_ts: number) => ({ topic_id, topic_name: topic_id, latest_ts });

	it('unseenTopicAnnouncements keeps only topics past their seen watermark', () => {
		const summaries = [s('a', 100), s('b', 200), s('c', 300)];
		const seen = { a: 100, b: 150 }; // a: exactly seen (not unseen), b: behind, c: never seen
		expect(unseenTopicAnnouncements(summaries, seen).map((x) => x.topic_id)).toEqual(['b', 'c']);
		expect(unseenAnnouncementCount(summaries, seen)).toBe(2);
	});

	it('an absent watermark counts any announcement as unseen', () => {
		expect(unseenAnnouncementCount([s('a', 1)], {})).toBe(1);
	});

	it('newlyArrivedAnnouncements only toasts topics newer than the baseline AND unseen', () => {
		const summaries = [s('a', 200), s('b', 200), s('c', 200)];
		const seen = { a: 0, b: 250, c: 0 }; // b is already seen past 200 → never toast
		const baseline = { a: 100, b: 100, c: 200 }; // c unchanged since last poll → not "newly arrived"
		expect(newlyArrivedAnnouncements(summaries, seen, baseline).map((x) => x.topic_id)).toEqual(['a']);
	});

	it('the first poll (empty baseline) toasts nothing — the backlog only badges', () => {
		const summaries = [s('a', 200), s('b', 300)];
		expect(newlyArrivedAnnouncements(summaries, {}, {})).toEqual([]);
		// …but it is still unseen for the badge.
		expect(unseenAnnouncementCount(summaries, {})).toBe(2);
	});

	it('announcementBaseline maps topic_id → latest_ts', () => {
		expect(announcementBaseline([s('a', 10), s('b', 20)])).toEqual({ a: 10, b: 20 });
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

describe('topics-view — devtest #12 channel post order', () => {
	function post(author_npub: string, ts: number): ChannelPost {
		return { author_npub, body: author_npub, ts };
	}

	it('sorts newest-first backend posts into oldest-to-newest for rendering', () => {
		const newestFirst = [post('c', 300), post('b', 200), post('a', 100)];
		expect(sortChannelPostsAscending(newestFirst).map((p) => p.author_npub)).toEqual(['a', 'b', 'c']);
	});

	it('does not mutate the input array', () => {
		const newestFirst = [post('b', 200), post('a', 100)];
		sortChannelPostsAscending(newestFirst);
		expect(newestFirst.map((p) => p.author_npub)).toEqual(['b', 'a']);
	});

	it('is stable on ties (equal ts)', () => {
		const withTie = [post('later', 200), post('first', 100), post('second', 100)];
		expect(sortChannelPostsAscending(withTie).map((p) => p.author_npub)).toEqual(['first', 'second', 'later']);
	});
});

describe('topics-view — devtest #6 announcement interleave', () => {
	const post = (author_npub: string, ts: number): ChannelPost => ({ author_npub, body: author_npub, ts });
	const ann = (author_npub: string, ts: number): AnnouncementView => ({ author_npub, body: author_npub, ts });

	it('merges announcements and posts into one timestamp-ascending stream', () => {
		const items = interleaveChannel(
			[post('p1', 100), post('p2', 300)],
			[ann('a1', 200)],
		);
		expect(items.map((i) => (i.kind === 'post' ? i.post.author_npub : i.announce.author_npub))).toEqual(['p1', 'a1', 'p2']);
		expect(items.map((i) => i.kind)).toEqual(['post', 'announce', 'post']);
	});

	it('places an announcement just before a post of the same second (stable)', () => {
		const items = interleaveChannel([post('p', 100)], [ann('a', 100)]);
		expect(items.map((i) => i.kind)).toEqual(['announce', 'post']);
	});

	it('handles announcements-only and posts-only', () => {
		expect(interleaveChannel([], [ann('a', 5)]).map((i) => i.kind)).toEqual(['announce']);
		expect(interleaveChannel([post('p', 5)], []).map((i) => i.kind)).toEqual(['post']);
		expect(interleaveChannel([], [])).toEqual([]);
	});
});

describe('topics-view — devtest #15 topic deep-link resolver', () => {
	const topics = [
		{ topic_id: 'abc123', name: 'video/anime' },
		{ topic_id: 'def456', name: 'audio/vinyl' },
	];

	it('resolves a joined topic_id to its TopicView', () => {
		expect(resolveTopicParam('def456', topics)?.name).toBe('audio/vinyl');
	});

	it('returns null for an absent param', () => {
		expect(resolveTopicParam(null, topics)).toBeNull();
	});

	it('returns null for an id that is not in the joined list (not-joined/unknown)', () => {
		expect(resolveTopicParam('nonexistent', topics)).toBeNull();
	});
});

describe('topics-view — devtest #11 join-first primary action', () => {
	const lookup = (exists: boolean, count = 0): TopicLookup => ({
		topic_id: 'abc123',
		name: 'video/anime',
		exists,
		member_count_estimate: count,
	});

	it('defaults to Create when there is no lookup yet', () => {
		expect(createPrimaryAction(null)).toEqual({ label: 'Create', mode: 'create' });
	});

	it('defaults to Create when the name is free (exists: false)', () => {
		expect(createPrimaryAction(lookup(false))).toEqual({ label: 'Create', mode: 'create' });
	});

	it('switches to Join, with the member estimate, when the name already has a room', () => {
		const action = createPrimaryAction(lookup(true, 7));
		expect(action.mode).toBe('join');
		expect(action.label).toContain(memberCountLabel(7));
	});
});
