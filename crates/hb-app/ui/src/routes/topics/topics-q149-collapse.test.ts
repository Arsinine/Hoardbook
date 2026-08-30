// @vitest-environment jsdom
// QURATOR-149 — the group collapse toggle is a live control even in a group holding a joined
// Topic. `isCollapsed`'s `rootsWithJoined` override rendered such a group open regardless of the
// user's own toggle: `toggleGroup` added the root to `collapsedRoots`, and `isCollapsed`'s final
// `if (rootsWithJoined.has(root)) return false` overrode it back open. The fix narrows the
// original rationale to the JOIN TRANSITION itself: `loadMine` un-collapses a root that just
// GAINED a joined Topic (so a toggle-then-join never hides the user's own Topic behind a collapsed
// header), and `isCollapsed` never consults `rootsWithJoined` afterwards.
//
// QURATOR-152/154 — the template checked `mineLoadError` before the tree branches, so a failed
// LOCAL `topicList` read replaced the WHOLE list pane — including a directory that just painted
// (from the W3 cache, or fresh). Under W1 the two halves were separate tabs; the W2 merge means
// one half's failure could visually hide the other half's success. The fix renders the mine-load
// error as an inline notice below the tree (same retryable EmptyState dialect as the paintError
// banner), keeping the whole-pane error only when NOTHING painted.
//
// BEHAVIOURAL mount tests (render(Page) + fireEvent, mocking ONLY $lib/api.js — the established
// pattern). Each fix's test was mutation-proven (P-10): see the report for the exact probe.
// jsdom computes no layout — nothing here proves the chevron rotates; the structural tells are
// `aria-expanded` on the header and whether the rows are drawn at all.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { topicAnnounceSummaries, announceSeen, topicDirectoryCache } from '$lib/stores.js';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn(),
	topicCreate: vi.fn().mockResolvedValue({ topic_id: 't-new', name: '', description: '', tags: [], private: false, joined_at: 0 }),
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

import { topicList, topicDiscoverPaint, topicJoinPublic } from '$lib/api.js';
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const joinMock = topicJoinPublic as unknown as ReturnType<typeof vi.fn>;

/** A joined Topic (local record). */
function mine(topic_id: string, name: string, description = '', priv = false) {
	return { topic_id, name, description, tags: [], private: priv, joined_at: 0 };
}
/** A discovered (announced) public Topic. */
function disc(topic_id: string, name: string, description = '', count: number | null = null) {
	return { topic_id, name, description, tags: [name.split('/')[0]], member_count_estimate: count };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
	topicDirectoryCache.set([]);
});

/** The root-group header button for `root`, or null. */
function headerFor(container: HTMLElement, root: string) {
	return [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
		(h.textContent ?? '').toLowerCase().includes(root),
	) ?? null;
}

/** Drive the consent-gated public join for `name`: open the modal, ack, confirm. */
async function joinViaConsent(container: HTMLElement, openModal: () => Promise<void>) {
	await openModal();
	await tick();
	const ack = [...container.querySelectorAll<HTMLInputElement>('.join-consent input[type="checkbox"]')][0];
	expect(ack).toBeTruthy();
	await fireEvent.click(ack);
	await tick();
	const joinTopicBtn = [...container.querySelectorAll<HTMLButtonElement>('.join-consent button')].find((b) =>
		(b.textContent ?? '').trim() === 'Join Topic',
	)!;
	expect(joinTopicBtn).toBeTruthy();
	await fireEvent.click(joinTopicBtn);
	await tick();
}

