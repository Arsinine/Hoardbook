// @vitest-environment jsdom
// QURATOR-144 W2 — the self-populating sidebar. Every announced public Topic is in the left pane on
// open: joined rows in white with the admin meta line, unjoined rows muted with their blurb, grouped
// by path root and collapsible. BEHAVIOURAL mount tests (render(Page) + fireEvent, mocking ONLY
// $lib/api.js — the established pattern; topics-q83 is the proof the page mounts).
//
// Covers, each mutation-proven (P-10: break the production half on purpose, watch it red, restore):
//   1. MERGE + JOINED WINS — mine ∪ directory keyed on topic_id; a joined Topic renders from the
//      LOCAL record, never from the announce (a stale announce blurb must not win).
//   2. GROUP DEFAULTS — a group with something joined under it starts OPEN; a pure-discovery group
//      starts COLLAPSED.
//   3. THE CAP — ~25 per group with a STATED remainder ("+N more under X"); JOINED rows are never
//      truncated, even past the cap.
//   4. PATH-ONLY FILTER — matches paths, never descriptions; matching groups force-open.
//   5. LAZY CLAIMED COUNT — appears only in the detail pane for a selected unjoined Topic, fetched
//      on selection; never in the list.
//
// jsdom computes no layout — NOTHING here proves a row renders as one line, that the unjoined row
// is visually muted, or that the joined/unjoined contrast survives greyscale. Those are CSS/visual
// claims; the structural tell (meta line vs blurb + Join) IS covered here, because the two row
// states emit different DOM.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { topicAnnounceSummaries, announceSeen } from '$lib/stores.js';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscoverPaint: vi.fn().mockResolvedValue([]),
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
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;

/** A discovered (announced) public Topic. */
function disc(topic_id: string, name: string, description = '', count: number | null = null) {
	return { topic_id, name, description, tags: [name.split('/')[0]], member_count_estimate: count };
}
/** A joined Topic (local record). */
function mine(topic_id: string, name: string, description = '', priv = false) {
	return { topic_id, name, description, tags: [], private: priv, joined_at: 0 };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
});

/** The root-group header button for `root`, or null. */
function headerFor(container: HTMLElement, root: string) {
	return [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
		(h.textContent ?? '').toLowerCase().includes(root),
	) ?? null;
}
/** All drawn row labels in the open groups (joined rows keep the full path; unjoined strip it). */
function rowLabels(container: HTMLElement): string[] {
	return [...container.querySelectorAll('.list-pane .row .name, .list-pane .topic-row .name')].map((n) =>
		(n.textContent ?? '').trim(),
	);
}

/** Wait for `root`'s group to exist (the paint/mine load is async) and open it. Pure-discovery
 *  groups start COLLAPSED by the group-defaults rule, so any test that needs to touch a row under
 *  one must open the header first — the same pattern topics-w1-discovery uses. */
async function openRoot(container: HTMLElement, root: string) {
	const header = await waitFor(() => {
		const h = headerFor(container, root);
		expect(h).toBeTruthy();
		return h!;
	});
	await fireEvent.click(header);
	await tick();
}

