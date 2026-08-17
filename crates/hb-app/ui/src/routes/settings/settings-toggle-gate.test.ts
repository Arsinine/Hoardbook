// @vitest-environment jsdom
// M1 (settings review, QURATOR-93 twin) — the QURATOR-93 fix disabled the two Save buttons until
// settingsLoaded, but the four preference toggles (allow-DMs, snapshot auto-update, reconcile poll,
// discoverable) stayed enabled after a FAILED getSettings() and called saveSettings(fullSettings())
// built from the fallback `settings` object — one toggle click overwrote the user's real
// relays/privacy settings with defaults-shaped data.
//
// Fix: every settings mutator disables until settingsLoaded (belt-and-suspenders — the DOM disabled
// attribute AND a guard inside each handler, matching the existing relays/big-relay Save pattern).
//
// BEHAVIOURAL mount tests: assert on the toggle BUTTON's disabled property and on saveSettings call
// counts, never on prose. Each toggle is probed twice — once through the DOM `disabled` attribute
// (proves the template gate) and once by bypassing that attribute directly on the element before
// firing the click (proves the in-handler guard survives on its own, per the belt-and-suspenders
// requirement — a mutation that drops either half alone must red exactly one of the two checks).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

vi.mock('$lib/api.js', () => ({
	generateKeypair: vi.fn(),
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

const TOGGLE_LABELS = [
	/allow incoming messages from anyone/i,
	/auto-update snapshots on change/i,
	/reconcile poll for remotely-edited collections/i,
	/show up in discover hoarders/i,
];

const OK_SETTINGS = {
	relay_urls: ['wss://relay.example.com'],
	allow_dms: true,
	privacy_notice_acknowledged: true,
	last_seen_version: '',
	snapshot_auto_update: true,
	snapshot_reconcile_poll: false,
	show_online_count: true,
	discoverable: false,
	big_relay_url: '',
};

describe('M1 — settings toggles are gated by settingsLoaded, like the Save buttons', () => {
	it('load REJECTS → every toggle is disabled, and none dispatch a save even bypassing the DOM gate', async () => {
		getSettingsMock.mockRejectedValue(new Error('settings.json unreadable'));
		primeIdentity();

		const { getByRole } = render(SettingsPage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());

		for (const label of TOGGLE_LABELS) {
			const btn = getByRole('button', { name: label }) as HTMLButtonElement;
			// Template-level gate: the DOM disabled attribute mirrors the Save-button pattern.
			expect(btn.disabled, `${label} should be disabled while settings failed to load`).toBe(true);

			// Handler-level gate: bypass the DOM attribute directly (as a mutation removing only the
			// template `disabled={!settingsLoaded}` would) and prove the in-handler guard alone still
			// blocks the save.
			btn.disabled = false;
			await fireEvent.click(btn);
			await tick();
		}
		expect(saveSettingsMock).not.toHaveBeenCalled();
	});

	it('Retry: reject → resolve → every toggle re-enables', async () => {
		getSettingsMock
			.mockRejectedValueOnce(new Error('first attempt fails'))
			.mockResolvedValue(OK_SETTINGS);
		primeIdentity();

		const { getByRole, queryByRole } = render(SettingsPage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		await waitFor(() => expect(queryByRole('alert')).toBeNull());
		for (const label of TOGGLE_LABELS) {
			const btn = getByRole('button', { name: label }) as HTMLButtonElement;
			await waitFor(() => expect(btn.disabled).toBe(false));
		}
	});

	it('a SUCCESSFUL load leaves every toggle enabled, and flipping one now DOES call saveSettings', async () => {
		getSettingsMock.mockResolvedValue(OK_SETTINGS);
		primeIdentity();

		const { getByRole } = render(SettingsPage);
		const allowDms = getByRole('button', { name: TOGGLE_LABELS[0] }) as HTMLButtonElement;
		// The button exists immediately (loading state), but stays disabled until settingsLoaded
		// actually flips — wait for THAT, not just presence.
		await waitFor(() => expect(allowDms.disabled).toBe(false));

		await fireEvent.click(allowDms);
		await tick();
		await waitFor(() => expect(saveSettingsMock).toHaveBeenCalledTimes(1));
	});
});
