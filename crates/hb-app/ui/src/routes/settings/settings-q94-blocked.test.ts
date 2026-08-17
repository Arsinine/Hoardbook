// @vitest-environment jsdom
// QURATOR-94 (2/2) — dmUnblock had no UI. A blocked npub was permanent (only inspectable in the
// store file). The Settings page now carries a "Blocked contacts" section near Preferences listing
// `dm_blockedList()` with a per-row Unblock that calls `dm_unblock` and refreshes the list.
//
// Behavioural mount for the row + unblock behaviour, PLUS an import-statement assertion: the W4
// lesson (a whole-file symbol grep is satisfied by the call site itself) — `dmUnblock` must appear
// IN the actual `import {...} from '$lib/api.js'` line, not merely somewhere in the file.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
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
	// The two halves under test:
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

/** shortNpub display shape (contact-display.ts): first 8 + ellipsis + last 4. */
function shortNpub(npub: string): string {
	return npub.length > 14 ? npub.slice(0, 8) + '…' + npub.slice(-4) : npub;
}

function sliceApiImportLine(src: string): string {
	const m = src.match(/import\s*\{[^}]*\}\s*from\s*'\$lib\/api\.js'/);
	expect(m, 'the settings page has an import statement from $lib/api.js').toBeTruthy();
	return m![0];
}

describe('QURATOR-94 — Settings blocked-contacts section', () => {
	it('dmUnblock is in the api import STATEMENT (the W4 lesson: not a whole-file symbol grep)', () => {
		const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '+page.svelte'), 'utf8');
		const importLine = sliceApiImportLine(src);
		expect(importLine).toContain('dmUnblock');
		expect(importLine).toContain('dmBlockedList');
	});

	it('renders one row per blocked npub, shortened like shortNpub', async () => {
		blockedListMock.mockResolvedValue([BLOCKED_A, BLOCKED_B]);
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		profile.set({ display_name: 'Me', tags: [], languages: [] });

		const { getByText, queryByText, getAllByText } = render(SettingsPage);

		// Wait on the ROW text, not the section label — the label renders immediately with
		// blocked === [] (onMount's loadBlocked is still in flight), so a label-only waitFor
		// races ahead of the rows. Waiting on the row also proves loadBlocked actually ran.
		await waitFor(() => expect(getByText(shortNpub(BLOCKED_A))).toBeTruthy());
		expect(getByText('Blocked contacts')).toBeTruthy();
		// Shortened display, never the full npub… (both rows land in one assignment)
		expect(getByText(shortNpub(BLOCKED_B))).toBeTruthy();
		expect(queryByText(BLOCKED_A)).toBeNull();
		// …and exactly one Unblock affordance per row (getAllByText: two rows = two buttons).
		await waitFor(() => expect(getAllByText('Unblock')).toHaveLength(2));
	});

	it('Unblock calls dm_unblock with the row npub and refreshes the list', async () => {
		blockedListMock.mockResolvedValueOnce([BLOCKED_A]).mockResolvedValueOnce([]);
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		profile.set({ display_name: 'Me', tags: [], languages: [] });

		const { getByText, getAllByText } = render(SettingsPage);

		// Wait for the row (not the always-present label) so the click can't race loadBlocked.
		await waitFor(() => expect(getByText(shortNpub(BLOCKED_A))).toBeTruthy());
		await fireEvent.click(getByText('Unblock'));
		await tick();

		await waitFor(() => expect(unblockMock).toHaveBeenCalledTimes(1));
		expect(unblockMock).toHaveBeenCalledWith(BLOCKED_A);
		// The list was re-read after the unblock (the refresh half — without it the row lingers).
		await waitFor(() => expect(blockedListMock).toHaveBeenCalledTimes(2));
		await waitFor(() => expect(() => getByText(shortNpub(BLOCKED_A))).toThrow());
	});

	it('an empty blocklist renders no section noise beyond the quiet empty line', async () => {
		blockedListMock.mockResolvedValue([]);
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		profile.set({ display_name: 'Me', tags: [], languages: [] });

		const { getByText, queryByText } = render(SettingsPage);
		// The section-label is present (stable layout) but shows the quiet empty line — no row, no error.
		await waitFor(() => expect(getByText('Blocked contacts')).toBeTruthy());
		await waitFor(() => expect(getByText(/no blocked contacts/i)).toBeTruthy());
		expect(queryByText('Unblock')).toBeNull();
	});
});
