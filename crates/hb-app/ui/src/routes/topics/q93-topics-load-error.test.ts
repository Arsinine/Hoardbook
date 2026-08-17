// @vitest-environment jsdom
// QURATOR-93 (Topics half) — a FAILED `loadMine` (topicList) used to toast its failure and then fall
// through to the template, whose `mine.length === 0` branch rendered the confident "You haven't joined
// any Topics yet" negative on data that never arrived. The fix splits the mine load into a DISTINCT
// `mineLoadError` state (same machine as Discover's erroredRoots one tab over): a FAILED load renders
// an error + Retry; a later success clears it (the QURATOR-80/85 both-directions rule).
//
// BEHAVIOURAL mount tests: assert on the AFFORDANCES (role=alert, Retry BUTTON) plus the ABSENCE of
// the confident "haven't joined any Topics" string. This file only touches the MINE tab's loadMine —
// the Discover-tab suites (topics-q83, q80, q85) have their own mocks and are unaffected.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { topicAnnounceSummaries, announceSeen } from '$lib/stores.js';

// Mock the api module — every topic_* Tauri command is stubbed. topicList is the spy under test
// (loadMine's fetch); the others just need to resolve so the page's mount/create flow don't throw.
vi.mock('$lib/api.js', () => ({
	topicList: vi.fn(),
	topicCreate: vi.fn().mockResolvedValue({
		topic_id: 't-new',
		name: 'video/animation/anime',
		description: '',
		tags: [],
		private: false,
		joined_at: 0,
	}),
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

import { topicList } from '$lib/api.js';
const topicListMock = topicList as unknown as ReturnType<typeof vi.fn>;

const TOPIC = {
	topic_id: 't-video',
	name: 'video/anime',
	description: '',
	tags: ['video'],
	private: false,
	joined_at: 0,
};

const EMPTY_STRING = /haven.t joined any topics yet/i;
const ERROR_STRING = /couldn.t load your topics/i;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
});

describe('QURATOR-93 — Topics mine load failure is not a confident empty', () => {
	it('loadMine REJECTS → error alert + Retry render; the confident "haven\'t joined any Topics" does NOT', async () => {
		topicListMock.mockRejectedValue(new Error('listing unavailable'));

		const { getByRole, queryByText } = render(TopicsPage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		// The Retry affordance is a BUTTON (not the word appearing in prose).
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		// THE core assertion: the confident negative must NOT render on a failed load.
		expect(queryByText(EMPTY_STRING)).toBeNull();
		expect(queryByText(ERROR_STRING)).toBeTruthy();
	});

	it('Retry re-fetches: reject → resolve → the mine list renders and the error is gone', async () => {
		topicListMock.mockRejectedValueOnce(new Error('first attempt fails')).mockResolvedValue([TOPIC]);

		const { getByRole, queryByRole, findByText } = render(TopicsPage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		// The mine list actually lands (the user's real goal)…
		expect(await findByText('video/anime')).toBeTruthy();
		// …the error alert is gone.
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});

	it('a SUCCESSFUL load renders the genuine empty state, not the error', async () => {
		topicListMock.mockResolvedValue([]);

		const { queryByRole, findByText } = render(TopicsPage);
		expect(await findByText(EMPTY_STRING)).toBeTruthy();
		expect(queryByRole('alert')).toBeNull();
	});
});
