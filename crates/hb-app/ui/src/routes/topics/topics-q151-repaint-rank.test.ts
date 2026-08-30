// @vitest-environment jsdom
// QURATOR-151 — a create-forced repaint replaces `directory` with a fresh paint answer, and the
// paint half (`topic_discover_paint`) carries `member_count_estimate: null` for EVERY row. But
// `rankedIds` — the de-dup set telling rankDrawnRows "this id's count is already on screen" — was
// page state the repaint inherited unchanged. So every previously-ranked row both lost its count
// AND was suppressed from the re-fetch (`!rankedIds.has(...)` in the queue filter): the group's
// popularity ordering degraded to paint order for the rest of the session. The fix mirrors W3's
// cache-paint treatment of the same suppression: a paint that wholesale replaces `directory`
// resets `rankedIds`, so the fresh answer's rank pass re-queues the wiped rows.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (delete the `rankedIds = new Set();` reset in paintDirectory, re-run this file) MUST fail
// the first test — on broken code the second topicRank call never carries the old ids (or never
// fires for them), and the row order stays paint order.
//
// jsdom computes no layout — nothing here proves a row RENDERS as one line; the ordering assertion
// is about DOM sequence, not geometry.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({
		topic_id: 't-new',
		name: 'video/rank-new',
		description: '',
		tags: [],
		private: false,
		joined_at: 0,
	}),
	topicUpdateMeta: vi.fn(),
	topicDiscoverPaint: vi.fn(),
	topicRank: vi.fn(),
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

import { topicDiscoverPaint, topicRank } from '$lib/api.js';
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

// All under one root (video, the default) so exactly one group renders and the row sequence is
// the whole unjoined list. Paint order is B-then-A on purpose: if the re-rank ever stops
// happening, the DOM stays in paint order and the ordering assertion reds.
const A = { topic_id: 't-a', name: 'video/rank-a', description: '', tags: ['video'], member_count_estimate: null };
const B = { topic_id: 't-b', name: 'video/rank-b', description: '', tags: ['video'], member_count_estimate: null };
const NEW = { topic_id: 't-new', name: 'video/rank-new', description: '', tags: ['video'], member_count_estimate: null };

function rankByRequest(ids: string[]) {
	return ids.map((id) => ({
		topic_id: id,
		member_count_estimate: id === 't-a' ? 40 : id === 't-b' ? 5 : 1,
	}));
}

async function expandVideoGroup(container: HTMLElement) {
	const header = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
		h.textContent?.includes('video'),
	);
	expect(header).toBeTruthy();
	await fireEvent.click(header!);
	await tick();
}

async function createPublicTopic(getByRole: (role: string, opts?: object) => HTMLElement, getByPlaceholderText: (re: RegExp, opts?: object) => HTMLElement, container: HTMLElement) {
	await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
	await tick();
	await fireEvent.input(getByPlaceholderText(/sub-path/i), { target: { value: 'rank-new' } });
	await tick();
	const createBtn = [...container.querySelectorAll('button')].find(
		(b) => (b.textContent ?? '').trim().toLowerCase() === 'create',
	) as HTMLButtonElement;
	expect(createBtn).toBeTruthy();
	await fireEvent.click(createBtn);
	await tick();
}

