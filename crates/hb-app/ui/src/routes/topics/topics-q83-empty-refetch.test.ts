// @vitest-environment jsdom
// QURATOR-83 — a successful-but-EMPTY Discover result was cached for the entire app session, so a
// Topic created afterwards stayed invisible until an app restart. QURATOR-144 W2 replaced the
// per-root cache/retry machine with ONE paint (`topicDiscoverPaint`) that fills the whole tree, so
// the contract carries over as: a paint that answered EMPTY is not terminal — the NEXT paint (which
// a public create forces) actually re-asks, and its answer reaches the screen. The only honest way
// to pin a sequence is to run it and assert on the mocked CALL COUNT between steps; a test that
// only asserted the final rendered list would pass on broken code.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (drop the `painted = false; void paintDirectory()` from create(), re-run this file) MUST
// fail the publish tests here.
//
// jsdom computes no layout — nothing here proves a row RENDERS as one line; only that the row's
// label text and the fetch sequence are right.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

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

import { topicDiscoverPaint } from '$lib/api.js';
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

const published = {
	topic_id: 't-new',
	name: 'video/animation/anime',
	description: '',
	tags: ['video'],
	member_count_estimate: 1,
};

describe('QURATOR-83 (W2 form) — an empty paint is not terminal', () => {
	it('mount paints ONCE; nothing re-fetches while the user just reads the tree', async () => {
		paintMock.mockResolvedValue([published]);
		const { container } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		// Collapsing/expanding groups is pure UI — it must never re-fetch.
		const header = container.querySelector<HTMLButtonElement>('.root-header');
		expect(header).toBeTruthy();
		await fireEvent.click(header!);
		await tick();
		await fireEvent.click(header!);
		await new Promise((r) => setTimeout(r, 50));
		expect(paintMock).toHaveBeenCalledTimes(1);
	});

	it('empty paint -> publish a public Topic -> a SECOND paint fires and the row lands', async () => {
		paintMock.mockResolvedValueOnce([]).mockResolvedValue([published]);
		const { getByRole, getByPlaceholderText, getAllByRole, findByText, container } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));

		// Create a public Topic under video/.
		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		await fireEvent.input(getByPlaceholderText(/sub-path/i), { target: { value: 'animation/anime' } });
		await tick();
		const createBtn = getAllByRole('button').find(
			(b) => (b.textContent ?? '').trim().toLowerCase() === 'create',
		) as HTMLButtonElement;
		expect(createBtn).toBeTruthy();
		await fireEvent.click(createBtn);
		await tick();

		// THE assertion that matters: the publish forced a re-paint (on broken code this stays at 1
		// and the user's own Topic is invisible until restart).
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2), { timeout: 2000 });
		// ...and the answer actually lands on screen. The video group is PURE DISCOVERY here (the
		// create modal is still open, `mine` not yet re-listed with the new join), so it starts
		// COLLAPSED — open it before expecting the row.
		const header = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
			h.textContent?.includes('video'),
		);
		expect(header).toBeTruthy();
		await fireEvent.click(header!);
		await tick();
		expect(await findByText('animation/anime')).toBeTruthy();
	});

	it('a PRIVATE create never re-paints (it is unlisted — no announce exists to find)', async () => {
		paintMock.mockResolvedValue([published]);
		const { getByRole, getByPlaceholderText, container } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));

		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		const check = container.querySelector<HTMLInputElement>('input[type="checkbox"]');
		expect(check).toBeTruthy();
		await fireEvent.click(check!);
		// QURATOR-147 W5: private uses the SAME root picker + sub-path as public (no freeform field
		// anymore) — `other` + `back room` composes `other/back room`.
		const select = container.querySelector<HTMLSelectElement>('.path-row select');
		expect(select).toBeTruthy();
		await fireEvent.change(select!, { target: { value: 'other' } });
		await fireEvent.input(getByPlaceholderText(/sub-path/i), { target: { value: 'back room' } });
		await tick();
		const createBtn = getAllByRoleLikeCreate(container);
		await fireEvent.click(createBtn);
		await tick();
		await new Promise((r) => setTimeout(r, 50));
		expect(paintMock).toHaveBeenCalledTimes(1);
	});
});

function getAllByRoleLikeCreate(container: HTMLElement): HTMLButtonElement {
	const btn = [...container.querySelectorAll('button')].find(
		(b) => (b.textContent ?? '').trim().toLowerCase() === 'create',
	);
	expect(btn).toBeTruthy();
	return btn as HTMLButtonElement;
}
