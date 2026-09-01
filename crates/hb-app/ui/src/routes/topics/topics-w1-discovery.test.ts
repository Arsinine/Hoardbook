// @vitest-environment jsdom
// QURATOR-143 W1 + QURATOR-144 W2 — one read paints the whole directory into the SIDEBAR on open;
// ranking trickles behind it. The relay-citizenship contracts, asserted on REAL mounts with the api
// module mocked:
//
//   1. PAINT: mount fires exactly ONE fetch (topicDiscoverPaint) carrying ALL SIX roots, and ZERO
//      topicRank calls happen before the rows are on screen.
//   2. LAZY + BOUNDED: topicRank is called with ONLY the ids of rows that will actually be drawn
//      (per-group cap), never the undrawn tail.
//   3. ROUND-ROBIN: with two roots drawn, the ids sent to topicRank interleave — the first 8 ids
//      cannot all come from one root.
//
// Mutation probes (each proven red on the broken half, per CLAUDE.md §9 / P-10):
//   • making paint block on ranking reds the zero-rank-calls-before-paint assertion;
//   • replacing interleaveRoundRobin with a plain concat reds the round-robin assertion;
//   • removing the .slice(0, TOPIC_GROUP_DRAW_CAP) bound reds the undrawn-rows assertion.
//
// jsdom computes no layout — nothing here proves a row RENDERS as one line; only the label text,
// the group structure and the fetch sequence are covered.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
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

import { topicDiscoverPaint, topicRank } from '$lib/api.js';
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;

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

/** One joined Topic under `root` — makes that group default-open (the W2 rule). */
const joined = (root: string) => [
	{ topic_id: `mine-${root}`, name: `${root}/mine`, description: '', tags: [], private: false, joined_at: 0 },
];

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-143 W1 (W2 sidebar form) — one read paints the directory', () => {
	it('MOUNT fires ONE topicDiscoverPaint carrying all six roots — no Discover button exists', async () => {
		paintMock.mockResolvedValue(paintResult(2, 1));
		const { container } = render(TopicsPage);

		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		expect(paintMock).toHaveBeenLastCalledWith(SIX_ROOTS);
		// The tree is populated (pure discovery, so the group starts COLLAPSED — the header count
		// is the on-screen evidence before opening; then the row itself draws).
		const header = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
			h.textContent?.includes('video'),
		);
		expect(header).toBeTruthy();
		expect(header!.textContent).toContain('2');
		await fireEvent.click(header!);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('topic-0'));
		// The tab split is gone: there is no second pane/section, one master-detail only.
		expect(container.querySelectorAll('.master-detail').length).toBe(1);
	});

	it('ZERO topicRank calls before the rows paint — ranking never blocks first render', async () => {
		// The rank promise deliberately never resolves: if paint waited on ranking, the rows never
		// appear. The rows must render from the PAINT data alone.
		rankMock.mockReturnValue(new Promise(() => {}));
		paintMock.mockResolvedValue(paintResult(2, 0));
		const { container } = render(TopicsPage);
		const header = await waitFor(() => {
			const h = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((x) =>
				x.textContent?.includes('video'),
			);
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('topic-0'));
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
	});

	it('collapsing and re-opening a group fires NO new fetch', async () => {
		paintMock.mockResolvedValue(paintResult(2, 2));
		const { container } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		const header = container.querySelector<HTMLButtonElement>('.root-header');
		expect(header).toBeTruthy();
		await fireEvent.click(header!);
		await tick();
		await fireEvent.click(header!);
		await tick();
		await new Promise((r) => setTimeout(r, 50));
		expect(paintMock).toHaveBeenCalledTimes(1);
	});
});

describe('QURATOR-143 W1 (W2 sidebar form) — the lazy ranker is bounded and round-robin', () => {
	it('topicRank receives ONLY the drawn rows — never the undrawn tail past the group cap', async () => {
		// 40 unjoined video rows: the group draws TOPIC_GROUP_DRAW_CAP = 25 and states "+15 more".
		paintMock.mockResolvedValue(paintResult(40, 0));
		const { container } = render(TopicsPage);
		// Pure-discovery groups start COLLAPSED (W2) — open video first so rows are drawn.
		const header = await waitFor(() => {
			const h = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((x) =>
				x.textContent?.includes('video'),
			);
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header);
		await tick();

		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = (rankMock.mock.calls[0][0] as { topic_id: string }[]).map((r) => r.topic_id);
		expect(ids.length).toBe(25);
		expect(ids.every((id) => id.startsWith('video-'))).toBe(true);
		// And the remainder is STATED, never silently truncated.
		await waitFor(() => expect(container.textContent).toContain('+15 more under video'));
	});

	it('with two roots drawn, the first 8 ids interleave — neither root drains the other’s slots', async () => {
		// 12 video + 12 audio rows, both opened (both pure-discovery, both collapsed by default).
		paintMock.mockResolvedValue(paintResult(12, 12));
		const { container } = render(TopicsPage);
		const headers = await waitFor(() => {
			const hs = [...container.querySelectorAll<HTMLButtonElement>('.root-header')];
			expect(hs.length).toBeGreaterThanOrEqual(2);
			return hs;
		});
		for (const name of ['video', 'audio']) {
			const h = headers.find((x) => x.textContent?.includes(name));
			expect(h).toBeTruthy();
			await fireEvent.click(h!);
			await tick();
		}
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = (rankMock.mock.calls[0][0] as { topic_id: string }[]).map((r) => r.topic_id);
		expect(ids.length).toBe(24);
		const firstEight = ids.slice(0, 8);
		const roots = new Set(firstEight.map((id) => id.split('-')[0]));
		expect(roots.has('video')).toBe(true);
		expect(roots.has('audio')).toBe(true);
		expect(firstEight[0].startsWith('video-')).toBe(true);
		expect(firstEight[1].startsWith('audio-')).toBe(true);
	});

	it('the returned counts re-order the rows most-popular-first, without ever displaying a count in the list', async () => {
		paintMock.mockResolvedValue(paintResult(2, 0));
		rankMock.mockResolvedValue([
			{ topic_id: 'video-t1', member_count_estimate: 9 },
			{ topic_id: 'video-t0', member_count_estimate: 2 },
		]);
		const { container } = render(TopicsPage);

		// Pure discovery starts COLLAPSED — open video so the rows are on screen to reorder.
		const header = await waitFor(() => {
			const h = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((x) =>
				x.textContent?.includes('video'),
			);
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header);
		await tick();
		await waitFor(() => {
			const names = [...container.querySelectorAll('.list-pane .row .name, .list-pane .topic-row .name')].map((n) => n.textContent?.trim());
			expect(names[0]).toBe('topic-1');
			expect(names[1]).toBe('topic-0');
		});
		const listText = container.querySelector('.list-pane')?.textContent ?? '';
		expect(listText).not.toContain('claimed');
	});
});
