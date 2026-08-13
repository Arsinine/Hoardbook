// @vitest-environment jsdom
// QURATOR-83 — a successful-but-EMPTY Discover result was cached for the entire app session.
// `rootTopics[root] === []` is truthy-ish under the old `!== undefined` check, so `toggleRoot`
// hit `return` on every subsequent expand of that root. Creating a Topic in that root did not
// invalidate the cache. The only escape was an app restart.
//
// This is a BEHAVIOURAL test (a real mount + click drive), not a source-scan: the bug is a control-
// flow sequence (expand → empty → publish → expand again → should re-fetch), and the only honest
// way to pin a sequence is to run it and assert on the mocked `topicDiscover` CALL COUNT between
// steps. A test that only asserted the final rendered list would pass on the broken code (the
// second expand reads the stale `[]` and renders "No public Topics" — indistinguishable from a
// genuine empty at the DOM level). The call count is the discriminator.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (revert the fix, re-run this file only) is documented in the task brief.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

// Mock the api module — every topic_* Tauri command is stubbed. topicDiscover is the spy under
// test; the others just need to resolve so the page's onMount and create flow don't throw.
vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({
		topic_id: 't-new',
		name: 'video/animation/anime',
		description: '',
		tags: [],
		private: false,
		joined_at: 0,
	}),
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

