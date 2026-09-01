// @vitest-environment jsdom
// The six GLM-review findings against the Topics page (2026-08-28), each pinned by a mounted test
// and mutation-proven per CLAUDE.md §9 / P-10 (revert the production half, watch THIS file's named
// test red, restore). One describe per finding:
//
//   1. A failed paint with JOINED rows on screen surfaces a retryable error banner — the tree never
//      silently reads as "no public Topics exist" (the QURATOR-80 confusion). Probe: change the
//      banner guard to `mergedRows.length > 1` (i.e. drop the banner for a joined row present) —
//      the `alert` never renders and the test reds.
//   2. JOINED topic_ids are never queued for topicRank — mergedRows hardcodes
//      member_count_estimate null for the joined half and the fold writes only into `directory`,
//      so a joined id's result is discarded (and it displaces an unjoined row from the capped
//      batch). Probe: drop the `!d.joined` filter — 'mine-joined' appears in the rank ids and reds.
//   3. After a successful confirmJoin, the detail pane re-derives from the JOINED record, not the
//      stale unjoined announce view; and the row-level Join button does not bubble into the row's
//      click-to-select. Probes: delete the selectedDiscoveredId reset (the stale "not a member"
//      line stays and reds); delete the stopPropagation (a rank call fires for the joined row's
//      click-select and reds).
//   4. Collapsed groups' rows are not ranked at paint time — only rows actually drawn are read
//      (the "only rows that will actually be drawn" contract); expanding the group ranks them
//      lazily. Probe: rank the unfiltered rowsForRoot (ignore collapse) — a paint-time rank call
//      fires with the collapsed group's ids and reds.
//   5. Keyboard focus on a roster row fetches the bio (parity with mouseenter — the component's
//      own CSS promises "surfaces under the row on hover/focus"). Probe: revert onfocus to the
//      rosterHover-only assignment — focus fires no pasteKey and reds.
//   6. A REJECTED bio resolve is NOT cached as absent: a later hover retries; a resolved empty
//      string ('') is a real empty bio, distinct from `false`'s "No published profile" line.
//      Probe: cache the reject as `false` again — the second hover fires no second pasteKey and
//      reds.
//
// jsdom computes no layout — the banner's placement/colour is not proven here, only that the
// alert affordance exists alongside the still-rendered rows and its Retry re-paints.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { identity, profile, topicAnnounceSummaries, announceSeen } from '$lib/stores.js';

vi.mock('$lib/api.js', () => ({
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
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

import { topicList, topicDiscoverPaint, topicRank, topicJoinPublic, topicRoster, pasteKey } from '$lib/api.js';
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;
const joinMock = topicJoinPublic as unknown as ReturnType<typeof vi.fn>;
const rosterMock = topicRoster as unknown as ReturnType<typeof vi.fn>;
const pasteKeyMock = pasteKey as unknown as ReturnType<typeof vi.fn>;

const SELF_NPUB = 'npub1selfselfselfselfselfselfselfselfselfselfselfse';
const STRANGER_NPUB = 'npub1strangerstrangerstrangerstrangerstrange';

function disc(topic_id: string, name: string, description = '', count: number | null = null) {
	return { topic_id, name, description, tags: [name.split('/')[0]], member_count_estimate: count };
}
function mine(topic_id: string, name: string, description = '') {
	return { topic_id, name, description, tags: [], private: false, joined_at: 0 };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
	identity.set(null);
	profile.set(null);
});

/** The root-group header button for `root`, or null. */
function headerFor(container: HTMLElement, root: string) {
	return [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
		(h.textContent ?? '').toLowerCase().includes(root),
	) ?? null;
}

async function seedSelf() {
	identity.set({ npub: SELF_NPUB, npub_short: 'npub1sel…lfse', share_code: 'hbk-x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' });
}

// ── Finding 1 — a failed paint over a joined tree is a retryable banner, never silence ─────────
describe('review 1 — paint failure with joined rows still surfaces a retryable error', () => {
	it('mount paint FAILS with a joined Topic on screen → the joined row AND the alert both render, and Retry re-paints', async () => {
		// A joined Topic under video means mergedRows > 0 even though the paint failed — the case
		// the old `paintError && mergedRows.length === 0` branch fell through silently.
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		paintMock.mockRejectedValueOnce(new Error('relay down')).mockResolvedValue([disc('d1', 'video/retro')]);

		const { container, findByText, getByRole, queryByRole } = render(TopicsPage);
		// The still-valid joined row is NOT hidden by the failure.
		expect(await findByText('video/anime')).toBeTruthy();
		// ...and the failure says so, retryably — not silence, and not the confident negative.
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		expect(container.textContent).not.toContain('Nothing here yet');

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();
		// The re-paint succeeds: the error clears and the fresh tree lands.
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2));
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});
});

