// @vitest-environment jsdom
// QURATOR-176 — the manifest-cache clear. The owner ruling (2026-09-03) is explicit on shape:
// ONE button, ONE click, the WHOLE cache goes. No confirmation dialog, no "are you sure", no undo
// affordance, and no two-step confirm state — so the assertions here pin exactly that:
//
//   1. the button fires clearManifestCache() exactly once per click (global, all-or-nothing);
//   2. it is a single-click action — after the click there is NO confirm/cancel pair and no
//      second confirm button interposed between the click and the call;
//   3. it never touches the wider store — wipeData() is NOT called (scope: manifest cache only);
//   4. a failure toasts and un-busies the button (the cache may legitimately be unreadable).
//
// The mocks intercept every destructive edge, so nothing here can touch a real store. Drive the
// real mounted page through the real DOM, not a source scan. The "no confirm" check asserts on
// the DOM (button inventory before/after the click), never on prose.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import SettingsPage from './+page.svelte';
import { identity, profile, toastMessage } from '$lib/stores.js';
import { get } from 'svelte/store';

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
	clearManifestCache: vi.fn().mockResolvedValue(undefined),
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

vi.mock('@tauri-apps/plugin-dialog', () => ({
	open: vi.fn(),
	save: vi.fn(),
	confirm: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

import { getSettings, clearManifestCache, wipeData } from '$lib/api.js';

const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const clearMock = clearManifestCache as unknown as ReturnType<typeof vi.fn>;
const wipeMock = wipeData as unknown as ReturnType<typeof vi.fn>;

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

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	profile.set(null);
	// A sticky ERROR toast from a previous test otherwise blocks every later success toast
	// (blockedByStickyError), making an unrelated test fail on store leakage, not on its own
	// assertion.
	toastMessage.set(null);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

async function renderSettings() {
	getSettingsMock.mockResolvedValue(OK_SETTINGS);
	primeIdentity();
	return render(SettingsPage);
}

/** The exact buttons in the danger zone + clear row, before vs after the clear click. A confirm
 *  step would have to appear as a NEW button after the click — this catches one appearing. */
function dangerAndClearButtons(container: HTMLElement): HTMLElement[] {
	return Array.from(container.querySelectorAll('button')).filter((b) =>
		/clear (manifest )?cache|clearing|wipe data|confirm wipe|cancel|are you sure/i.test(
			(b.textContent ?? '') + ' ' + (b.getAttribute('aria-label') ?? ''),
		),
	);
}

describe('QURATOR-176 — one click clears the whole manifest cache', () => {
	it('a single click calls clearManifestCache() once, with no confirm interposed and no wipeData', async () => {
		const { container, getByRole } = await renderSettings();

		const before = dangerAndClearButtons(container).map((b) => b.textContent?.trim());
		const btn = getByRole('button', { name: /^clear cache$/i }) as HTMLButtonElement;
		await waitFor(() => expect(btn.disabled).toBe(false));

		await fireEvent.click(btn);
		await waitFor(() => expect(clearMock).toHaveBeenCalledTimes(1));

		// Single-click, still: no confirm/cancel pair appeared after the click, and the button
		// returns to its idle label (not swapped for a "Confirm clear" step).
		await waitFor(() =>
			expect(dangerAndClearButtons(container).map((b) => b.textContent?.trim())).toEqual(before),
		);

		// Scope: the manifest cache only — never the wider store.
		expect(wipeMock).not.toHaveBeenCalled();
	});

	it('a rejected clear toasts an error and un-busies the button', async () => {
		clearMock.mockRejectedValueOnce(new Error('disk unavailable'));
		const { getByRole } = await renderSettings();

		const btn = getByRole('button', { name: /^clear cache$/i }) as HTMLButtonElement;
		await waitFor(() => expect(btn.disabled).toBe(false));

		await fireEvent.click(btn);
		await waitFor(() => expect(get(toastMessage)?.kind).toBe('error'));

		await waitFor(() => expect(btn.disabled).toBe(false));
		expect(btn.textContent?.trim()).toBe('Clear cache');
	});

	it('a successful clear toasts success', async () => {
		const { getByRole } = await renderSettings();

		const btn = getByRole('button', { name: /^clear cache$/i }) as HTMLButtonElement;
		await waitFor(() => expect(btn.disabled).toBe(false));

		await fireEvent.click(btn);
		await waitFor(() => expect(get(toastMessage)?.kind).toBe('success'));
	});
});
