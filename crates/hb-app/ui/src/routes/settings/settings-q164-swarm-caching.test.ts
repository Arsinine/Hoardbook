// @vitest-environment jsdom
// QURATOR-164 build item 4 — the one opt-in switch (discovery-triggered auto-fetch AND
// relay-caching, one switch per owner ruling 2026-09-04) plus its reciprocity disclosure.
//
// This file drives the REAL mounted Settings page through the real DOM — not a source scan
// (CLAUDE.md §7: source-scans are the documented origin of vacuous controls here). Two pins:
//   1. the switch exists, states the reciprocity plainly ("other people will request
//      collections from you too" — the honesty IS the design, not a footnote), and
//      toggling it actually reaches saveSettings with swarm_caching: true on the WHOLE object;
//   2. the toggle is disabled until the settings load succeeds — the QURATOR-93 defaults-editor
//      hazard, same guard every other toggle row carries.
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
	validateBackup: vi.fn(),
	wipeData: vi.fn(),
	clearManifestCache: vi.fn(),
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
	dmBlock: vi.fn(),
	validateShareCode: vi.fn(),
	shareCodeInfo: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn(), confirm: vi.fn() }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

import { getSettings, saveSettings } from '$lib/api.js';

const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const saveSettingsMock = saveSettings as unknown as ReturnType<typeof vi.fn>;

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
	swarm_caching: false,
	serving_notice_acknowledged: true,
};

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

/** Exact type of `render(SettingsPage)` — same svelte-check note as the q126 test. */
type Rendered = ReturnType<typeof renderSettings>;
const renderSettings = () => render(SettingsPage);

describe('QURATOR-164 — the swarm-caching opt-in switch and its reciprocity copy', () => {
	it('toggling the switch saves the WHOLE settings object with swarm_caching: true, and every other field survives', async () => {
		getSettingsMock.mockResolvedValue({ ...OK_SETTINGS });
		primeIdentity();
		const r = renderSettings();
		// The load is async — the button renders immediately but stays disabled until
		// `settingsLoaded` flips, so the enabled-state check needs its own waitFor.
		await waitFor(() => expect((r.getByRole('button', { name: 'Fetch new collections automatically' }) as HTMLButtonElement).disabled).toBe(false));

		await fireEvent.click(r.getByRole('button', { name: 'Fetch new collections automatically' }));
		await tick();

		await waitFor(() => expect(saveSettingsMock).toHaveBeenCalledTimes(1));
		const saved = saveSettingsMock.mock.calls[0][0];
		// MUTATION (P-10): in +page.svelte, in the `toggleSwarmCaching` handler body, change
		// `settings = { ...settings, swarm_caching: !settings.swarm_caching };` to
		// `settings = { ...settings };` (drop the flip) — this test reds: saveSettings still fires
		// (the click still runs) but `saved.swarm_caching` is false, not true.
		expect(saved.swarm_caching).toBe(true);
		// The whole object is what gets saved — a partial save would silently write `undefined`
		// over the persisted fields (the fullSettings() gotcha this page documents).
		expect(saved).toEqual({ ...OK_SETTINGS, swarm_caching: true });
		expect(saved.serving_notice_acknowledged).toBe(true);
		expect(saved.relay_urls).toEqual(['wss://relay.example.com']);
		expect(saved.big_relay_url).toBe('');
	});

	it('the reciprocity copy states plainly that other people will request from you too', async () => {
		getSettingsMock.mockResolvedValue({ ...OK_SETTINGS });
		primeIdentity();
		const r = renderSettings();
		await waitFor(() => expect(r.getByText('Fetch new collections automatically')).toBeTruthy());
		// MUTATION (P-10): in +page.svelte, in the swarm-caching toggle-row's `<div class="toggle-sub">`
		// region, delete the sentence "It works both ways: you'll hold a lot more, and other people
		// will request collections from you too." — this test reds on getByText below.
		expect(r.getByText(/other people will request collections from you too/i)).toBeTruthy();
		// And it is not softened away: the same sub-label also names the "you'll hold a lot more" half.
		expect(r.getByText(/you'll hold a lot more/i)).toBeTruthy();
	});

	it('the toggle is disabled until the settings load succeeds (QURATOR-93 twin)', async () => {
		// getSettings never resolves → settingsLoaded stays false → every toggle row is disabled.
		getSettingsMock.mockReturnValue(new Promise(() => {}));
		primeIdentity();
		const r = renderSettings();
		const btn = r.getByRole('button', { name: 'Fetch new collections automatically' });
		await waitFor(() => expect((btn as HTMLButtonElement).disabled).toBe(true));
		await fireEvent.click(btn);
		await tick();
		// MUTATION (P-10): in +page.svelte, in the `toggleSwarmCaching` handler body, delete the
		// `if (!settingsLoaded) return;` guard — this test reds: saveSettings fires despite the
		// load never finishing (a defaults-shaped object persisted over the user's real one).
		expect(saveSettingsMock).not.toHaveBeenCalled();
	});
});
