// @vitest-environment jsdom
// Owner feedback #6 — Topics carries a Chat-style Refresh button in its topbar. Behavioural mount
// test: the button must exist with an accessible name, and clicking it must re-run the page's
// EXISTING loaders — loadMine (topicList) and the directory paint (topicDiscoverPaint) — asserted
// on the mocked api call counts, never on internals.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (point the button's onclick at a no-op, re-run this file) MUST fail the test here.
//
// jsdom computes no layout — nothing here proves the button RENDERS in the topbar, only that it
// exists, is named, and is wired to the loaders.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

// Same stub set as topics-q83-empty-refetch.test.ts — every Tauri command Topics imports, stubbed
// so the page's mount effects don't throw. topicList and topicDiscoverPaint are the spies.
vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({
		topic_id: 't-new',
		name: 'video/animation/anime',
		description: '',
		tags: [],
		private: false,
		joined_at: 0,
	}),
	topicUpdateMeta: vi.fn(),
	topicDiscoverPaint: vi.fn(),
	topicRank: vi.fn().mockResolvedValue([]),
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

import { topicList, topicDiscoverPaint } from '$lib/api.js';
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('Owner feedback #6 — Topics topbar Refresh button', () => {
	it('renders with an accessible name and re-runs both loaders on click', async () => {
		paintMock.mockResolvedValue([]);
		const { getByRole } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		await waitFor(() => expect(listMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 50)); // let mount settle (painting flag clears)

		const btn = getByRole('button', { name: 'Refresh topics' }) as HTMLButtonElement;
		expect(btn.getAttribute('title')).toBe('Refresh topics');
		await fireEvent.click(btn);
		await tick();

		// THE assertions that matter: one click re-pulled the joined list AND the directory paint
		// (the pane shows the merged tree — refreshing only one half would leave the visible
		// content stale).
		await waitFor(() => expect(listMock).toHaveBeenCalledTimes(2));
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2));
	});
});