describe('QURATOR-149 — the collapse toggle is live in a group with a joined Topic', () => {
	it('collapsing a group that holds a joined Topic STAYS collapsed (the chevron is not dead)', async () => {
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro')]);
		const { container, findByText } = render(TopicsPage);

		// video starts OPEN (joined under it).
		expect(await findByText('video/anime')).toBeTruthy();
		expect(container.textContent).toContain('retro');
		const video = headerFor(container, 'video')!;
		expect(video.getAttribute('aria-expanded')).toBe('true');

		// THE assertion: the user collapses it — and it STAYS collapsed. On the broken code the
		// `rootsWithJoined` override flips it back open, so this row is still drawn.
		await fireEvent.click(video);
		await tick();
		expect(video.getAttribute('aria-expanded')).toBe('false');
		expect(container.textContent).not.toContain('video/anime');
		expect(container.textContent).not.toContain('retro');

		// The control works BOTH ways: expanding redraws the rows.
		await fireEvent.click(video);
		await tick();
		expect(video.getAttribute('aria-expanded')).toBe('true');
		expect(container.textContent).toContain('video/anime');
	});

	it('a group COLLAPSED at the moment a join lands under it OPENS (the original rationale, preserved)', async () => {
		// Start with NOTHING joined under video (so it seeds collapsed) and an announced row to join.
		// The mock chain is set UP FRONT so the SECOND loadMine (confirmJoin's) deterministically
		// carries the join — re-mocking after the join fires is a race.
		listMock.mockResolvedValueOnce([]).mockResolvedValue([mine('t1', 'video/retro')]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb')]);
		joinMock.mockResolvedValue(undefined);
		const { container } = render(TopicsPage);

		const video = await waitFor(() => {
			const h = headerFor(container, 'video');
			expect(h).toBeTruthy();
			return h!;
		});
		// Pure discovery → collapsed (the seed rule).
		expect(video.getAttribute('aria-expanded')).toBe('false');

		// Open the group ONLY to select the row, then COLLAPSE it again — the detail pane keeps the
		// unjoined selection, so the join can be confirmed while the group is genuinely collapsed.
		await fireEvent.click(video); // open → reach the row
		await tick();
		const row = await waitFor(() => {
			const el = [...container.querySelectorAll('.list-pane .row .name')].find((n) =>
				(n.textContent ?? '').trim() === 'retro',
			);
			expect(el).toBeTruthy();
			return el!;
		});
		await fireEvent.click(row.closest('.row')!); // select → detail pane
		await tick();
		await waitFor(() => expect(container.textContent).toContain('not a member'));
		await fireEvent.click(video); // collapse — the group is now collapsed AT JOIN TIME
		await tick();
		expect(video.getAttribute('aria-expanded')).toBe('false');

		// Join video/retro through the real consent path (detail-pane Join → ack → Join Topic).
		await joinViaConsent(container, async () => {
			const detailJoin = [...container.querySelectorAll<HTMLButtonElement>('.detail-pane button')].find((b) =>
				(b.textContent ?? '').trim() === 'Join',
			)!;
			expect(detailJoin).toBeTruthy();
			await fireEvent.click(detailJoin);
		});

		await waitFor(() => expect(joinMock).toHaveBeenCalledWith('video/retro'));
		// THE assertion: the join transition OPENED the group it landed under — the user's own
		// Topic is not hidden behind a collapsed header. On a fix that deleted the rationale
		// entirely, the group stays collapsed and this reds.
		await waitFor(() => expect(headerFor(container, 'video')!.getAttribute('aria-expanded')).toBe('true'));
		// A JOINED row keeps the full path (unjoined rows are stripped to their sub-path label).
		await waitFor(() => expect(container.textContent).toContain('video/retro'));
	});

	it('a join that lands under an ALREADY-OPEN group changes nothing (no spurious state churn)', async () => {
		// video already holds a join (open); a second join under it must not need any un-collapse.
		listMock.mockResolvedValueOnce([mine('t1', 'video/anime')]).mockResolvedValue([
			mine('t1', 'video/anime'),
			mine('t2', 'video/retro'),
		]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb')]);
		joinMock.mockResolvedValue(undefined);
		const { container, findByText } = render(TopicsPage);

		expect(await findByText('video/anime')).toBeTruthy();
		const video = headerFor(container, 'video')!;
		expect(video.getAttribute('aria-expanded')).toBe('true');

		await joinViaConsent(container, async () => {
			const joinBtn = [...container.querySelectorAll<HTMLButtonElement>('.list-pane .row button')].find((b) =>
				(b.textContent ?? '').trim() === 'Join',
			)!;
			expect(joinBtn).toBeTruthy();
			await fireEvent.click(joinBtn);
		});
		await waitFor(() => expect(joinMock).toHaveBeenCalledWith('video/retro'));
		// The second join landed as a JOINED row (full path), and the group stayed open throughout.
		await waitFor(() => expect(container.textContent).toContain('video/retro'));
		expect(headerFor(container, 'video')!.getAttribute('aria-expanded')).toBe('true');
	});
});

describe('QURATOR-152/154 — a failed mine load never hides a painted directory', () => {
	it('mine load FAILS, directory painted → the tree STAYS visible and the failure is an inline notice', async () => {
		listMock.mockRejectedValue(new Error('local db unavailable'));
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'announced blurb')]);
		const { container, findByText } = render(TopicsPage);

		// The directory painted and stays painted: its group header and rows are still there.
		const header = await waitFor(() => {
			const h = headerFor(container, 'video');
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header); // pure discovery → open it; the rows must be REACHABLE
		await tick();
		expect(await findByText('retro')).toBeTruthy();
		expect(container.textContent).toContain('announced blurb');

		// THE assertion: the failure rides BELOW the tree as a retryable alert, never replaces it.
		const alert = await waitFor(() => {
			const a = container.querySelector('[role="alert"]');
			expect(a).toBeTruthy();
			return a!;
		});
		expect(alert.textContent).toContain("Couldn't load your Topics");
		expect(container.querySelector('.root-header')).toBeTruthy();
		// The painted rows are still there — the pane was not swapped for the error.
		expect(container.textContent).toContain('retro');
	});

	it('mine load FAILS with NOTHING painted → the whole-pane error state still renders (no blank)', async () => {
		listMock.mockRejectedValue(new Error('local db unavailable'));
		paintMock.mockResolvedValue([]);
		const { container, queryByRole, findByRole } = render(TopicsPage);

		const alert = await findByRole('alert');
		expect(alert.textContent).toContain("Couldn't load your Topics");
		// The tree never painted: no group headers either.
		expect(container.querySelector('.root-header')).toBeNull();
		expect(queryByRole('button', { name: /retry/i })).toBeTruthy();
	});

	it('Retry on the inline notice re-fetches mine; success clears the notice and keeps the tree', async () => {
		listMock.mockRejectedValueOnce(new Error('first attempt fails')).mockResolvedValue([mine('t1', 'video/anime')]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'announced blurb')]);
		const { container, findByRole } = render(TopicsPage);

		const alert = await findByRole('alert');
		await fireEvent.click(alert.querySelector<HTMLButtonElement>('button')!);
		await tick();

		// The mine list landed and the error is gone — but the directory was never disturbed.
		await waitFor(() => expect(container.textContent).toContain('video/anime'));
		await waitFor(() => expect(container.querySelector('[role="alert"]')).toBeNull());
		expect(container.textContent).toContain('retro');
	});
});