// ── Finding 2 — joined topic_ids are never queued for topicRank ───────────────────────────────
describe('review 2 — the rank queue excludes joined topic_ids', () => {
	it('a root with 1 joined + 2 unjoined rows ranks ONLY the unjoined ids', async () => {
		// The joined row opens the video group (the W2 default-open rule), so all rows draw.
		listMock.mockResolvedValue([mine('mine-joined', 'video/mine')]);
		paintMock.mockResolvedValue([disc('u1', 'video/one'), disc('u2', 'video/two')]);
		rankMock.mockResolvedValue([]);

		render(TopicsPage);
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = rankMock.mock.calls.flatMap((c) => (c[0] as { topic_id: string }[]).map((r) => r.topic_id));
		expect(ids).not.toContain('mine-joined'); // the joined id never spends a read
		expect(ids).toContain('u1');
		expect(ids).toContain('u2');
	});

	it('a joined id displacing an unjoined row from the capped batch is visible in the queue length', async () => {
		// 1 joined + 26 unjoined under video: the cap is 25 drawn rows, and the joined row draws
		// WITHOUT consuming a rank slot — the queue holds only unjoined ids.
		listMock.mockResolvedValue([mine('mine-joined', 'video/mine')]);
		paintMock.mockResolvedValue(Array.from({ length: 26 }, (_, i) => disc(`u${i}`, `video/x-${i}`)));
		rankMock.mockResolvedValue([]);

		render(TopicsPage);
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = rankMock.mock.calls.flatMap((c) => (c[0] as { topic_id: string }[]).map((r) => r.topic_id));
		expect(ids.every((id) => id !== 'mine-joined')).toBe(true);
		expect(ids.length).toBeLessThanOrEqual(25); // the cap spends itself on unjoined rows only
	});
});

