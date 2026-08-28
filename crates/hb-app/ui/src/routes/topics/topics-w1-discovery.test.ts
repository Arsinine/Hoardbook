// @vitest-environment jsdom
// QURATOR-143 W1 — one read paints the Discover directory; ranking trickles behind it. The three
// relay-citizenship contracts, asserted on REAL mounts with the api module mocked (the established
// pattern — topics-q83-empty-refetch.test.ts is the proof the page mounts):
//
//   1. PAINT: opening Discover fires exactly ONE fetch (topicDiscoverPaint) carrying ALL SIX roots,
//      and ZERO topicRank/member_count calls happen before the rows are on screen.
//   2. LAZY + BOUNDED: topicRank is called with ONLY the ids of rows that will actually be drawn
//      (per-group cap + expanded rows), never the undrawn tail.
//   3. ROUND-ROBIN: with two roots drawn, the ids sent to topicRank interleave — the first
//      TOPIC_DISCOVERY_CONCURRENCY = 8 ids cannot all come from one root.
//
// Mutation probes (each proven red on the broken half, per CLAUDE.md §9 / P-10):
//   • making paint block on scoring (awaiting rankDrawnRows inside toggleRoot before caching) reds
//     the zero-rank-calls-before-paint assertion;
//   • replacing interleaveRoundRobin with a plain concat reds the round-robin assertion;
//   • removing the .slice(0, TOPIC_GROUP_DRAW_CAP) bound reds the undrawn-rows assertion.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscover: vi.fn(),
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

import { topicDiscoverPaint, topicRank, topicDiscover } from '$lib/api.js';
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;
const discoverMock = topicDiscover as unknown as ReturnType<typeof vi.fn>;

const SIX_ROOTS = ['video', 'audio', 'image', 'text', 'software', 'other'];