describe('QURATOR-144 W2 — merged list, joined wins', () => {
	it('renders mine ∪ directory in one pane; a joined Topic renders from the LOCAL record, never the announce', async () => {
		// The announce for t1 carries a STALE blurb; the local record's must win. The announce
		// rows render as blurb+Join rows; the joined row carries the admin meta line instead.
		listMock.mockResolvedValue([mine('t1', 'video/anime', 'the local blurb')]);
		paintMock.mockResolvedValue([
			disc('t1', 'video/anime', 'STALE ANNOUNCE BLURB', 4),
			disc('d1', 'video/retro', 'announced blurb', null),
		]);
		const { container, findByText } = render(TopicsPage);

		expect(await findByText('video/anime')).toBeTruthy();
		await waitFor(() => expect(container.textContent).toContain('the local blurb'));
		// JOINED WINS: the stale announce blurb never renders.
		expect(container.textContent).not.toContain('STALE ANNOUNCE BLURB');
		// One row per topic_id — t1 is not duplicated by its announce.
		const animeRows = rowLabels(container).filter((l) => l.startsWith('video/anime'));
		expect(animeRows.length).toBe(1);
		// The unjoined row is there too, with its announce blurb + a Join affordance.
		expect(container.textContent).toContain('announced blurb');
		const joinBtns = [...container.querySelectorAll('.list-pane .row button')].filter((b) =>
			(b.textContent ?? '').trim() === 'Join',
		);
		expect(joinBtns.length).toBe(1);
	});

	it('private joined Topics ride the tree (they can never be announced)', async () => {
		listMock.mockResolvedValue([mine('p1', 'back room', '', true)]);
		paintMock.mockResolvedValue([]);
		const { container, findAllByText } = render(TopicsPage);
		// The root name and the row label are legitimately BOTH "back room" (a rootless private
		// name groups under itself) — assert the ROW, not the ambiguous text.
		await waitFor(() => expect(container.querySelectorAll('.topic-row').length).toBe(1));
		const rows = [...container.querySelectorAll('.topic-row .name')];
		expect(rows.some((n) => (n.textContent ?? '').includes('back room'))).toBe(true);
		// The private tag rides the joined row (the structural tell, not a colour).
		expect(container.textContent).toContain('private');
	});
});

describe('QURATOR-144 W2 — group defaults', () => {
	it('a group with something JOINED under it starts OPEN; a pure-discovery group starts COLLAPSED', async () => {
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro'), disc('d2', 'audio/lossless')]);
		const { container, findByText } = render(TopicsPage);

		// video is open (joined under it): both its rows are drawn.
		expect(await findByText('video/anime')).toBeTruthy();
		expect(container.textContent).toContain('retro');
		// audio is pure discovery: collapsed — its row is NOT drawn until opened.
		expect(container.textContent).not.toContain('lossless');
		const audio = headerFor(container, 'audio');
		expect(audio).toBeTruthy();
		expect(audio!.getAttribute('aria-expanded')).toBe('false');
		await fireEvent.click(audio!);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('lossless'));
	});
});

describe('QURATOR-144 W2 — the per-group cap', () => {
	it('caps unjoined rows at 25 with a STATED remainder; joined rows are NEVER truncated', async () => {
		// 30 joined + 30 unjoined under video: the cap would allow only 25 total if joined rows
		// counted — they must not. All 30 joined draw; the unjoined tail states +0..? — with 30
		// joined taking slots, NO unjoined rows fit, so the remainder is all 30.
		const joinedRows = Array.from({ length: 30 }, (_, i) => mine(`j${i}`, `video/joined-${i}`));
		const unjoinedRows = Array.from({ length: 30 }, (_, i) => disc(`u${i}`, `video/extra-${i}`));
		listMock.mockResolvedValue(joinedRows);
		paintMock.mockResolvedValue(unjoinedRows);
		const { container, findByText } = render(TopicsPage);

		expect(await findByText('video/joined-0')).toBeTruthy();
		// Every joined row drew — none truncated.
		for (const r of joinedRows) {
			expect(container.textContent).toContain(r.name), `joined row missing: ${r.name}`;
		}
		// The unjoined tail is STATED, not silently dropped, and not a single one drew.
		expect(container.textContent).toContain('+30 more under video');
		expect(container.textContent).not.toContain('video/extra-0');
	});

	it('a pure-discovery group under the cap draws all its rows with no remainder line', async () => {
		listMock.mockResolvedValue([]); // defensive: clearAllMocks keeps mockResolvedValue
		paintMock.mockResolvedValue([disc('d1', 'video/one'), disc('d2', 'video/two')]);
		const { container } = render(TopicsPage);
		// Pure discovery starts COLLAPSED (the group-defaults rule) — open it, then read the rows.
		await openRoot(container, 'video');
		await waitFor(() => expect(container.textContent).toContain('one'));
		expect(container.textContent).toContain('two');
		expect(container.textContent).not.toMatch(/\+\d+ more under/);
	});
});

