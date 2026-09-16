// @vitest-environment jsdom
// QURATOR-271 — a stale debounce timer leaks an intended-private Topic name to relays.
//
// The compose form's lookup effect ran its `if (newPrivate || !name) return;` early-return BEFORE
// it cancelled the pending debounce timer, so a lookup scheduled while the Topic was still Public
// survived the switch to Private and fired anyway — sending the now-intended-private name to
// relays via topicLookup. The fix moves the cancel above the early return.
//
// ⚠ The pin below is on topicLookup NOT BEING CALLED — the request never being SENT. The
// generation guard (lookupGeneration) already prevents a landed stale result from being APPLIED,
// so a test asserting only "no stale result displayed" would pass WITHOUT the fix and be vacuous.
// Assert on the mock itself.
//
// P-10 mutation (must red the first test, leave the second green): in `+page.svelte`, inside the
// compose-form lookup $effect — the effect that reads `composedName` and owns `lookupTimer`,
// lines ~270-296 — move the `clearTimeout(lookupTimer);` statement from just ABOVE the
// `if (newPrivate || !name) {` early return to just BELOW that early-return block (immediately
// before `lookupTimer = setTimeout(`), restoring the pre-fix ordering.
//
// Timers: fake timers are safe with this harness — @testing-library/svelte v5's asyncWrapper is
// `act` -> `await Svelte.tick()`, microtask-based (no setTimeout), and vitest does not fake
// microtasks by default. No page test here used fake timers before; the mounted-page harness is
// the established part and is unchanged.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

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

import { topicLookup } from '$lib/api.js';
const lookupMock = topicLookup as unknown as ReturnType<typeof vi.fn>;

const DEBOUNCE_MS = 300;

async function openCreateModal() {
	const utils = render(TopicsPage);
	await fireEvent.click(utils.getByRole('button', { name: /\+ new topic/i }));
	await tick();
	return utils;
}

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.clearAllMocks();
});

describe('QURATOR-271 — switching to Private cancels a pending lookup (the request is never sent)', () => {
	it('toggling Private with a pending public-name lookup timer: topicLookup is never called', async () => {
		vi.useFakeTimers();
		const { getByPlaceholderText, container } = await openCreateModal();

		// A PUBLIC name first — this is what schedules the debounce timer.
		await fireEvent.input(getByPlaceholderText(/sub-path/i), { target: { value: 'back room' } });
		await tick();
		expect(lookupMock).not.toHaveBeenCalled(); // still inside the debounce window

		// Before the timer fires, switch the Topic to Private.
		const check = container.querySelector<HTMLInputElement>('input[type="checkbox"]');
		expect(check).toBeTruthy();
		await fireEvent.click(check!);
		await tick();

		// The whole debounce window (and far beyond) elapses.
		vi.advanceTimersByTime(DEBOUNCE_MS * 3);

		// THE pin: the lookup request was never SENT. Without the fix the pre-Private timer
		// survives the early return and fires here — the generation guard only stops its
		// RESULT being applied, it does not stop the relay round-trip.
		expect(lookupMock).not.toHaveBeenCalled();
	});
});

describe('QURATOR-271 — the fix does not break the normal public debounce', () => {
	it('a public name still yields exactly one lookup after the delay — not zero, not one per keystroke', async () => {
		vi.useFakeTimers();
		const { getByPlaceholderText } = await openCreateModal();
		const sub = getByPlaceholderText(/sub-path/i);

		// Two "keystrokes" — each restarts the debounce; only the final name may be looked up.
		await fireEvent.input(sub, { target: { value: 'back' } });
		await tick();
		vi.advanceTimersByTime(DEBOUNCE_MS - 100);
		await fireEvent.input(sub, { target: { value: 'back room' } });
		await tick();

		// Not before the full window since the LAST input…
		vi.advanceTimersByTime(DEBOUNCE_MS - 1);
		expect(lookupMock).not.toHaveBeenCalled();
		// …and exactly once at the deadline, with the final composed name.
		vi.advanceTimersByTime(1);
		expect(lookupMock).toHaveBeenCalledTimes(1);
		expect(lookupMock).toHaveBeenCalledWith(expect.stringContaining('back room'));
	});
});