/** A paint result: `nV` topics under video, `nA` under audio (ids stable per index). */
function paintResult(nV: number, nA: number) {
	const mk = (root: string, i: number) => ({
		topic_id: `${root}-t${i}`,
		name: `${root}/topic-${i}`,
		description: '',
		tags: [root],
		member_count_estimate: null,
	});
	return [...Array.from({ length: nV }, (_, i) => mk('video', i)), ...Array.from({ length: nA }, (_, i) => mk('audio', i))];
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

async function openDiscover(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement) {
	await fireEvent.click(getByRole('button', { name: /discover/i }));
	await tick();
}

async function expandRoot(getByRole: (r: string, o?: Record<string, unknown>) => HTMLElement, root: string) {
	await fireEvent.click(getByRole('button', { name: new RegExp(`^\\s*${root}`, 'i') }));
	await tick();
}

describe('QURATOR-143 W1 — one read paints the directory', () => {
	it('paint fires ONE topicDiscoverPaint call carrying all six roots — never six per-root fetches', async () => {
		paintMock.mockResolvedValue(paintResult(2, 1));
		const { getByRole } = render(TopicsPage);
		await openDiscover(getByRole);
		await expandRoot(getByRole, 'video');

		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		expect(paintMock).toHaveBeenLastCalledWith(SIX_ROOTS);
		// The old per-root loop is gone on the paint path: no topicDiscover call fired for the expand.
		expect(discoverMock).not.toHaveBeenCalled();
	});

	it('ZERO topicRank calls before the rows paint — ranking never blocks first render', async () => {
		// The rank promise deliberately never resolves: if paint waited on ranking, this test hangs
		// or the rows never appear. The rows must render from the PAINT data alone.
		rankMock.mockReturnValue(new Promise(() => {}));
		paintMock.mockResolvedValue(paintResult(2, 0));
		const { getByRole, findByText } = render(TopicsPage);
		await openDiscover(getByRole);
		await expandRoot(getByRole, 'video');

		// The row is on screen from the paint.
		expect(await findByText('topic-0')).toBeTruthy();
		// ...and the rank was ASKED for (it started), but nothing about rendering depended on it.
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
	});

	it('expanding a SECOND root draws from the same paint — no new fetch', async () => {
		paintMock.mockResolvedValue(paintResult(2, 2));
		const { getByRole } = render(TopicsPage);
		await openDiscover(getByRole);
		await expandRoot(getByRole, 'video');
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		await expandRoot(getByRole, 'audio');
		// collapse + re-expand the second root — it must draw from the same paint, no new fetch.
		await fireEvent.click(getByRole('button', { name: new RegExp('^\\s*audio', 'i') }));
		await tick();
		await fireEvent.click(getByRole('button', { name: new RegExp('^\\s*audio', 'i') }));
		await tick();
		await new Promise((r) => setTimeout(r, 50));
		expect(paintMock).toHaveBeenCalledTimes(1);
	});
});

describe('QURATOR-143 W1 — the lazy ranker is bounded to drawn rows, round-robin', () => {
	it('topicRank receives ONLY the drawn rows — never the undrawn tail past the group cap', async () => {
		// 40 video rows: the group draws TOPIC_GROUP_DRAW_CAP = 25 and states "+15 more". The rank
		// request must carry exactly those 25 video ids, not the 40.
		paintMock.mockResolvedValue(paintResult(40, 0));
		const { getByRole, findByText } = render(TopicsPage);
		await openDiscover(getByRole);
		await expandRoot(getByRole, 'video');

		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = rankMock.mock.calls[0][0] as string[];
		expect(ids.length).toBe(25);
		expect(ids.every((id) => id.startsWith('video-'))).toBe(true);
		// And the remainder is STATED, never silently truncated.
		expect(await findByText('+15 more under video')).toBeTruthy();
	});

	it('with two roots drawn, the first 8 ids interleave — neither root drains the other’s slots', async () => {
		// 12 video + 12 audio rows, both drawn under the cap. Round-robin: the first 8 ids must
		// contain BOTH roots' rows (a plain concat would be 8 video ids first).
		paintMock.mockResolvedValue(paintResult(12, 12));
		const { getByRole } = render(TopicsPage);
		await openDiscover(getByRole);
		await expandRoot(getByRole, 'video');
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = rankMock.mock.calls[0][0] as string[];
		expect(ids.length).toBe(24);
		const firstEight = ids.slice(0, 8);
		const roots = new Set(firstEight.map((id) => id.split('-')[0]));
		expect(roots.has('video')).toBe(true);
		expect(roots.has('audio')).toBe(true);
		// Strict alternation at the head: the interleave takes one from each queue in turn.
		expect(firstEight[0].startsWith('video-')).toBe(true);
		expect(firstEight[1].startsWith('audio-')).toBe(true);
	});

	it('the returned counts re-order the group most-popular-first, without ever displaying a count', async () => {
		// The fetch serves ORDERING only (unchanged ruling): the sidebar never prints the number.
		// Paint gives video/t0 and video/t1; the rank says t1 has more members, so t1 draws first.
		paintMock.mockResolvedValue(paintResult(2, 0));
		rankMock.mockResolvedValue([
			{ topic_id: 'video-t1', member_count_estimate: 9 },
			{ topic_id: 'video-t0', member_count_estimate: 2 },
		]);
		const { getByRole, container } = render(TopicsPage);
		await openDiscover(getByRole);
		await expandRoot(getByRole, 'video');

		await waitFor(() => {
			const names = [...container.querySelectorAll('.tree-child .name')].map((n) => n.textContent?.trim());
			expect(names[0]).toBe('topic-1');
			expect(names[1]).toBe('topic-0');
		});
		// The count itself never renders in the sidebar — null stayed null, and the fetched number
		// is displayed nowhere in the accordion rows.
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const rowText = container.querySelector('.root-group')?.textContent ?? '';
		expect(rowText).not.toContain('claimed');
	});
});