describe('QURATOR-144 W2 — path-only filter, force-open', () => {
	it('matches PATHS ONLY — never descriptions — and force-opens the matching group', async () => {
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		paintMock.mockResolvedValue([
			disc('d1', 'video/retro', 'the word anime appears ONLY in this description'),
			disc('d2', 'audio/anime-rips'),
		]);
		const { container, findByText } = render(TopicsPage);
		expect(await findByText('video/anime')).toBeTruthy();

		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		expect(input).toBeTruthy();
		await fireEvent.input(input!, { target: { value: 'anime' } });
		await tick();

		// Path matches: the joined video/anime AND audio/anime-rips (its group force-opens).
		expect(container.textContent).toContain('video/anime');
		await waitFor(() => expect(container.textContent).toContain('anime-rips'));
		// Description matches are EXCLUDED (owner ruling: paths only) — d1's blurb never renders.
		expect(container.textContent).not.toContain('the word anime appears');
		// The non-matching group (video/retro) is filtered out entirely.
		expect(container.textContent).not.toContain('retro');
	});

	it('a query matching nothing states it honestly', async () => {
		listMock.mockResolvedValue([]); // defensive: clearAllMocks keeps mockResolvedValue
		paintMock.mockResolvedValue([disc('d1', 'video/retro')]);
		const { container } = render(TopicsPage);
		await openRoot(container, 'video');
		await waitFor(() => expect(container.textContent).toContain('retro'));
		const input = container.querySelector<HTMLInputElement>('.discover-search input');
		await fireEvent.input(input!, { target: { value: 'zzz-no-such' } });
		await tick();
		await waitFor(() => expect(container.textContent).toContain('No Topics match that path'));
	});
});

describe('QURATOR-144 W2 — claimed count is detail-only and lazy', () => {
	it('selecting an unjoined row fetches the count ONCE for that row and renders it in the DETAIL pane only', async () => {
		listMock.mockResolvedValue([]); // defensive: clearAllMocks keeps mockResolvedValue
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb', null)]);
		rankMock.mockResolvedValue([{ topic_id: 'd1', member_count_estimate: 12 }]);
		const { container } = render(TopicsPage);

		await openRoot(container, 'video');
		const row = await waitFor(() => {
			const el = [...container.querySelectorAll('.list-pane .row .name')].find((n) =>
				(n.textContent ?? '').trim() === 'retro',
			);
			expect(el).toBeTruthy();
			return el!;
		});
		await fireEvent.click(row.closest('.row')!);
		await tick();

		await waitFor(() => expect(rankMock).toHaveBeenCalledWith(['d1']));
		await waitFor(() => expect(container.textContent).toContain('12 claimed'));
		// The count lives ONLY in the detail pane — the list pane never prints it.
		const listText = container.querySelector('.list-pane')?.textContent ?? '';
		expect(listText).not.toContain('claimed');
		// The roster never renders for a non-member (members-only data).
		expect(container.textContent).not.toContain('Roster');
	});

	it('a paint that already carries the count reuses it — no extra topicRank call on selection', async () => {
		listMock.mockResolvedValue([]); // defensive: clearAllMocks keeps mockResolvedValue
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb', 7)]);
		const { container } = render(TopicsPage);
		await openRoot(container, 'video');
		const row = await waitFor(() => {
			const el = [...container.querySelectorAll('.list-pane .row .name')].find((n) =>
				(n.textContent ?? '').trim() === 'retro',
			);
			expect(el).toBeTruthy();
			return el!;
		});
		await fireEvent.click(row.closest('.row')!);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('7 claimed'));
		expect(rankMock).not.toHaveBeenCalled();
	});
});
