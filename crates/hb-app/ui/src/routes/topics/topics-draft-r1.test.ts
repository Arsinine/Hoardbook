// @vitest-environment jsdom
// Hoardbook Topics draft r1 — the three presentational additions to the Topics page: the PersonRow
// roster, the unread pill on the My Topics rows, and the Discover search. BEHAVIOURAL tests (real
// mount + fireEvent drive, mocking only `$lib/api.js`), per CLAUDE.md §7 — the route page mounts in
// vitest (proven by topics-q83-empty-refetch.test.ts), so every claim here is asserted on rendered
// DOM, not source strings. The stores the page reads (`contacts`, `identity`, `profile`,
// `topicAnnounceSummaries`, `announceSeen`) are set directly — they are app-wide writable stores
// the layout populates in production, which is exactly the "already-fetched data" contract this
// work sits on (no new fetch is the point of the pill).
//
// Per CLAUDE.md §9, each test was proven red by mutating the production half it pins (commenting
// out the pill render, dropping the fingerprint prop pass-through, and clearing the search corpus)
// — see the report in the task brief.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import {
	contacts,
	identity,
	profile,
	topicAnnounceSummaries,
	announceSeen,
} from '$lib/stores.js';
import { ANNOUNCE_EXPLAINER } from '$lib/announce-view.js';
import type { CachedPeer } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscover: vi.fn(),
	topicLookup: vi.fn().mockResolvedValue({ topic_id: '', name: '', exists: false, member_count_estimate: 0 }),
	topicJoinPublic: vi.fn(),
	topicRedeemInvite: vi.fn(),
	topicPreviewInvite: vi.fn(),
	topicLeave: vi.fn(),
	topicInvite: vi.fn(),
	topicRoster: vi.fn().mockResolvedValue([]),
	topicAnnounce: vi.fn(),
	topicAnnounceStatus: vi.fn().mockResolvedValue(0),
}));

import { topicDiscover, topicRoster, topicList } from '$lib/api.js';
const discoverMock = topicDiscover as unknown as ReturnType<typeof vi.fn>;
const rosterMock = topicRoster as unknown as ReturnType<typeof vi.fn>;
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;

const SELF_NPUB = 'npub1selfselfselfselfselfselfselfselfselfselfselfse';
const CONTACT_NPUB = 'npub1contactcontactcontactcontactcontactcontactca';
const STRANGER_NPUB = 'npub1strangerstrangerstrangerstrangerstrange';

