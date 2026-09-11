// @vitest-environment jsdom
// QURATOR-143 W1 + QURATOR-144 W2 — one read paints the whole directory into the SIDEBAR on open;
// ranking trickles behind it. The relay-citizenship contracts, asserted on REAL mounts with the api
// module mocked:
//
//   1. PAINT: mount fires exactly ONE fetch (topicDiscoverPaint) carrying ALL SIX roots.
//   2. LAZY + BOUNDED + ROUND-ROBIN + ONCE: topicRank is called with ONLY the ids of rows that
//      will actually be drawn (per-group cap), interleaved across roots, exactly once per paint —
//      never the undrawn tail, never a second call for the same rows. (This was originally
//      "ZERO topicRank calls before the rows paint", narrowed QURATOR-193: the assertion's
//      rationale was always relay citizenship — the pre-W1 path fired ~600 round trips per open —
//      not visual instancy, and the cold-paint hold below makes instancy expressly NOT a contract
//      on the cold path. Zero NEW round trips is what must hold.)
//   3. THE COLD-PAINT HOLD (QURATOR-193): a cold open (no cache) keeps the tree hidden until the
//      drawn rows' ranks fold in — and reveals on rank FAILURE too, never stranding the directory.
//      The CACHED tree (QURATOR-145 W3) paints instantly and is never held (owner ruling: cold
//      paint only).
//
// Mutation probes (each proven red on the broken half, per CLAUDE.md §9 / P-10):
//   • removing the .slice(0, TOPIC_GROUP_DRAW_CAP) bound reds the once-per-paint drawn-rows
//     assertion AND the undrawn-rows assertion below;
//   • deleting the await tick() in rankDrawnRows (ranking before collapse-seeding) reds the
//     once-per-paint assertion — the mount pass spends reads on rows the seed is about to hide;
//   • deleting the .finally() reveal in paintDirectory reds the hold tests (tree stays hidden);
//   • replacing interleaveRoundRobin with a plain concat reds the round-robin assertion.
//
// jsdom computes no layout — nothing here proves a row RENDERS as one line, and the cold-hold
// assertions read the wrapper's CLASS, not computed visibility; only the label text, the group
// structure, the fetch sequence and the hold class are covered.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { topicDirectoryCache } from '$lib/stores.js';

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