describe('QURATOR-151 — a create-forced repaint re-fetches the ranked counts it wiped', () => {
	it('republishing paints a countless tree; previously-ranked ids are re-ranked and order survives', async () => {
		paintMock.mockResolvedValueOnce([B, A]).mockResolvedValue([B, A, NEW]);
		rankMock.mockImplementation(async (ids: string[]) => rankByRequest(ids));
		const { getByRole, getByPlaceholderText, container } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));

		// Pure-discovery group seeds COLLAPSED, so the paint-time rank pass queued nothing; expand
		// it to force the lazy rank pass over A and B (counts land, ids enter rankedIds).
		await expandVideoGroup(container);
		await waitFor(() => expect(rankMock).toHaveBeenCalledTimes(1));
		expect(rankMock.mock.calls[0][0]).toEqual(['t-b', 't-a']);
		// Wait for the FOLD, not the call: rankedIds is only written after the rank resolve, and
		// the order flip (paint order B,A -> count order A,B) is its observable effect. Without
		// this wait the create can race the fold, and on broken code rankedIds may still be empty
		// at create time — the mutation then survives (seen red-on-green, fixed here).
		await waitFor(() => {
			const names = [...container.querySelectorAll('.row.tree-child.unjoined .name')].map(
				(n) => n.textContent?.trim(),
			);
			expect(names).toEqual(['rank-a', 'rank-b']);
		});

		// Publish a new public Topic mid-session: create forces a repaint whose answer carries
		// null counts for every row.
		await createPublicTopic(getByRole, getByPlaceholderText, container);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2), { timeout: 2000 });

		// THE assertion: the repaint's rank pass re-queues the previously-ranked ids (on broken
		// code rankedIds survives the repaint, suppressing them — nothing re-fetches, and the
		// final order assertion below reds). Assert on the CALL that carries the old ids, not on
		// a call index: the repaint's rank pass can legitimately be superseded by an intervening
		// collapse/expand transition in the same turn, making the index move.
		await waitFor(
			() => {
				expect(
					rankMock.mock.calls.some((c) => {
						const ids = c[0] as string[];
						return ids.includes('t-a') && ids.includes('t-b');
					}),
				).toBe(true);
			},
			{ timeout: 2000 },
		);
		const requeuedCall = rankMock.mock.calls.find((c) => {
			const ids = c[0] as string[];
			return ids.includes('t-a') && ids.includes('t-b');
		});
		expect(requeuedCall).toBeTruthy();

		// ...and the popularity ordering survives the create: count order (40, 5, 1), not the
		// paint order (B first) the broken code degrades to.
		await waitFor(() => {
			const names = [...container.querySelectorAll('.row.tree-child.unjoined .name')].map(
				(n) => n.textContent?.trim(),
			);
			expect(names).toEqual(['rank-a', 'rank-b', 'rank-new']);
		});
	});

	it('the de-dup rankedIds exists for is otherwise intact — a stable tree never re-ranks', async () => {
		paintMock.mockResolvedValue([B, A]);
		rankMock.mockImplementation(async (ids: string[]) => rankByRequest(ids));
		const { container, getByPlaceholderText } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		// The filter force-opens matching groups, deterministically — unlike the collapse seed,
		// which races the mount-time rank pass's tick() (that pass reads collapse state before the
		// group-seeding $effect runs). Type a query matching both rows and wait for the FOLD (the
		// order flip is the observable that rankedIds and the counts landed).
		await fireEvent.input(getByPlaceholderText(/filter by path/i), { target: { value: 'rank' } });
		await tick();
		await waitFor(() => {
			const names = [...container.querySelectorAll('.row.tree-child.unjoined .name')].map(
				(n) => n.textContent?.trim(),
			);
			expect(names).toEqual(['rank-a', 'rank-b']);
		});
		expect(rankMock).toHaveBeenCalledTimes(1);

		// Now clear the filter and cycle the group through collapse/expand (the seeded state is
		// collapsed, so this is expand -> collapse -> expand): every drawn row is ranked AND
		// carries its count, so neither the rankedIds de-dup NOR the count condition may spend
		// another topicRank on them.
		await fireEvent.input(getByPlaceholderText(/filter by path/i), { target: { value: '' } });
		await tick();
		await expandVideoGroup(container); // expand (seeded collapsed)
		await expandVideoGroup(container); // collapse
		await expandVideoGroup(container); // expand again -> coalesced lazy rank (100ms window)
		await new Promise((r) => setTimeout(r, 300));
		expect(rankMock).toHaveBeenCalledTimes(1);
	});
});


