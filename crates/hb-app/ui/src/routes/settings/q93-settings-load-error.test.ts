// @vitest-environment jsdom
// QURATOR-93 (Settings half) — a FAILED `getSettings` used to fall through to "proceed with
// defaults": an EMPTY relay editor (DEFAULT_RELAYS rows) with a LIVE Save. One click there persists
// a defaults-shaped settings object over the user's real one — the worst case of the confident-
// empty family, because the empty state was editable and armed.
//
// BEHAVIOURAL mount tests: assert on the affordances (role=alert, the Save BUTTON's disabled
// property, the Retry button), never on the word "retry" appearing in prose.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

vi.mock('$lib/api.js', () => ({
	generateKeypair: vi.fn(),
	// The load under test: individual tests flip resolve/reject.
	getSettings: vi.fn(),
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
	dmBlockedList: vi.fn().mockResolvedValue([]),
	dmUnblock: vi.fn().mockResolvedValue(undefined),
}));

import { getSettings, saveSettings } from '$lib/api.js';
const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const saveSettingsMock = saveSettings as unknown as ReturnType<typeof vi.fn>;

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

/** The relays "Save" button — the control that used to be live on a defaults editor. There are two
 *  Save buttons on the page (relays + big relay); scope by the relay-add-row that owns it. */
function relaySaveButton(container: HTMLElement): HTMLButtonElement | null {
	for (const row of container.querySelectorAll('.relay-add-row')) {
		const btn = row.querySelector('button.btn-primary');
		// The relays row is the one with a "wss://" placeholder input.
		if (row.querySelector('input[placeholder^="wss://"]')) return btn as HTMLButtonElement;
	}
	return null;
}

describe('QURATOR-93 — Settings load failure disables Save and offers Retry', () => {
	it('load REJECTS → error alert renders and the relay Save button is DISABLED', async () => {
		getSettingsMock.mockRejectedValue(new Error('settings.json unreadable'));
		primeIdentity();

		const { getByRole, container } = render(SettingsPage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		const save = relaySaveButton(container);
		expect(save, 'the relays Save button exists').toBeTruthy();
		// THE core assertion: the empty-editor-with-live-Save case is dead.
		expect(save!.disabled).toBe(true);
	});

	it('Retry: reject → resolve → Save ENABLES and the error is gone', async () => {
		getSettingsMock
			.mockRejectedValueOnce(new Error('first attempt fails'))
			.mockResolvedValue({ relay_urls: ['wss://relay.example.com'], allow_dms: true, big_relay_url: '' });
		primeIdentity();

		const { getByRole, queryByRole, container } = render(SettingsPage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());
		expect(relaySaveButton(container)!.disabled).toBe(true);

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		// The real relays render AND the Save re-arms — the both-directions rule.
		await waitFor(() => expect(container.textContent).toContain('wss://relay.example.com'));
		await waitFor(() => expect(relaySaveButton(container)!.disabled).toBe(false));
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});

	it('a SUCCESSFUL load leaves Save enabled with no error', async () => {
		getSettingsMock.mockResolvedValue({ relay_urls: ['wss://ok.example.com'], allow_dms: true, big_relay_url: '' });
		primeIdentity();

		const { queryByRole, container } = render(SettingsPage);
		await waitFor(() => expect(container.textContent).toContain('wss://ok.example.com'));
		expect(queryByRole('alert')).toBeNull();
		expect(relaySaveButton(container)!.disabled).toBe(false);
	});

	it('Save is not clickable while disabled (saveSettings never fires on a failed load)', async () => {
		getSettingsMock.mockRejectedValue(new Error('still failing'));
		primeIdentity();

		const { getByRole, container } = render(SettingsPage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		const save = relaySaveButton(container)!;
		// A disabled button ignores clicks — prove saveSettings stayed cold.
		await fireEvent.click(save);
		await tick();
		expect(saveSettingsMock).not.toHaveBeenCalled();
	});
});
