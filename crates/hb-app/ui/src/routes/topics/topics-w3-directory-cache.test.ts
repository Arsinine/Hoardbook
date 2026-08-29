// @vitest-environment jsdom
// QURATOR-145 (Topics W3) — the cross-mount directory cache: paint the cached tree instantly on
// open, fetch behind it, replace on arrival. Only a NON-EMPTY successful answer is ever written
// back, so an empty or failed result never poisons the next open — the 52ca0b2 bug (a
// session-cached empty Discover root) caused the owner's original "Discover finds nothing" report
// (QURATOR-80), and W2's auto-population would recreate it at ten times the visibility.
//
// The regression pins the SEQUENCE, not the state (QURATOR-83's own gate, carried across mounts):
// a cached `[]` and a genuine empty are identical at the DOM level, so the mocked CALL COUNT
// between mounts is the only honest discriminator. Each test mounts, unmounts, and mounts AGAIN.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The probes:
//   1. SEQUENCE test — let a cached POPULATED tree suppress the background fetch (the rejected
//      "cache-until-restart" shortcut: skip paintDirectory when the cache painted). The third
//      mount's call count stays at 2 and the test REDS.
//   2. EMPTY-NEVER-CACHED test — drop the `uniq.length > 0` guard so the cache stores an empty
//      answer. Mount 2's fresh empty evicts the last-known-good tree, mount 3's landing screen
//      goes blank while the fetch is still pending, and the test REDS.
//   3. DEGRADE-TO-LAST-KNOWN test — make the catch blank the screen (`directory = []`). The
//      cached rows vanish at the failure and the test REDS.
//
// jsdom computes no layout — nothing here proves the landing screen paints in one frame or that
// the stale tree is visually seamless; only that the rows are in the DOM and the fetch sequence
// is right.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';
import { topicDirectoryCache } from '$lib/stores.js';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({ topic_id: 't-new', name: '', description: '', tags: [], private: false, joined_at: 0 }),
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

// The cache is a module-level store — it survives unmount by design, so it must be reset between
// tests or every test after the first inherits the previous one's tree (the w2 file resets the
// announce stores for the same reason).
afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	topicDirectoryCache.set([]);
});

const published = {
	topic_id: 't1',
	name: 'video/animation/anime',
	description: 'a topic',
	tags: ['video'],
	member_count_estimate: 2,
};

/** The video root-group header (the group is PURE DISCOVERY here — nothing joined — so it starts
 *  COLLAPSED; open it before expecting a row inside). */
function videoHeader(container: HTMLElement) {
	return [...container.querySelectorAll<HTMLButtonElement>('.root-header')].find((h) =>
		h.textContent?.includes('video'),
	) ?? null;
}

describe('QURATOR-145 (W3) — the cross-mount directory cache', () => {
	it('SEQUENCE: open -> EMPTY paint -> remount -> a SECOND fetch fires, and a cached tree never suppresses the third', async () => {
		paintMock.mockResolvedValueOnce([]).mockResolvedValue([published]);

		// Mount 1: the relays answer EMPTY. The cache must NOT record it — there is nothing to
		// paint instantly next time, and a cached nothing is the 52ca0b2 bug.
		const first = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		first.unmount();

		// Mount 2: THE assertion — a second fetch actually happens. On broken code (an empty was
		// cached and served without re-asking) this stays at 1 and the screen "still looks empty",
		// which a final-render assertion could never distinguish.
		const second = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2), { timeout: 2000 });
		const header = videoHeader(second.container);
		expect(header).toBeTruthy();
		await fireEvent.click(header!);
		await tick();
		expect(second.getByText('animation/anime')).toBeTruthy();
		second.unmount();

		// Mount 3: the cache is now POPULATED and paints instantly — but the refresh must STILL
		// fire behind it (the "cache-until-restart + Refresh button" alternative was rejected; a
		// stale answer is the landing screen, never the answer). This is the hop the
		// skip-fetch-when-cached mutation kills.
		const third = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(3), { timeout: 2000 });
	});

	it('an EMPTY answer is never written: the last-known-good tree survives as the next landing screen', async () => {
		// Mount 1: a populated answer — cached.
		paintMock.mockResolvedValueOnce([published]);
		const a = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		a.unmount();

		// Mount 2: the fresh answer is EMPTY. The SCREEN takes it (fresh truth replaces stale
		// data, even to nothing), but the CACHE must not — an empty never evicts the last
		// known-good tree.
		paintMock.mockResolvedValueOnce([]);
		const b = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2));
		expect(videoHeader(b.container)).toBeNull(); // the honest fresh empty: no groups at all
		b.unmount();

		// Mount 3: the fetch NEVER RESOLVES, so the ONLY thing that can be on screen is the
		// cache. If mount 2's empty had been written back, this landing screen is blank —
		// indistinguishable-from-broken, the exact shape the owner ruled not negotiable.
		let release: ((v: unknown) => void) | undefined;
		paintMock.mockImplementationOnce(() => new Promise((res) => { release = res; }));
		const c = render(TopicsPage);
		await waitFor(() => expect(videoHeader(c.container)).not.toBeNull());
		await fireEvent.click(videoHeader(c.container)!);
		await tick();
		expect(c.getByText('animation/anime')).toBeTruthy(); // cached tree, painted before any answer
		release!([published]); // settle the in-flight paint so the mount ends clean
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(3));
	});

	it('a FAILED fetch degrades to last-known: the cached tree stays on screen, never a confident "no Topics"', async () => {
		// Mount 1: a populated answer — cached and on screen.
		paintMock.mockResolvedValueOnce([published]);
		const a = render(TopicsPage);
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		const headerA = videoHeader(a.container);
		expect(headerA).toBeTruthy();
		await fireEvent.click(headerA!);
		await tick();
		expect(a.getByText('animation/anime')).toBeTruthy();
		a.unmount();

		// Mount 2: the cache paints instantly, then the refresh FAILS.
		paintMock.mockRejectedValueOnce(new Error('relay down'));
		const b = render(TopicsPage);
		await waitFor(() => expect(videoHeader(b.container)).not.toBeNull());
		const headerB = videoHeader(b.container)!;
		await fireEvent.click(headerB);
		await tick();
		expect(b.getByText('animation/anime')).toBeTruthy(); // the instant landing from cache
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(2));
		await tick();

		// THE assertion: the failure did not blank what was on screen, and the retryable error
		// surface (the alert) did not replace the populated tree — the template's error branch
		// only renders over an EMPTY tree, and the catch never touches `directory`.
		expect(b.getByText('animation/anime')).toBeTruthy();
		expect(b.queryByRole('alert')).toBeNull();
		expect(b.queryByText(/Nothing here yet/i)).toBeNull();
	});
});
