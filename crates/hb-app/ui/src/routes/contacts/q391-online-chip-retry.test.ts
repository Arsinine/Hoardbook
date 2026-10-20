// @vitest-environment jsdom
// QURATOR-391 devtest item 5 — "In contacts, the X on network indicator does not update
// immediately when a user opens contacts but it will update if you switch tabs then come back."
//
// ROOT CAUSE (from the code): refreshOnline() polls ONCE on mount; if that first call rejects
// (backend/relays still coming up at app start) the catch keeps the old value — the chip stays
// red "– on network" — and the only thing scheduled next is the 20s ONLINE_POLL_VISIBLE_MS
// interval. A rejected poll never re-arms anything. Switching tabs "fixed" it only because
// leaving Contacts unmounts the page and returning re-polled on mount.
//
// FIX UNDER TEST: a bounded quick-retry chain (2s apart, max 3 per mount) armed only while the
// chip is still unknown. Steady state (chip answered) never arms it, so normal poll load is
// unchanged.
//
// MOUNTED (P-4) — the page mounts in jsdom; every mount-path api call is try/catch-wrapped in
// the page, and the real $lib/api.js is spread with only onlineCount/relayStatus overridden, so
// untouched calls reject benignly ("Tauri not available") exactly as they do outside Tauri.
//
// P-13 LIMIT: jsdom has no layout — this asserts the chip LABEL text, not pixels.
//
// P-10 MUTATION (seen red, exact edit): comment out the arming line
// `if (chip.unknown) scheduleOnlineRetry();` at the end of refreshOnline in
// routes/contacts/+page.svelte → the first test reds (onlineCount is still called once after
// 2.1s of fake time; the chip never reaches "3 on network"). Restore → green.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/api.js', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	onlineCount: vi.fn(),
	relayStatus: vi.fn(),
}));

import ContactsPage from './+page.svelte';
import { onlineCount, relayStatus } from '$lib/api.js';

const onlineCountMock = onlineCount as unknown as ReturnType<typeof vi.fn>;
const relayStatusMock = relayStatus as unknown as ReturnType<typeof vi.fn>;

const ANSWER = { online: 3, fixture: 'q391', fetched_at: '2026-10-20T00:00:00Z', fresh: [] };
const HEALTHY = [{ url: 'wss://relay.example', connected: true, status: 'connected' }];

beforeEach(() => {
	vi.useFakeTimers();
	onlineCountMock.mockReset();
	relayStatusMock.mockReset();
	relayStatusMock.mockResolvedValue(HEALTHY);
});

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.clearAllMocks();
});

async function flushMount() {
	// Let the mount-time poll's promise chain settle WITHOUT advancing the poll interval.
	await vi.advanceTimersByTimeAsync(0);
	await vi.advanceTimersByTimeAsync(0);
}

describe('QURATOR-391 item 5 — the online chip recovers without a tab switch', () => {
	it('re-arms quickly after a rejected first poll: the chip reaches the real count with no tab switch', async () => {
		onlineCountMock.mockRejectedValueOnce(new Error('backend not ready yet')).mockResolvedValue(ANSWER);
		const { container } = render(ContactsPage);
		await flushMount();
		expect(onlineCountMock).toHaveBeenCalledTimes(1);
		expect(container.textContent).toContain('– on network');
		await vi.advanceTimersByTimeAsync(2_100);
		await vi.advanceTimersByTimeAsync(0);
		expect(onlineCountMock).toHaveBeenCalledTimes(2);
		expect(container.textContent).toContain('3 on network');
	});

	it('steady state is unchanged: an answered chip never arms a quick retry (mount + 20s ticks only)', async () => {
		onlineCountMock.mockResolvedValue(ANSWER);
		const { container } = render(ContactsPage);
		await flushMount();
		expect(container.textContent).toContain('3 on network');
		expect(onlineCountMock).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(6_000);
		expect(onlineCountMock).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(15_000);
		expect(onlineCountMock).toHaveBeenCalledTimes(2);
		expect(container.textContent).toContain('3 on network');
	});

	it('a genuinely-down backend exhausts the 3-retry budget, then falls back to the 20s tick', async () => {
		onlineCountMock.mockRejectedValue(new Error('backend down'));
		const { container } = render(ContactsPage);
		await flushMount();
		expect(container.textContent).toContain('– on network');
		await vi.advanceTimersByTimeAsync(6_500);
		await vi.advanceTimersByTimeAsync(0);
		expect(onlineCountMock).toHaveBeenCalledTimes(4); // mount + 3 retries
		expect(container.textContent).toContain('– on network');
		await vi.advanceTimersByTimeAsync(14_000);
		await vi.advanceTimersByTimeAsync(0);
		expect(onlineCountMock).toHaveBeenCalledTimes(5); // + the 20s interval tick at t≈21s
	});
});
