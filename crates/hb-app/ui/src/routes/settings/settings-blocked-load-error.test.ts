// @vitest-environment jsdom
// minor-5 (settings review, QURATOR-93/QURATOR-80/85 twin) — ~:324-326 used to swallow a
// dmBlockedList() failure, and ~:803-804 then rendered the quiet "No blocked contacts" line —
// hiding an UNKNOWN blocklist (and every Unblock control) behind a confident negative.
//
// Fix: a blocked-list load-error state renders the shared EmptyState error variant (error, onretry)
// instead of the empty line. A success — first-try or retry — always clears the error (both
// directions), and a retry that fails AFTER a prior success keeps the stale rows rather than
// reverting to the quiet-empty or error state (never clear the list on failure).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const BLOCKED_A = 'npub1blockeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const BLOCKED_B = 'npub1blockeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeef';

vi.mock('$lib/api.js', () => ({
	generateKeypair: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ relay_urls: [], allow_dms: true }),
	saveSettings: vi.fn().mockResolvedValue(undefined),
	importNsec: vi.fn(),
	backupData: vi.fn(),
	peekBackup: vi.fn(),
	restoreData: vi.fn(),
	wipeData: vi.fn(),
	checkRelay: vi.fn().mockResolvedValue(undefined),
	relayStatus: vi.fn().mockResolvedValue([]),
	beaconStatus: vi.fn().mockResolvedValue(null),
	checkUpdate: vi.fn().mockResolvedValue(null),
	downloadUpdate: vi.fn(),
	applyStagedUpdate: vi.fn(),
	takeUpdateNotice: vi.fn().mockResolvedValue(null),
	updaterIsPortable: vi.fn().mockRejectedValue(new Error('not tauri')),
	checkPortableUpdate: vi.fn().mockResolvedValue(null),
	applyPortableUpdate: vi.fn(),
	hasPublishedProfile: vi.fn().mockResolvedValue(false),
	publishProfile: vi.fn(),
	copyDiagnostics: vi.fn(),
	revealLogFolder: vi.fn(),
	natClassification: vi.fn().mockResolvedValue('undetermined'),
	// The halves under test:
	dmBlockedList: vi.fn(),
	dmUnblock: vi.fn().mockResolvedValue(undefined),
}));

import { dmBlockedList, dmUnblock } from '$lib/api.js';
const blockedListMock = dmBlockedList as unknown as ReturnType<typeof vi.fn>;
const unblockMock = dmUnblock as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	profile.set(null);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

/** shortNpub display shape (contact-display.ts): first 8 + ellipsis + last 4. */
function shortNpub(npub: string): string {
	return npub.length > 14 ? npub.slice(0, 8) + '…' + npub.slice(-4) : npub;
}

describe('minor-5 — a failed blocked-list load renders an error state, not a confident empty', () => {
	it('dmBlockedList REJECTS → error EmptyState with Retry renders; the quiet empty is absent', async () => {
		blockedListMock.mockRejectedValue(new Error('local read failed'));
		primeIdentity();

		const { getByRole, queryByText } = render(SettingsPage);

		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		// THE core assertion: an unknown blocklist must never read as "No blocked contacts".
		expect(queryByText(/no blocked contacts/i)).toBeNull();
	});

	it('Retry: reject → resolve → rows render and the error is gone', async () => {
		blockedListMock
			.mockRejectedValueOnce(new Error('first attempt fails'))
			.mockResolvedValue([BLOCKED_A]);
		primeIdentity();

		const { getByRole, queryByRole, getByText } = render(SettingsPage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		await waitFor(() => expect(getByText(shortNpub(BLOCKED_A))).toBeTruthy());
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});

	it('a retry that fails AFTER a prior success keeps the stale rows (list never cleared on failure)', async () => {
		blockedListMock
			.mockResolvedValueOnce([BLOCKED_A, BLOCKED_B])
			.mockRejectedValueOnce(new Error('second read fails'));
		primeIdentity();

		const { getByText, getAllByText, queryByRole, queryByText } = render(SettingsPage);
		await waitFor(() => expect(getByText(shortNpub(BLOCKED_A))).toBeTruthy());

		// Unblock triggers handleUnblock → dmUnblock → loadBlocked() refresh, which is the second
		// dmBlockedList call primed to reject above. Two rows exist, so scope to the first.
		await fireEvent.click(getAllByText('Unblock')[0]);
		await tick();

		await waitFor(() => expect(unblockMock).toHaveBeenCalledTimes(1));
		await waitFor(() => expect(blockedListMock).toHaveBeenCalledTimes(2));

		// Stale rows survive the failed refresh — neither the error state nor the quiet empty.
		expect(getByText(shortNpub(BLOCKED_A))).toBeTruthy();
		expect(getByText(shortNpub(BLOCKED_B))).toBeTruthy();
		expect(queryByRole('alert')).toBeNull();
		expect(queryByText(/no blocked contacts/i)).toBeNull();
	});
});
