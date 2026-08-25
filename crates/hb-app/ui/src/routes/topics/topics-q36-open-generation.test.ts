// @vitest-environment jsdom
// Finding #36 (CWE-362) — the open-Topic panel's async roster fetch had no generation guard: click
// Topic A then Topic B quickly, and if A's roster resolve lands after B's, B's pane shows A's member
// roster (over-disclosure to the wrong audience). This is a BEHAVIOURAL test (real mount + click
// drive, mocking only `$lib/api.js`), not a source-scan: the bug is a resolve-ordering race, and the
// only honest way to pin it is to hold A's fetch in flight, open B, then release A and assert the
// roster still reads B's — on the unguarded code the late resolve overwrites it with A's.
//
// Per CLAUDE.md §9, proven red by mutating the production half (dropping the `generation ===
// openGeneration` guard in `open()`) and re-running this file.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { contacts, identity, profile, topicAnnounceSummaries, announceSeen } from '$lib/stores.js';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscover: vi.fn(),
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

import { topicRoster, topicList } from '$lib/api.js';
const rosterMock = topicRoster as unknown as ReturnType<typeof vi.fn>;
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;

const NPUB_A = 'npub1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const NPUB_B1 = 'npub1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
const NPUB_B2 = 'npub1cccccccccccccccccccccccccccccccccccccccccc';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	// Reset the app-wide stores this suite seeds — a leaked value would bleed into any later test
	// in the same worker that reads them (the stores are module singletons).
	contacts.set([]);
	identity.set(null);
	profile.set(null);
	topicAnnounceSummaries.set([]);
	announceSeen.set({});
});

describe('finding #36 — open() roster fetch is generation-guarded', () => {
	it('a stale roster resolve from a previous Topic must not bind to the newly-open one', async () => {
		identity.set({
			npub: 'npub1selfselfselfselfselfselfselfselfselfselfselfse',
			npub_short: 'npub1sel…lfse',
			share_code: 'hbk-x',
			key_storage: 'plain-file',
		});
		profile.set({ display_name: 'Me', bio: undefined, tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' });

		// Topic A's roster hangs until we release it; Topic B's resolves immediately (2 members).
		let releaseA: (v: unknown) => void = () => {};
		const inFlightA = new Promise((r) => { releaseA = r; });
		rosterMock.mockReturnValueOnce(inFlightA).mockResolvedValue([NPUB_B1, NPUB_B2]);

		listMock.mockResolvedValue([
			{ topic_id: 'topic-A', name: 'video/anime', description: '', tags: [], private: false, joined_at: 0 },
			{ topic_id: 'topic-B', name: 'video/films', description: '', tags: [], private: false, joined_at: 0 },
		]);

		const { container, getByText } = render(TopicsPage);
		await waitFor(() => expect(container.querySelectorAll('.topic-row').length).toBe(2));

		const rows = container.querySelectorAll<HTMLButtonElement>('.topic-row');

		// Open A — its roster fetch goes out and hangs.
		await fireEvent.click(rows[0]);
		await waitFor(() => expect(rosterMock).toHaveBeenCalledWith('topic-A'));

		// Open B before A resolves — B's roster lands and is shown (2 members).
		await fireEvent.click(rows[1]);
		await waitFor(() => expect(getByText('Roster (2)')).toBeTruthy());

		// Now let A's stale roster land. On unguarded code this overwrites B's 2 with A's 1.
		releaseA([NPUB_A]);
		await tick();
		await new Promise((r) => setTimeout(r, 20));

		// B's pane must still show B's roster, not A's.
		expect(getByText('Roster (2)')).toBeTruthy();
		expect(container.querySelectorAll('.roster li').length).toBe(2);
	});
});
