// @vitest-environment jsdom
// QURATOR-148 — Topic aliveness gates discovery-sidebar visibility (owner ruling 2026-08-31:
// "whether or not it's worth joining, meaning whether or not it shows up as an option in the
// discovery sidebar on the left"). Aliveness = some roster member pinged within the last 30 days,
// read off the presence beacon the app already publishes; it arrives on the EXISTING lazy rank
// call (`topicRank`) as `alive_count` (null = unknown). A KNOWN zero drops the row; unknown keeps
// it — the same never-a-confident-zero rule the member count obeys.
//
// SCOPE NOTE (updated 2026-09-01 — the earlier "gate is inert in production" caveat no longer
// holds): non-member key recovery landed (`recover_public_topic_key` via the name-derived
// public-join credential, commands/topics.rs), so an unjoined public row CAN come back with a
// known `alive_count: 0` in production. These tests still mock `$lib/api.js`, so what they prove
// is the UI half — a 0 drops the row, null keeps it, the honest empty state — not the backend
// read; that half (the .authors() bound, the 30-day boundary, the recovery gate) is pinned in
// hb-net/src/count.rs and commands/topics.rs.
//
// Behavioural mount tests (render(Page) + fireEvent, mocking ONLY $lib/api.js — the established
// pattern; topics-q83 is the proof the page mounts, topics-w1/w2 the sidebar idiom). NOT a
// source-scan: the row's absence from the rendered directory is asserted against the DOM.
//
// jsdom computes no layout — nothing here proves a row renders as one line; the presence/absence of
// a row's label in the list pane is structural DOM and IS covered.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { topicAnnounceSummaries, announceSeen, topicDirectoryCache } from '$lib/stores.js';

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
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const rankMock = topicRank as unknown as ReturnType<typeof vi.fn>;

function disc(topic_id: string, name: string, description = '') {
	return { topic_id, name, description, tags: [name.split('/')[0]], member_count_estimate: null };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
	topicDirectoryCache.set([]);
});

/** Open a root group (pure-discovery groups start collapsed) and return the container. */
async function mounted() {
	const r = render(TopicsPage);
	await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
	return r;
}

describe('QURATOR-148 — aliveness gates the discovery sidebar', () => {
	it('a Topic with a member alive in 30 days stays; one with none drops out of the directory', async () => {
		listMock.mockResolvedValue([]);
		paintMock.mockResolvedValue([disc('live', 'video/live-room'), disc('dead', 'video/dead-room')]);
		rankMock.mockResolvedValue([
			{ topic_id: 'live', member_count_estimate: 3, alive_count: 1 },
			{ topic_id: 'dead', member_count_estimate: 9, alive_count: 0 },
		]);
		const { container } = await mounted();

		// Both rows paint first (aliveness is lazy, behind the rank call)…
		const video = await waitFor(() => {
			const h = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((el) =>
				(el.textContent ?? '').includes('video'),
			);
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(video);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('live-room'));
		expect(container.textContent).toContain('dead-room');

		// …then the rank answer lands and the DEAD row leaves the directory — the ruling's exact
		// behaviour: not-alive ⇒ not an option in the left pane. The live row stays.
		await waitFor(() => expect(container.textContent).not.toContain('dead-room'));
		expect(container.textContent).toContain('live-room');
	});

	it('aliveness unknown (null) keeps the row — never a confident drop', async () => {
		paintMock.mockResolvedValue([disc('maybe', 'video/maybe-room')]);
		// alive_count: null — no key / the read failed. The row must survive.
		rankMock.mockResolvedValue([{ topic_id: 'maybe', member_count_estimate: 2, alive_count: null }]);
		const { container } = await mounted();

		const video = await waitFor(() => {
			const h = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((el) =>
				(el.textContent ?? '').includes('video'),
			);
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(video);
		await tick();
		await waitFor(() => expect(rankMock).toHaveBeenCalled());
		await waitFor(() => expect(container.textContent).toContain('maybe-room'));
		// Give any (wrong) asynchronous drop a chance to land, then re-assert presence.
		await new Promise((r) => setTimeout(r, 50));
		expect(container.textContent).toContain('maybe-room');
	});

	it('a paint where every public Topic is dead states it honestly — never a bare "nothing here yet"', async () => {
		paintMock.mockResolvedValue([disc('gone', 'video/gone-room')]);
		rankMock.mockResolvedValue([{ topic_id: 'gone', member_count_estimate: 5, alive_count: 0 }]);
		const { container } = await mounted();

		// Open the group first: a collapsed group's rows are deliberately never ranked (the
		// relay-citizenship bound — a queued-but-undrawn row is a read spent on nobody), so the
		// aliveness fold only ever runs for rows that were actually drawn.
		const video = await waitFor(() => {
			const h = [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((el) =>
				(el.textContent ?? '').includes('video'),
			);
			expect(h).toBeTruthy();
			return h!;
		});
		await fireEvent.click(video);
		await tick();
		await waitFor(() => expect(container.textContent).toContain('gone-room'));
		// The rank answer lands, the dead row leaves, the tree empties…
		await waitFor(() => expect(container.textContent).not.toContain('gone-room'));
		// …and the honest empty replaces it: the directory WAS read and everything in it went
		// quiet — distinct copy from the confident "Nothing here yet" negative (the QURATOR-80/93
		// rule at Topic scale).
		await waitFor(() =>
			expect(container.textContent).toContain('No live Topics right now'),
		);
		expect(container.textContent).not.toContain('Nothing here yet');
	});
});