describe('QURATOR-83 — empty Discover cache is not terminal', () => {
	// ⚠ WHICH TEST PINS WHICH HALF — established by mutating each half SEPARATELY, because reverting
	// both at once cannot attribute a failure. The fix has two independent parts:
	//   (1) toggleRoot treats a cached EMPTY as a miss;  (2) create() evicts its root's cache key.
	// Verified by mutation:
	//   • Revert (1) only  -> THIS test reds ("expected 2 times, got 1"). The publish test below and
	//     the non-empty test stayed GREEN, so neither of them pins (1).
	//   • Revert (2) only  -> all three stayed GREEN, because (1) alone already re-fetches an empty.
	//     (2) only earns its place when the cache holds a NON-EMPTY list and a new Topic joins that
	//     root — which is what the last test here covers, and nothing else did.
	// This first test is also the owner's actual sequence: they collapsed, re-expanded, edited the
	// description and deleted, all with no create in between, and saw nothing every time.
	it('expand -> empty -> collapse -> re-expand with NO publish -> A SECOND FETCH HAPPENS', async () => {
		discoverMock.mockResolvedValue([]);
		const { getByRole } = render(TopicsPage);

		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();

		// First expand: fetch fires, relay legitimately answers "none".
		await clickRoot(getByRole, 'video');
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(1));

		// Collapse, then re-expand. Nothing else happens in between — no create, no edit.
		await clickRoot(getByRole, 'video');
		await tick();
		await clickRoot(getByRole, 'video');

		// A cached EMPTY must not be terminal: asking again must actually ask again. On the broken
		// code this stays at 1 forever and only an app restart clears it.
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(2), { timeout: 2000 });
		expect(discoverMock).toHaveBeenLastCalledWith(['video']);
	});

	it('expand -> empty -> publish -> expand again -> A SECOND FETCH HAPPENS', async () => {
		// Step 1: first expand returns an empty list.
		discoverMock.mockResolvedValueOnce([]);
		const { getByRole, getByPlaceholderText, getAllByRole } = render(TopicsPage);
		// Let onMount's topicList resolve.
		await waitFor(() => expect(discoverMock).not.toHaveBeenCalled());

		// Switch to the Discover tab.
		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();

		// Expand `video` — first fetch fires, returns [].
		await clickRoot(getByRole, 'video');
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(1));
		expect(discoverMock).toHaveBeenLastCalledWith(['video']);

		// Collapse video.
		await clickRoot(getByRole, 'video');
		await tick();

		// Step 2: create a public Topic under video/. This must invalidate the cached empty.
		// Open the Create modal and fill the form with a public path under `video`.
		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		// The root picker defaults to TOPIC_ROOTS[0] = 'video'; type the sub-path.
		const subPathInput = getByPlaceholderText(/sub-path/i);
		await fireEvent.input(subPathInput, { target: { value: 'animation/anime' } });
		await tick();
		// Submit via the Create button.
		const createBtn = getAllByRole('button').find(
			(b) => (b.textContent ?? '').trim().toLowerCase() === 'create',
		) as HTMLButtonElement;
		expect(createBtn).toBeTruthy();
		await fireEvent.click(createBtn);
		await tick();
		// Let the create() async flow (topicCreate + loadMine) resolve.
		await waitFor(() => expect(getByRole('button', { name: /discover/i })).toBeTruthy());

		// Switch back to Discover and re-expand video.
		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();
		await clickRoot(getByRole, 'video');

		// THE ASSERTION THAT MATTERS: a SECOND fetch must have happened after the publish invalidated
		// the cached empty. On the broken code this stays at 1 (the stale `[]` short-circuits).
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(2), { timeout: 2000 });
		expect(discoverMock).toHaveBeenLastCalledWith(['video']);
	});

	it('expand -> NON-EMPTY list -> collapse -> expand again -> NO second fetch', async () => {
		// Counterpart: the fix must not become "always re-fetch". A cached NON-EMPTY result stays
		// cached across a collapse+re-expand (that is why the cache exists — switching roots must not
		// be a relay round-trip each time).
		const hit = { topic_id: 't1', name: 'video/existing', description: '', tags: ['video'], member_count_estimate: 3 };
		discoverMock.mockResolvedValue([hit]);
		const { getByRole } = render(TopicsPage);
		await waitFor(() => expect(discoverMock).not.toHaveBeenCalled());

		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();

		// Expand video — fetch fires once, returns non-empty.
		await clickRoot(getByRole, 'video');
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(1));

		// Collapse + re-expand.
		await clickRoot(getByRole, 'video');
		await tick();
		await clickRoot(getByRole, 'video');
		await tick();
		// Allow any microtasks to settle, then assert NO second fetch occurred.
		await new Promise((r) => setTimeout(r, 50));

		expect(discoverMock).toHaveBeenCalledTimes(1);
	});

	// The ONLY test that pins the create() eviction. The empty-cache fix cannot cover this case: here
	// the cache holds a NON-EMPTY list, so `cached.length > 0` is true and toggleRoot would return
	// early forever. Without the eviction, publishing into a root you have already browsed leaves your
	// own new Topic invisible in Discover until the app restarts — the same defect class as
	// QURATOR-83, just one branch over.
	it('expand -> NON-EMPTY -> publish into that root -> re-expand -> A SECOND FETCH HAPPENS', async () => {
		const existing = { topic_id: 't1', name: 'video/existing', description: '', tags: ['video'], member_count_estimate: 3 };
		discoverMock.mockResolvedValue([existing]);
		const { getByRole, getByPlaceholderText, getAllByRole } = render(TopicsPage);

		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();

		// Browse video — it has topics, so the list is legitimately cached.
		await clickRoot(getByRole, 'video');
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(1));
		await clickRoot(getByRole, 'video'); // collapse
		await tick();

		// Now publish a new public Topic under the SAME root.
		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		await fireEvent.input(getByPlaceholderText(/sub-path/i), { target: { value: 'animation/anime' } });
		await tick();
		const createBtn = getAllByRole('button').find(
			(b) => (b.textContent ?? '').trim().toLowerCase() === 'create',
		) as HTMLButtonElement;
		await fireEvent.click(createBtn);
		await waitFor(() => expect(getByRole('button', { name: /discover/i })).toBeTruthy());

		await fireEvent.click(getByRole('button', { name: /discover/i }));
		await tick();
		await clickRoot(getByRole, 'video');

		// The cached non-empty list is now stale by exactly one Topic — the user's own. Asking again
		// must actually ask. On code without the eviction this stays at 1.
		await waitFor(() => expect(discoverMock).toHaveBeenCalledTimes(2), { timeout: 2000 });
	});
});
