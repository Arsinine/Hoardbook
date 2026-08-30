// @vitest-environment jsdom
// QURATOR-158 — the launch-time background update check. Behavioural mount tests: the page is
// really rendered and asserted on its affordances, never on prose scoured from source.
//
// What is pinned here:
//   1. A check fires on mount with NO user interaction, and it populates portableInfo (portable
//      build) or updateInfo (NSIS build) — proven by the "Update & restart" / "Download update"
//      buttons of the EXISTING update row appearing.
//   2. The check runs AFTER `updaterIsPortable()` resolves — a portable build must not take the
//      NSIS branch (mutation 1 moves the call above the await and this reds).
//   3. A FAILING background check leaves NO error text, NO toast, the manual button idle, and no
//      "Up to date" claim (updateChecked must not be set by the background path).
//   4. The badge appears only when a newer version was detected; with a null check result it is
//      absent.
//   5. The manual button's behaviour is unchanged: it still reports errors and still confirms
//      "Up to date" on success.
//
// Honest limits (jsdom computes no layout): nothing here proves the badge renders visually ABOVE
// the "Currently running v…" label — that is CSS, asserted only by source position. The
// scrollIntoView routing is likewise a visual convenience; what is tested is that the badge routes
// into the existing update row's flow, i.e. that the update buttons it points at are real.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile, toastMessage } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

