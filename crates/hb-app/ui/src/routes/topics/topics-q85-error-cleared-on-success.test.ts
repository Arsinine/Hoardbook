// @vitest-environment jsdom
// QURATOR-85 — an overlapping FAILING request could leave the error state set AFTER a SUCCESSFUL
// one rendered real topics, and the template checked the error branch first, hiding a good list.
// QURATOR-144 W2 collapsed the six per-root error states into ONE tree-level `paintError`; the
// carried-over contract is: a later SUCCESSFUL paint clears a stale error (a stale error never
// hides a good tree). Only controlling the resolve order can reproduce it, hence the deferred
// promises. The mount-time paint always resolves here; the failing one is a user-driven Retry.
//
// Per CLAUDE.md §9 the probe (drop `paintError = false` from the paint success path, re-run this
// file) MUST fail this test.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({ topic_id: 't-new', name: '', description: '', tags: [], private: false, joined_at: 0 }),
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

import { topicDiscoverPaint } from '$lib/api.js';
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-85 (W2 form) — a successful paint clears the tree error', () => {
	it('mount paint FAILS -> error + Retry render -> Retry succeeds -> the tree renders and the error is gone', async () => {
		const good = { topic_id: 't1', name: 'video/animation/anime', description: 'a topic', tags: ['video'], member_count_estimate: 2 };
		paintMock.mockRejectedValueOnce(new Error('relay down')).mockResolvedValue([good]);

		const { getByRole, queryByRole, findByText, queryByText, container } = render(TopicsPage);
		// The mount paint failed: the retryable error renders, and the confident negative does not.
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		expect(queryByText(/haven.t joined any topics/i)).toBeNull();
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		// THE assertion: the successful re-paint clears the stale error AND lands the rows. On
		// broken code (error never cleared) the alert stays over a good tree. The video group is
		// PURE DISCOVERY (nothing joined under it), so it starts COLLAPSED — open it first.
		const header = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
			h.textContent?.includes('video'),
		);
		expect(header).toBeTruthy();
		await fireEvent.click(header!);
		await tick();
		expect(await findByText('animation/anime')).toBeTruthy();
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});
});