function makeContact(overrides: Partial<CachedPeer> = {}): CachedPeer {
	return {
		npub: CONTACT_NPUB,
		petname: 'Carol',
		profile: { display_name: 'Carol', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
		collections: [],
		online: true,
		last_fetched: '',
		local_tags: [],
		fingerprint: { words: ['amber', 'cedar', 'jade', 'quartz', 'tarn'], colorHex: '#f00' },
		...overrides,
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	// Reset the app-wide stores this suite seeds — a leaked value would bleed into any later test
	// in the same worker that reads them (the stores are module singletons).
	contacts.set([]);
	identity.set(null);
	profile.set(null);
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
});

/** Click the first My Topics row (an `.topic-row` button) so the detail pane + roster render, and
 *  wait for the roster fetch to settle. Waits for the topic list to load first — `loadMine` is an
 *  onMount async, so the row does not exist on the first paint. */
async function openFirstTopic(container: HTMLElement) {
	await waitFor(() => {
		expect(container.querySelector('.topic-row')).not.toBeNull();
	});
	const btn = container.querySelector<HTMLButtonElement>('.topic-row');
	expect(btn).toBeTruthy();
	await fireEvent.click(btn!);
	await waitFor(() => expect(rosterMock).toHaveBeenCalled());
}

describe('Hoardbook Topics draft r1 — roster rows (PersonRow)', () => {
	it('renders avatar + name + fingerprint for a saved contact with a resolved fingerprint', async () => {
		contacts.set([makeContact()]);
		identity.set({
			npub: SELF_NPUB,
			npub_short: 'npub1sel…lfse',
			share_code: 'hbk-x',
			key_storage: 'plain-file',
		});
		profile.set({ display_name: 'Me', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' });
		rosterMock.mockResolvedValue([CONTACT_NPUB]);
		listMock.mockResolvedValue([{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 }]);
		const { container, getByText, findByText } = render(TopicsPage);

		await openFirstTopic(container);

		// The contact resolves via rosterLabel → petname "Carol", with the fingerprint words below.
		expect(await findByText('Carol')).toBeTruthy();
		for (const w of ['amber', 'cedar', 'jade', 'quartz', 'tarn']) expect(getByText(w)).toBeTruthy();
		// Presence is known online (CachedPeer.online) → the dot renders.
		expect(container.querySelector('.online-dot')).not.toBeNull();
	});

	it('labels the self entry "Name (you)" and shows NO fingerprint row for it', async () => {
		identity.set({
			npub: SELF_NPUB,
			npub_short: 'npub1sel…lfse',
			share_code: 'hbk-x',
			key_storage: 'plain-file',
		});
		profile.set({ display_name: 'Me', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' });
		rosterMock.mockResolvedValue([SELF_NPUB]);
		listMock.mockResolvedValue([{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 }]);
		const { container, findByText } = render(TopicsPage);

		await openFirstTopic(container);

		// "You" via rosterLabel's self path — and absent-gracefully: no fingerprint line (the self
		// entry is never in the viewer's own contacts).
		expect(await findByText('Me (you)')).toBeTruthy();
		expect(container.querySelector('.fp-row')).toBeNull();
	});

	it('renders a non-contact roster member with name only — no fingerprint, no presence dot', async () => {
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue([{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 }]);
		const { container, findByText } = render(TopicsPage);

		await openFirstTopic(container);

		// rosterLabel's short-npub fallback, avatar letter present, nothing invented.
		const short = `${STRANGER_NPUB.slice(0, 8)}…${STRANGER_NPUB.slice(-4)}`;
		expect(await findByText(short)).toBeTruthy();
		expect(container.querySelector('.fp-row')).toBeNull();
		expect(container.querySelector('.online-dot')).toBeNull();
	});

	it('omits the fingerprint for a contact whose fingerprint is not yet resolved', async () => {
		contacts.set([makeContact({ fingerprint: undefined, online: false })]);
		rosterMock.mockResolvedValue([CONTACT_NPUB]);
		listMock.mockResolvedValue([{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 }]);
		const { container, findByText } = render(TopicsPage);

		await openFirstTopic(container);

		expect(await findByText('Carol')).toBeTruthy();
		expect(container.querySelector('.fp-row')).toBeNull();
		expect(container.querySelector('.online-dot')).toBeNull(); // offline contact → no dot
	});
});

describe('Hoardbook Topics draft r1 — unread pill (My Topics)', () => {
	function twoTopics() {
		return [
			{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 },
			{ topic_id: 't2', name: 'audio/vinyl', description: '', tags: [], private: false, joined_at: 0 },
		];
	}

	it('badges exactly the topics past their seen watermark', async () => {
		listMock.mockResolvedValue(twoTopics());
		topicAnnounceSummaries.set([
			{ topic_id: 't1', topic_name: 'video/anime', latest_ts: 500 },
			{ topic_id: 't2', topic_name: 'audio/vinyl', latest_ts: 500 },
		]);
		announceSeen.set({ t1: 500, t2: 100 }); // t1 exactly seen; t2 behind
		const { container } = render(TopicsPage);
		await waitFor(() => expect(container.querySelectorAll('.topic-row').length).toBe(2));

		const rows = container.querySelectorAll<HTMLButtonElement>('.topic-row');
		const pillOf = (row: Element) => row.querySelector('.unread');
		expect(pillOf(rows[0])).toBeNull(); // seen → no pill
		expect(pillOf(rows[1])).not.toBeNull(); // unseen → pill
	});

	it('renders no pill when every topic is seen', async () => {
		listMock.mockResolvedValue(twoTopics());
		topicAnnounceSummaries.set([{ topic_id: 't1', topic_name: 'video/anime', latest_ts: 500 }]);
		announceSeen.set({ t1: 500 });
		const { container } = render(TopicsPage);
		await waitFor(() => expect(container.querySelectorAll('.topic-row').length).toBe(2));
		expect(container.querySelector('.unread')).toBeNull();
	});
});

describe('Hoardbook Topics draft r1 — Discover search (already-fetched roots only)', () => {
	const hits = [
		{ topic_id: 'd1', name: 'video/animation/anime', description: 'cel work', tags: ['video'], member_count_estimate: 2 },
		{ topic_id: 'd2', name: 'audio/lossless', description: '', tags: ['audio'], member_count_estimate: 5 },
	];

	async function goToDiscover(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement) {
		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();
	}

	/** Expand `root` and let its fetch settle (clickRoot pattern from topics-q83). */
	async function expandRoot(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement, root: string) {
		await fireEvent.click(getByRole('button', { name: new RegExp(`^\\s*${root}`, 'i') }));
		await tick();
		await waitFor(() => expect(discoverMock).toHaveBeenCalled());
	}

	it('filters the cached roots by sub-path and fires NO extra topicDiscover call', async () => {
		discoverMock.mockResolvedValue(hits);
		const { getByRole, container, getByText, queryByText } = render(TopicsPage);
		await goToDiscover(getByRole);
		await expandRoot(getByRole, 'video');
		const callsAfterExpand = discoverMock.mock.calls.length;

		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		expect(input).toBeTruthy();
		await fireEvent.input(input!, { target: { value: 'animation' } });
		await tick();

		// THE no-new-fetch claim: typing filtered zero extra topicDiscover calls.
		expect(discoverMock.mock.calls.length).toBe(callsAfterExpand);
		// The match renders (path-only search: the sub-path label carries "animation/anime").
		expect(getByText('animation/anime')).toBeTruthy();
		// The non-matching root's topic does not.
		expect(queryByText('lossless')).toBeNull();
	});

	it('Codex review 2026-08-15: dedupes a topic_id cached under more than one root', async () => {
		// topic_discover is a TAG query, not a strict name-prefix lookup, so the same topic_id can
		// legitimately surface under two different root queries (e.g. a legacy/externally published
		// announce carrying multiple root-category tags). The mock returns the SAME hits list for
		// every root, so expanding both video and audio caches d1 under both — exactly the
		// cross-root duplication this test targets.
		discoverMock.mockResolvedValue(hits);
		const { getByRole, container } = render(TopicsPage);
		await goToDiscover(getByRole);
		await expandRoot(getByRole, 'video');
		await expandRoot(getByRole, 'audio');

		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		// 'a' matches BOTH d1 (video/animation/anime) and d2 (audio/lossless) once each per root —
		// four raw (root, hit) pairs total before dedup, two distinct topic_ids.
		await fireEvent.input(input!, { target: { value: 'a' } });
		await tick();

		const rows = container.querySelectorAll('.search-results .tree-child');
		expect(rows.length).toBe(2); // not 4 — one row per DISTINCT topic_id, not per (root, hit) pair
	});

	it('shows the partial-coverage hint while some roots have never been expanded', async () => {
		discoverMock.mockResolvedValue(hits);
		const { getByRole, container, getByText } = render(TopicsPage);
		await goToDiscover(getByRole);
		await expandRoot(getByRole, 'video'); // 1 of 6 roots fetched

		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		await fireEvent.input(input!, { target: { value: 'anime' } });
		await tick();

		expect(getByText(/expand a category to search it too/i)).toBeTruthy();
	});

	it('shows the empty result line for a query that matches nothing cached', async () => {
		discoverMock.mockResolvedValue(hits);
		const { getByRole, container, getByText } = render(TopicsPage);
		await goToDiscover(getByRole);
		await expandRoot(getByRole, 'video');

		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		await fireEvent.input(input!, { target: { value: 'zzz-no-such-topic' } });
		await tick();

		expect(getByText(/no public topics matching/i)).toBeTruthy();
	});

	it('clearing the query restores the accordion', async () => {
		discoverMock.mockResolvedValue(hits);
		const { getByRole, container } = render(TopicsPage);
		await goToDiscover(getByRole);
		await expandRoot(getByRole, 'video');

		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		await fireEvent.input(input!, { target: { value: 'animation' } });
		await tick();
		expect(container.querySelector('.search-results')).not.toBeNull();

		await fireEvent.input(input!, { target: { value: '' } });
		await tick();
		expect(container.querySelector('.search-results')).toBeNull();
		// The accordion root headers are back.
		expect(getByRole('button', { name: /^audio$/i })).toBeTruthy();
	});
});

describe('Hoardbook Topics draft r1 — announce terms visible without hovering', () => {
	// Owner, 2026-08-27: the terms moved INTO the composer's placeholder and the separate
	// `.announce-terms` caption line was deleted — "same facts, one surface instead of two."
	// A placeholder is on screen before any hover or typing, so the "visible without hovering"
	// property still holds; it's just carried by the input now, not a sibling div. The HintMarker
	// "?" affordance still carries the same ANNOUNCE_EXPLAINER constant for anyone who does hover.
	it('states the terms in the composer placeholder, visible with no hover and nothing typed', async () => {
		rosterMock.mockResolvedValue([]);
		listMock.mockResolvedValue([{ topic_id: 't1', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 }]);
		const { container } = render(TopicsPage);
		await openFirstTopic(container);

		const input = container.querySelector('.announce-row input') as HTMLInputElement | null;
		expect(input).not.toBeNull();
		// Pinned to the SAME constant the HintMarker tooltip uses, so the two can never drift apart.
		expect(input!.placeholder).toBe(ANNOUNCE_EXPLAINER);
		expect(input!.placeholder).toContain('24h');
		expect(input!.placeholder).toContain('one per hour');
		expect(input!.value).toBe('');
	});
});

describe('Hoardbook Topics draft r1 — member count wording', () => {
	// The label's unit test lives in lib/topics-view.test.ts; this pins it on the RENDERED page so a
	// revert of the wording reds here too (the page is where the owner reads it).
	//
	// SUPERSEDED r1 wording test, r4 2026-08-27 (QURATOR-143 W1): the sidebar no longer displays a
	// count AT ALL — the lazily-fetched member_count_estimate serves ORDERING only (most-popular-
	// first), and the count displays in the detail pane (W2), never here. What stays pinned is the
	// negative: neither the old "~N members (estimate)" NOR any count wording renders on the row.
	const hits41 = () => [
		{ topic_id: 'd1', name: 'video/anime', description: '', tags: ['video'], member_count_estimate: 41 },
	];

	it('renders NO count on the Discover rows — the count orders, never displays (r4)', async () => {
		discoverMock.mockResolvedValue(hits41());
		const { getByRole, container, queryByText } = render(TopicsPage);
		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();
		await fireEvent.click(getByRole('button', { name: /^\s*video/i }));
		await waitFor(() => expect(discoverMock).toHaveBeenCalled());
		// Row rendered (old-lookup path still populates the accordion)…
		await waitFor(() => expect(container.querySelector('.tree-child')).not.toBeNull());
		// …but no count wording anywhere on it.
		expect(queryByText(/estimate/)).toBeNull();
		expect(queryByText(/~41 members/)).toBeNull();
		expect(queryByText(/claimed/)).toBeNull();
	});
});