vi.mock('$lib/api.js', () => ({
	generateKeypair: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ relay_urls: ['wss://relay.example.com'], allow_dms: true, big_relay_url: '' }),
	saveSettings: vi.fn(),
	importNsec: vi.fn(),
	backupData: vi.fn(),
	peekBackup: vi.fn(),
	restoreData: vi.fn(),
	wipeData: vi.fn(),
	checkRelay: vi.fn().mockResolvedValue(undefined),
	relayStatus: vi.fn().mockResolvedValue([]),
	beaconStatus: vi.fn().mockResolvedValue(null),
	// The two under test — individual tests override.
	checkUpdate: vi.fn().mockResolvedValue(null),
	downloadUpdate: vi.fn(),
	applyStagedUpdate: vi.fn(),
	takeUpdateNotice: vi.fn().mockResolvedValue(null),
	updaterIsPortable: vi.fn().mockResolvedValue(false),
	checkPortableUpdate: vi.fn().mockResolvedValue(null),
	applyPortableUpdate: vi.fn(),
	hasPublishedProfile: vi.fn().mockResolvedValue(false),
	publishProfile: vi.fn(),
	copyDiagnostics: vi.fn(),
	revealLogFolder: vi.fn(),
	natClassification: vi.fn().mockResolvedValue('no-nat'),
	dmBlockedList: vi.fn().mockResolvedValue([]),
	dmUnblock: vi.fn().mockResolvedValue(undefined),
	dmBlock: vi.fn(),
	validateShareCode: vi.fn(),
	shareCodeInfo: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn(), confirm: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.18.0') }));

import { checkUpdate, checkPortableUpdate, updaterIsPortable, getSettings } from '$lib/api.js';
const checkUpdateMock = checkUpdate as unknown as ReturnType<typeof vi.fn>;
const checkPortableUpdateMock = checkPortableUpdate as unknown as ReturnType<typeof vi.fn>;
const isPortableMock = updaterIsPortable as unknown as ReturnType<typeof vi.fn>;
const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	// clearAllMocks clears CALLS, not implementations — without this reset, the portable test's
	// mockImplementation(async () => true) leaks into every later test and they all silently take
	// the portable branch (checkUpdate never fires). This was a real 5-test failure, not flake.
	isPortableMock.mockResolvedValue(false);
	// Same leak class as above: mockResolvedValue/mockRejectedValue set an implementation that
	// clearAllMocks does not clear. Reset all three check mocks to the idle default each test
	// starts from; individual tests override again.
	checkUpdateMock.mockResolvedValue(null);
	checkPortableUpdateMock.mockResolvedValue(null);
	getSettingsMock.mockResolvedValue({ relay_urls: ['wss://relay.example.com'], allow_dms: true, big_relay_url: '' });
	toastMessage.set(null);
	identity.set(null);
	profile.set(null);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

/** The manual "Check for updates" button — the idle/busy control the background path must not
 *  disturb. Scoped by its exact accessible name. */
function manualButton(): HTMLButtonElement {
	return document.querySelector('.update-actions button.btn-default') as HTMLButtonElement;
}

describe('QURATOR-158 — background update check on mount', () => {
	it('NSIS build: the check fires with no user interaction and the existing update row lights up', async () => {
		primeIdentity();
		checkUpdateMock.mockResolvedValue({ version: '0.19.0', body: 'notes' });

		const { findByRole } = render(SettingsPage);
		// The existing UI, driven only by the background check's result — no click happened.
		const download = await findByRole('button', { name: 'Download update' });
		expect(download).toBeTruthy();
		expect(checkUpdateMock).toHaveBeenCalledTimes(1);
		expect(checkPortableUpdateMock).not.toHaveBeenCalled();
	});

	it('portable build: the portable branch is taken once isPortable resolves', async () => {
		primeIdentity();
		// isPortable resolves asynchronously — never synchronously — so the ordering is real.
		isPortableMock.mockImplementation(async () => true);
		checkPortableUpdateMock.mockResolvedValue({ version: '0.19.0', notes: 'notes' });

		const { findByRole } = render(SettingsPage);
		const apply = await findByRole('button', { name: 'Update & restart' });
		expect(apply).toBeTruthy();
		// THE ordering pin: the portable branch was picked, so the background check ran after the
		// await updaterIsPortable() line — not before it.
		expect(checkPortableUpdateMock).toHaveBeenCalledTimes(1);
		expect(checkUpdateMock).not.toHaveBeenCalled();
	});

	it('a FAILING background check is silent: no error text, no toast, manual button idle, no "Up to date"', async () => {
		primeIdentity();
		checkUpdateMock.mockRejectedValue(new Error('network unreachable'));

		const { container } = render(SettingsPage);
		// Let the rejected promise settle.
		await waitFor(() => expect(checkUpdateMock).toHaveBeenCalledTimes(1), { timeout: 5000 });
		await tick(); await tick();

		expect(container.querySelector('.update-error-text')).toBeNull();
		expect(container.textContent).not.toContain('network unreachable');
		// No toast of any kind fired by the background path.
		let toasts: string[] = [];
		const unsub = toastMessage.subscribe((t) => { toasts = t ? [t.text] : []; });
		unsub();
		expect(toasts).toEqual([]);
		// The manual button is in its normal idle state — not disabled, not "Checking…".
		const btn = manualButton();
		expect(btn, 'manual check button exists').toBeTruthy();
		expect(btn.disabled).toBe(false);
		expect(btn.textContent).toBe('Check for updates');
		// updateChecked must NOT be set by the background path — an unrequested check may not
		// assert "you're up to date".
		expect(container.textContent).not.toContain('Up to date');
	});

	it('the new-version badge appears only when a newer version was detected', async () => {
		primeIdentity();
		checkUpdateMock.mockResolvedValue({ version: '0.19.0', body: 'notes' });

		const { container, findByRole } = render(SettingsPage);
		await findByRole('button', { name: 'Download update' });
		const badge = container.querySelector('.update-badge');
		expect(badge, 'badge present when a newer version exists').toBeTruthy();
		expect(badge!.textContent).toContain('New version available');
	});

	it('clicking the badge routes into the existing update row (scrollIntoView wiring)', async () => {
		primeIdentity();
		checkUpdateMock.mockResolvedValue({ version: '0.19.0', body: 'notes' });
		// jsdom does not implement scrollIntoView — stub it so the click exercises real handler code.
		const scrollIntoView = vi.fn();
		Element.prototype.scrollIntoView = scrollIntoView as unknown as () => void;

		const { container, findByRole } = render(SettingsPage);
		await findByRole('button', { name: 'Download update' });
		await fireEvent.click(container.querySelector('.update-badge')!);
		expect(scrollIntoView).toHaveBeenCalledTimes(1);
		// The badge routes to the row that owns the REAL update button — the existing flow, not a fork.
		expect(container.querySelector('.update-row')!.contains(container.querySelector('.update-badge'))).toBe(true);
	});

	it('badge ABSENT when the background check returns null (nothing newer)', async () => {
		primeIdentity();

		const { container } = render(SettingsPage);
		await waitFor(() => expect(checkUpdateMock).toHaveBeenCalledTimes(1), { timeout: 5000 });
		await tick(); await tick();
		expect(container.querySelector('.update-badge')).toBeNull();
		// And still no unrequested "Up to date" claim.
		expect(container.textContent).not.toContain('Up to date');
	});

	it('MANUAL button unchanged: a failure still reports its error (regression guard)', async () => {
		primeIdentity();
		checkUpdateMock
			.mockRejectedValueOnce(new Error('network unreachable'))   // the background check
			.mockRejectedValueOnce(new Error('404 manifest missing')); // the manual click

		const { container, getByRole } = render(SettingsPage);
		await waitFor(() => expect(checkUpdateMock).toHaveBeenCalledTimes(1), { timeout: 5000 });
		await tick();

		await fireEvent.click(manualButton());
		await waitFor(() => expect(container.querySelector('.update-error-text')).toBeTruthy());
		expect(container.querySelector('.update-error-text')!.textContent).toContain('404 manifest missing');
		// The manual button stays clickable through its own failure.
		expect(manualButton().disabled).toBe(false);
		expect(getByRole('button', { name: 'Check for updates' })).toBeTruthy();
	});

	it('MANUAL button unchanged: a null result still confirms "Up to date"', async () => {
		primeIdentity();
		// Background check runs first and returns null; the manual click then gets a null too.
		checkUpdateMock.mockResolvedValue(null);

		const { container } = render(SettingsPage);
		await waitFor(() => expect(checkUpdateMock).toHaveBeenCalledTimes(1), { timeout: 5000 });
		await tick();

		// Background path did NOT claim up-to-date…
		expect(container.textContent).not.toContain('Up to date');

		// …but the manual press does.
		await fireEvent.click(manualButton());
		await waitFor(() => expect(container.textContent).toContain('Up to date'));
	});
});