import { topicList, topicDiscoverPaint, topicRank } from '$lib/api.js';
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;

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
	// QURATOR-193: the cross-mount cache makes every later test a CACHED (never-held) mount —
	// reset it so each test controls its own cold/cached precondition (q148's idiom).
	topicDirectoryCache.set([]);
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

	it('topicRank carries ONLY the drawn rows — bounded, round-robin, exactly once per paint', async () => {
		// Narrowed QURATOR-193 (was "ZERO topicRank calls before the rows paint"): the contract's
		// rationale is relay citizenship, not visual instancy — the pre-W1 path fired ~600 round
		// trips per open. One call per paint, carrying exactly the rows that are drawn, is what
		// must hold; the cold-paint hold below may delay the REVEAL but adds no round trips.
		paintMock.mockResolvedValue(paintResult(40, 12));
		const { container } = render(TopicsPage);
		const headers = await waitFor(() => {
			const hs = [...container.querySelectorAll<HTMLButtonElement>('.root-header')];
			expect(hs.length).toBeGreaterThanOrEqual(2);
			return hs;
		});
		// Open both groups inside the coalescing window: ONE round-robin call, never one per group.
		for (const name of ['video', 'audio']) {
			const h = headers.find((x) => x.textContent?.includes(name));
			expect(h).toBeTruthy();
			await fireEvent.click(h!);
			await tick();
		}
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		expect(rankMock).toHaveBeenCalledTimes(1);
		const ids = (rankMock.mock.calls[0][0] as { topic_id: string }[]).map((r) => r.topic_id);
		// Bounded: video draws its 25-row cap (never the undrawn 15-row tail) + all 12 of audio.
		expect(ids).toHaveLength(37);
		const idSet = new Set(ids);
		expect(idSet.size).toBe(37);
		for (let i = 0; i < 25; i++) expect(idSet.has(`video-t${i}`)).toBe(true);
		for (let i = 0; i < 12; i++) expect(idSet.has(`audio-t${i}`)).toBe(true);
		expect(idSet.has('video-t25')).toBe(false);
		// Round-robin: the head of the queue alternates roots — video never drains audio's slots.
		expect(ids[0].startsWith('video-')).toBe(true);
		expect(ids[1].startsWith('audio-')).toBe(true);
		// Exactly once per paint: the fold landing triggers no second pass over the same rows.
		await new Promise((r) => setTimeout(r, 300));
		expect(rankMock).toHaveBeenCalledTimes(1);
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

describe('QURATOR-193 — the cold-paint hold (reveal only after the drawn ranks fold in)', () => {
	it('a COLD paint stays held until the rank fold lands, then reveals in ranked order', async () => {
		// A joined row under video makes that group default-OPEN, so the mount-time rank pass
		// actually queues the drawn rows — the hold is observable across a rank we control.
		listMock.mockResolvedValue(joined('video'));
		let resolveRank!: (v: { topic_id: string; member_count_estimate: number }[]) => void;
		rankMock.mockReturnValue(
			new Promise<{ topic_id: string; member_count_estimate: number }[]>((r) => {
				resolveRank = r;
			}),
		);
		paintMock.mockResolvedValue(paintResult(2, 0));
		const { container } = render(TopicsPage);

		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const tree = container.querySelector('.directory-tree')!;
		expect(tree).toBeTruthy();
		// HELD: the rows are drawn (the rank call proves the paint landed and was queued) but not
		// revealed — this is the state that used to show paint order before the reorder flicker.
		expect(tree.classList.contains('cold-hold')).toBe(true);

		// The fold lands (t1 outranks t0)…
		resolveRank([
			{ topic_id: 'video-t1', member_count_estimate: 9 },
			{ topic_id: 'video-t0', member_count_estimate: 2 },
		]);
		await waitFor(() => expect(tree.classList.contains('cold-hold')).toBe(false));
		// …and the reveal shows the RANKED order — paint order was never the visible state.
		const names = [...tree.querySelectorAll('.row.tree-child.unjoined .name')].map((n) =>
			n.textContent?.trim(),
		);
		expect(names).toEqual(['topic-1', 'topic-0']);
	});

	it('a rank FAILURE still reveals — the hold never strands the directory (best-effort catch)', async () => {
		listMock.mockResolvedValue(joined('video'));
		rankMock.mockRejectedValue(new Error('relay down'));
		paintMock.mockResolvedValue(paintResult(2, 0));
		const { container } = render(TopicsPage);
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const tree = container.querySelector('.directory-tree')!;
		expect(tree).toBeTruthy();
		await waitFor(() => expect(tree.classList.contains('cold-hold')).toBe(false));
		// Best-effort ranking: the rows remain, in paint order — an unranked directory, never a
		// hidden one.
		const names = [...tree.querySelectorAll('.row.tree-child.unjoined .name')].map((n) =>
			n.textContent?.trim(),
		);
		expect(names).toEqual(['topic-0', 'topic-1']);
	});

	it('a CACHED tree paints instantly — never held, even while ranking is pending', async () => {
		// Owner ruling ("cold paint only", QURATOR-145 W3): the last-known-good tree IS the landing
		// screen. Seed the cache and make the rank answer NEVER resolve — the tree must be revealed
		// regardless; only the no-cache path may be held. (clearAllMocks keeps implementations, so
		// the joined() topicList from the tests above is reset explicitly — this tree is pure
		// discovery, its group seeds collapsed, and the click below must EXPAND it.)
		listMock.mockResolvedValue([]);
		topicDirectoryCache.set(paintResult(2, 0));
		rankMock.mockReturnValue(new Promise(() => {}));
		paintMock.mockResolvedValue(paintResult(2, 0));
		const { container } = render(TopicsPage);
		const tree = await waitFor(() => {
			const t = container.querySelector('.directory-tree');
			expect(t).toBeTruthy();
			return t!;
		});
		expect(tree.classList.contains('cold-hold')).toBe(false);
		// The cached group is collapsed (pure discovery) — opening it draws the rows, still unheld.
		const header = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
			h.textContent?.includes('video'),
		)!;
		expect(header).toBeTruthy();
		await fireEvent.click(header);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('topic-0'));
		expect(tree.classList.contains('cold-hold')).toBe(false);
	});
});
