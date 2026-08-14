// @vitest-environment jsdom
// QURATOR-85 — a successful Discover fetch never cleared `erroredRoots`, so an overlapping
// FAILING request (A) could set the error AFTER a successful request (B) cached real topics.
// The template checks the error branch BEFORE the data branch, so a root that was both
// cached-non-empty AND errored rendered "Couldn't reach the relays" on top of a good list.
//
// This is a BEHAVIOURAL test (real mount + deferred-promise drive), not a source-scan: the bug
// is a resolve-ordering race, and only controlling the exact resolution sequence (A fails, THEN
// B succeeds) can reproduce it. The deferred-promise pattern is borrowed from QURATOR-83's
// in-flight test. The sequence contains NO publish/create (which would delete the cache key and
// make the buggy branch unreachable — the QURATOR-83 pitfall).
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The probe
// (revert the fix in toggleRoot, re-run this file only) MUST fail this test.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({ topic_id: 't-new', name: '', description: '', tags: [], private: false, joined_at: 0 }),
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

import { topicDiscover } from '$lib/api.js';
const discoverMock = topicDiscover as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

/** Click the root-header button for `root` (the accordion toggle that triggers the Discover fetch). */
async function clickRoot(getByRole: (role: string, opts?: Record<string, unknown>) => HTMLElement, root: string) {
	await fireEvent.click(getByRole('button', { name: new RegExp(`^\\s*${root}`, 'i') }));
	await tick();
}

describe('QURATOR-85 — a successful fetch clears a stale error on the same root', () => {
	it('A fails AFTER B starts -> B succeeds -> user sees B\'s results, not the error', async () => {
		// Two overlapping requests for `video`. A hangs and will REJECT; B hangs and will RESOLVE
		// with a real topic. We control the exact order: A fails first, then B succeeds.
		let rejectA: (e: Error) => void = () => {};
		let resolveB: (v: unknown) => void = () => {};
		const failingA = new Promise<never>((_, rej) => { rejectA = rej; });
		const succeedingB = new Promise<unknown>((res) => { resolveB = res; });
		const goodResult = { topic_id: 't1', name: 'video/animation/anime', description: 'a topic', tags: ['video'], member_count_estimate: 2 };
		discoverMock.mockReturnValueOnce(failingA).mockReturnValueOnce(succeedingB);

		const { getByRole, queryByText, findByText } = render(TopicsPage);

		// Switch to Discover.
		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();

		// Expand video — request A starts and hangs.
		await clickRoot(getByRole, 'video');
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(1));

		// Collapse + re-expand — request B starts and hangs. Both A and B are now in flight.
		await clickRoot(getByRole, 'video');
		await tick();
		await clickRoot(getByRole, 'video');
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(2));

		// Step 3: A FAILS. erroredRoots gets 'video'. The user would see the error if they looked
		// right now, but B is still in flight — the race is live.
		rejectA(new Error('relay down'));
		await tick();
		await new Promise((r) => setTimeout(r, 20));

		// Step 4: B SUCCEEDS. It caches real topics AND (with the fix) clears the stale error.
		resolveB([goodResult]);
		await tick();
		await new Promise((r) => setTimeout(r, 20));

		// THE ASSERTION: the user sees B's results, not the error message. On the broken code the
		// error is still set (A added it, B never cleared it) and the template checks error first,
		// so "Couldn't reach the relays" renders over a perfectly good cached list.
		expect(queryByText(/reach the relays/i)).toBeNull();
		expect(await findByText('animation/anime')).toBeTruthy();
	});
});