// ── Finding 3 — post-join detail re-derivation + Join-button propagation ──────────────────────
describe('review 3 — after a successful join the detail pane shows the JOINED view', () => {
	it('joining from the detail pane clears the stale "not a member" view and opens the joined record', async () => {
		listMock.mockResolvedValueOnce([]).mockResolvedValue([mine('d1', 'video/retro')]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb')]);
		joinMock.mockResolvedValue(undefined);
		rosterMock.mockResolvedValue([]);

		const { container } = render(TopicsPage);
		// video is pure discovery (nothing joined yet) — open it, select the unjoined row.
		const header = await waitFor(() => {
			const h = headerFor(container, 'video');
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header);
		await tick();
		const row = await waitFor(() => {
			const el = [...container.querySelectorAll('.list-pane .row .name')].find((n) =>
				(n.textContent ?? '').trim() === 'retro',
			);
			expect(el).toBeTruthy();
			return el!;
		});
		await fireEvent.click(row.closest('.row')!);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('not a member'));

		// The detail pane's own Join opens the consent modal (the F12 gate).
		const detailJoin = [...container.querySelectorAll<HTMLButtonElement>('.detail-pane button')].find((b) =>
			(b.textContent ?? '').trim() === 'Join',
		)!;
		expect(detailJoin).toBeTruthy();
		await fireEvent.click(detailJoin);
		await tick();
		// ...then ack + confirm through it.
		const ack = [...container.querySelectorAll<HTMLInputElement>('.join-consent input[type="checkbox"]')][0];
		await fireEvent.click(ack);
		await tick();
		const joinTopicBtn = [...container.querySelectorAll<HTMLButtonElement>('.join-consent button')].find((b) =>
			(b.textContent ?? '').trim() === 'Join Topic',
		)!;
		await fireEvent.click(joinTopicBtn);
		await tick();

		await waitFor(() => expect(joinMock).toHaveBeenCalledWith('video/retro'));
		await waitFor(() => expect(rosterMock).toHaveBeenCalled());
		// THE assertions: the stale unjoined view is gone; the joined view (roster pane) is in.
		await waitFor(() => expect(container.textContent).not.toContain('not a member'));
		expect(container.textContent).toContain('Roster');
	});

	it('the unjoined selection is cleared even when the joined record does not match by name', async () => {
		// The re-derivation matches `mine` on name; a name the backend normalized differently (or a
		// future rename) finds no record. The CLEAR alone must still drop the stale "not a member"
		// view — the fallback empty pane beats a stale unjoined view for a Topic just joined.
		listMock.mockResolvedValueOnce([]).mockResolvedValue([]); // mine never carries the join
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb')]);
		joinMock.mockResolvedValue(undefined);

		const { container } = render(TopicsPage);
		const header = await waitFor(() => {
			const h = headerFor(container, 'video');
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header);
		await tick();
		const row = await waitFor(() => {
			const el = [...container.querySelectorAll('.list-pane .row .name')].find((n) =>
				(n.textContent ?? '').trim() === 'retro',
			);
			expect(el).toBeTruthy();
			return el!;
		});
		await fireEvent.click(row.closest('.row')!);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('not a member'));

		const detailJoin = [...container.querySelectorAll<HTMLButtonElement>('.detail-pane button')].find((b) =>
			(b.textContent ?? '').trim() === 'Join',
		)!;
		await fireEvent.click(detailJoin);
		await tick();
		const ack = [...container.querySelectorAll<HTMLInputElement>('.join-consent input[type="checkbox"]')][0];
		expect(ack).toBeTruthy();
		await fireEvent.click(ack);
		await tick();
		const joinTopicBtn = [...container.querySelectorAll<HTMLButtonElement>('.join-consent button')].find((b) =>
			(b.textContent ?? '').trim() === 'Join Topic',
		)!;
		await fireEvent.click(joinTopicBtn);
		await tick();

		await waitFor(() => expect(joinMock).toHaveBeenCalledWith('video/retro'));
		// No joined record to open — the detail falls back to the EMPTY pane, never the stale
		// unjoined view with its live Join button.
		await waitFor(() => expect(container.textContent).not.toContain('not a member'));
		expect(container.textContent).toContain('Select a Topic');
	});

	it('the row-level Join button does not bubble into the row click-to-select (no extra topicRank)', async () => {
		listMock.mockResolvedValue([]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro', 'a blurb')]);
		rankMock.mockResolvedValue([]);

		const { container } = render(TopicsPage);
		const header = await waitFor(() => {
			const h = headerFor(container, 'video');
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(header);
		await tick();
		const joinBtn = await waitFor(() => {
			const el = [...container.querySelectorAll('.list-pane .row button')].find((b) =>
				(b.textContent ?? '').trim() === 'Join',
			);
			expect(el).toBeTruthy();
			return el!;
		});
		// Let any paint-time rank settle, then click Join: only the consent modal opens — the row's
		// own select handler (and its topicRank call) must NOT fire.
		await new Promise((r) => setTimeout(r, 50));
		rankMock.mockClear();
		await fireEvent.click(joinBtn);
		await tick();
		expect(container.querySelector('.join-consent')).toBeTruthy();
		expect(rankMock).not.toHaveBeenCalled();
	});
});

// ── Finding 4 — collapsed groups' rows are not ranked; expanding ranks them lazily ─────────────
describe('review 4 — the paint-time rank pass skips collapsed groups; expand ranks lazily', () => {
	it('a COLLAPSED pure-discovery group fires NO rank at paint time; expanding it ranks its rows', async () => {
		listMock.mockResolvedValue([]); // nothing joined → video starts collapsed
		paintMock.mockResolvedValue([disc('d1', 'video/one'), disc('d2', 'video/two')]);
		rankMock.mockResolvedValue([]);

		const { container } = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		// The group exists and is collapsed (aria-expanded=false) — the rows are NOT drawn, so no
		// rank read may fire for them.
		const header = headerFor(container, 'video');
		expect(header).toBeTruthy();
		expect(header!.getAttribute('aria-expanded')).toBe('false');
		await new Promise((r) => setTimeout(r, 50));
		expect(rankMock).not.toHaveBeenCalled();

		// Expanding reveals the rows — and ONLY NOW ranks them.
		await fireEvent.click(header!);
		await tick();
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		const ids = rankMock.mock.calls.flatMap((c) => (c[0] as { topic_id: string }[]).map((r) => r.topic_id));
		expect(ids).toContain('d1');
		expect(ids).toContain('d2');
	});

	it('an OPEN group (joined under it) still ranks its unjoined rows at paint time — the W1/W2 contracts hold', async () => {
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		paintMock.mockResolvedValue([disc('d1', 'video/retro')]);
		rankMock.mockResolvedValue([]);

		const { container } = render(TopicsPage);
		await waitFor(() => expect(container.textContent).toContain('retro')); // group open, row drawn
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		expect(rankMock.mock.calls.flatMap((c) => (c[0] as { topic_id: string }[]).map((r) => r.topic_id))).toContain('d1');
	});
});

// ── Finding 5 — keyboard focus fetches the bio (parity with mouseenter) ───────────────────────
describe('review 5 — focusing a roster row fetches the bio', () => {
	it('Tab-focus (no mouse) fires pasteKey and renders the bio', async () => {
		await seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		pasteKeyMock.mockResolvedValue({
			npub: STRANGER_NPUB,
			profile: { display_name: 'Stranger', bio: 'keyboard bio', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: false, last_fetched: '',
		});
		const { container, findByText } = render(TopicsPage);

		await waitFor(() => expect(container.querySelector('.topic-row')).not.toBeNull());
		await fireEvent.click(container.querySelector<HTMLButtonElement>('.topic-row')!);
		await waitFor(() => expect(rosterMock).toHaveBeenCalled());
		await tick();

		const row = container.querySelector<HTMLButtonElement>('.roster-row:not(.self)')!;
		expect(row).toBeTruthy();
		row.focus();
		await fireEvent.focus(row); // jsdom does not dispatch focus from .focus() alone
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER_NPUB));
		expect(await findByText('keyboard bio')).toBeTruthy();
	});
});

// ── Finding 6 — a rejected bio resolve is retry-later, never cached-absent ────────────────────
describe('review 6 — a rejected bio fetch does not poison the cache', () => {
	it('a REJECTED resolve does NOT render the absent line; a later hover RETRIES (second pasteKey)', async () => {
		await seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		pasteKeyMock.mockRejectedValueOnce(new Error('relay unreachable')).mockResolvedValue({
			npub: STRANGER_NPUB,
			profile: { display_name: 'Stranger', bio: 'recovered bio', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: false, last_fetched: '',
		});
		const { container, findByText } = render(TopicsPage);

		await waitFor(() => expect(container.querySelector('.topic-row')).not.toBeNull());
		await fireEvent.click(container.querySelector<HTMLButtonElement>('.topic-row')!);
		await waitFor(() => expect(rosterMock).toHaveBeenCalled());
		await tick();

		const row = container.querySelector<HTMLButtonElement>('.roster-row:not(.self)')!;
		// Hover 1: the relay is unreachable. The honest absent line must NOT appear — "couldn't
		// ask" is not "asked and there is none" — and nothing is cached as final.
		await fireEvent.mouseEnter(row);
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));
		expect(container.querySelector('.roster-bio')).toBeNull();

		// Hover 2 (relay back): the retry fires and the real bio lands.
		await fireEvent.mouseLeave(row);
		await fireEvent.mouseEnter(row);
		expect(await findByText('recovered bio')).toBeTruthy();
		expect(pasteKeyMock).toHaveBeenCalledTimes(2);
	});

	it('a resolved empty-string bio is a real bio, never the "No published profile" line', async () => {
		await seedSelf();
		rosterMock.mockResolvedValue([STRANGER_NPUB]);
		listMock.mockResolvedValue([mine('t1', 'video/anime')]);
		pasteKeyMock.mockResolvedValue({
			npub: STRANGER_NPUB,
			profile: { display_name: 'Stranger', bio: '', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: false, last_fetched: '',
		});
		const { container } = render(TopicsPage);

		await waitFor(() => expect(container.querySelector('.topic-row')).not.toBeNull());
		await fireEvent.click(container.querySelector<HTMLButtonElement>('.topic-row')!);
		await waitFor(() => expect(rosterMock).toHaveBeenCalled());
		await tick();

		const row = container.querySelector<HTMLButtonElement>('.roster-row:not(.self)')!;
		await fireEvent.mouseEnter(row);
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledTimes(1));
		// The empty string resolves and is cached — a hover renders the bio REGION (no stated
		// nothing), the `false` line never fires for ''.
		await waitFor(() => expect(container.querySelector('.roster-bio')).not.toBeNull());
		expect(container.textContent).not.toContain('No published profile');
	});
});
