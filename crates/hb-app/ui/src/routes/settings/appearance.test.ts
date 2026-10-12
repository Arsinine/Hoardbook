// @vitest-environment jsdom
// QURATOR-352 lane C — the Appearance section in Settings.
//
// Mounts the real Settings page with the house mock of $lib/api.js (copied from
// settings-toggle-gate.test.ts), which also stubs api.ts's two window-material commands. The REAL $lib/themes/apply.ts runs — the assertions read what IT painted on
// <html> (data-theme) and mirrors to localStorage — so a stubbed applier would red these tests.
// The 2026-10-12 owner ruling (Vault is DEFAULT, Hoard = BASE) is honoured: picking Vault SETS
// data-theme='vault' (apply.ts removes the attribute only for BASE_THEME).
//
// Pinned here:
//   - all 8 theme cards render, from THEMES, with roving tabindex on the checked card;
//   - picking a theme applies IMMEDIATELY (root attribute / default = absent) AND calls the
//     mocked saveSettings with theme:'<id>' — the full-object save path every other toggle uses;
//   - arrow keys move focus and select the neighbour (native radio-group semantics);
//   - the see-through row exists ONLY when window_material_supported passes;
//   - glass-off / Hoard disable see-through with their reason as the help text;
//   - the QURATOR-93 belt-and-suspenders gate: a failed settings load disables the new controls
//     and the in-handler guards alone still block the save.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile } from '$lib/stores.js';
import { THEMES } from '$lib/themes/themes.js';

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
	windowMaterialSupported: vi.fn(),
	setWindowMaterial: vi.fn().mockResolvedValue(undefined),
}));

import { getSettings, saveSettings, windowMaterialSupported, setWindowMaterial } from '$lib/api.js';
const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const saveSettingsMock = saveSettings as unknown as ReturnType<typeof vi.fn>;
const probeMock = windowMaterialSupported as unknown as ReturnType<typeof vi.fn>;
const setMaterialMock = setWindowMaterial as unknown as ReturnType<typeof vi.fn>;

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

// Older settings files lack the appearance fields entirely — that absence is the case the
// optional fields + DEFAULT_THEME fallback exist for.
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

beforeEach(() => {
	// Fresh root per test: apply.ts paints these, and a stale attribute would fake a pass.
	for (const a of ['data-theme', 'data-glass-float', 'data-see-through', 'data-glass-off']) {
		document.documentElement.removeAttribute(a);
	}
	localStorage.clear();
	probeMock.mockReset();
	setMaterialMock.mockReset().mockResolvedValue(undefined);
});

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

async function renderLoaded() {
	getSettingsMock.mockResolvedValue({ ...OK_SETTINGS });
	primeIdentity();
	const utils = render(SettingsPage);
	// settingsLoaded flips only after getSettings resolves; the Vault card enables then.
	await waitFor(() => {
		const vault = utils.getByRole('radio', { name: /^Vault/ }) as HTMLButtonElement;
		expect(vault.disabled).toBe(false);
	});
	return utils;
}

describe('QURATOR-352 — Appearance section in Settings', () => {
	it('renders all 8 theme cards from THEMES, Vault selected by default, roving tabindex on it', async () => {
		const { getByRole } = await renderLoaded();

		expect(getByRole('radiogroup', { name: 'Theme' })).toBeTruthy();
		const radios = getAllRadios();
		expect(radios.length).toBe(THEMES.length);
		expect(THEMES.length).toBe(8);
		for (const t of THEMES) {
			expect(getByRole('radio', { name: new RegExp(`^${t.name}`) })).toBeTruthy();
		}
		// No theme field in settings → the default theme is selected (2026-10-12 ruling: Vault).
		expect(getByRole('radio', { name: /^Vault/ }).getAttribute('aria-checked')).toBe('true');
		expect(getByRole('radio', { name: /^Hoard/ }).getAttribute('aria-checked')).toBe('false');
		// Roving tabindex: the checked card is tabbable, the others are not.
		expect((getByRole('radio', { name: /^Vault/ }) as HTMLElement).tabIndex).toBe(0);
		expect((getByRole('radio', { name: /^Hoard/ }) as HTMLElement).tabIndex).toBe(-1);
	});

	it('clicking a non-default theme applies it immediately on <html> and saves theme:<id>', async () => {
		const { getByRole } = await renderLoaded();

		await fireEvent.click(getByRole('radio', { name: /^Oxblood Steel/ }));
		await tick();
		expect(document.documentElement.getAttribute('data-theme')).toBe('oxblood-steel');
		expect(getByRole('radio', { name: /^Oxblood Steel/ }).getAttribute('aria-checked')).toBe('true');
		expect(getByRole('radio', { name: /^Vault/ }).getAttribute('aria-checked')).toBe('false');
		expect(saveSettingsMock).toHaveBeenCalledWith(expect.objectContaining({ theme: 'oxblood-steel' }));
	});

	it("clicking Vault (the default) sets data-theme='vault', mirrors localStorage, and saves theme:'vault'", async () => {
		getSettingsMock.mockResolvedValue({ ...OK_SETTINGS, theme: 'oxblood-steel' });
		primeIdentity();
		const { getByRole } = render(SettingsPage);
		await waitFor(() => {
			const vault = getByRole('radio', { name: /^Vault/ }) as HTMLButtonElement;
			expect(vault.disabled).toBe(false);
		});
		// The page never applies on load (the boot script owns launch); with the saved theme
		// oxblood-steel the root carries no attribute yet.
		expect(document.documentElement.getAttribute('data-theme')).toBeNull();

		await fireEvent.click(getByRole('radio', { name: /^Vault/ }));
		await tick();
		// 2026-10-12 ruling: Vault IS the default, and the default is a theme file like any other —
		// apply.ts removes data-theme only for BASE_THEME (hoard). So picking Vault SETS it.
		expect(document.documentElement.getAttribute('data-theme')).toBe('vault');
		expect(getByRole('radio', { name: /^Vault/ }).getAttribute('aria-checked')).toBe('true');
		expect(saveSettingsMock).toHaveBeenCalledWith(expect.objectContaining({ theme: 'vault' }));
		// The real apply.ts ran: its best-effort mirror is on localStorage too.
		expect(JSON.parse(localStorage.getItem('hb-appearance') ?? '{}')).toEqual(
			expect.objectContaining({ theme: 'vault' }),
		);
	});

	it('arrow keys move focus AND select the neighbour (native radio-group semantics)', async () => {
		const { getByRole } = await renderLoaded();

		const vault = getByRole('radio', { name: /^Vault/ }) as HTMLElement;
		const pale = getByRole('radio', { name: /^Pale Pulse/ }) as HTMLElement;
		vault.focus();
		expect(document.activeElement).toBe(vault);

		const group = getByRole('radiogroup', { name: 'Theme' });
		await fireEvent.keyDown(group, { key: 'ArrowRight' });
		await tick();
		// THEMES order: … vault(5), pale-pulse(6), sky-slate(7) — Right wraps? No: 5→6.
		expect(document.activeElement).toBe(pale);
		expect(pale.getAttribute('aria-checked')).toBe('true');
		expect(vault.getAttribute('aria-checked')).toBe('false');
		expect(document.documentElement.getAttribute('data-theme')).toBe('pale-pulse');
		expect(saveSettingsMock).toHaveBeenCalledWith(expect.objectContaining({ theme: 'pale-pulse' }));

		// Left from Pale Pulse goes back to Vault (which sets data-theme='vault' like any theme).
		await fireEvent.keyDown(group, { key: 'ArrowLeft' });
		await tick();
		expect(document.activeElement).toBe(vault);
		expect(vault.getAttribute('aria-checked')).toBe('true');
		expect(document.documentElement.getAttribute('data-theme')).toBe('vault');
	});

	it('see-through row is ABSENT when window_material_supported resolves false', async () => {
		probeMock.mockResolvedValue(false);
		const { getByRole, queryByRole } = await renderLoaded();

		await waitFor(() => expect(probeMock).toHaveBeenCalled());
		expect(queryByRole('button', { name: /see-through window/i })).toBeNull();
		expect(getByRole('button', { name: /glass effects/i })).toBeTruthy();
	});

	it('see-through row is present when supported; clicking it saves see_through:true and drives the OS material', async () => {
		probeMock.mockResolvedValue(true);
		const { getByRole } = await renderLoaded();

		const seeThrough = await waitFor(() => {
			const b = getByRole('button', { name: /see-through window/i }) as HTMLButtonElement;
			expect(b.disabled).toBe(false); // default theme Vault ≠ BASE_THEME(hoard), glass default on
			return b;
		});

		await fireEvent.click(seeThrough);
		await tick();
		expect(saveSettingsMock).toHaveBeenCalledWith(expect.objectContaining({ see_through: true }));
		expect(setMaterialMock).toHaveBeenLastCalledWith({ seeThrough: true, glass: true, theme: 'vault' });
	});

	it('switching to Hoard with see-through on re-syncs the OS material, so Mica turns off', async () => {
		// GLM review 2026-10-12: changing theme or glass used to leave Mica on behind an opaque
		// window until restart. Every appearance change now re-sends the full state; Rust's
		// see_through_effective turns it off for hoard.
		// MUTATION (P-10): delete the `void setWindowMaterial();` line in selectTheme — this reds.
		probeMock.mockResolvedValue(true);
		getSettingsMock.mockResolvedValue({ ...OK_SETTINGS, see_through: true });
		primeIdentity();
		const { getByRole } = render(SettingsPage);
		await waitFor(() => expect((getByRole('radio', { name: /^Hoard/ }) as HTMLButtonElement).disabled).toBe(false));
		setMaterialMock.mockClear();
		await fireEvent.click(getByRole('radio', { name: /^Hoard/ }));
		await tick();
		expect(setMaterialMock).toHaveBeenLastCalledWith({ seeThrough: true, glass: true, theme: 'hoard' });
	});

	it('glass effects toggle applies at once and saves glass:false', async () => {
		const { getByRole } = await renderLoaded();

		await fireEvent.click(getByRole('button', { name: /glass effects/i }));
		await tick();
		expect(document.documentElement.getAttribute('data-glass-off')).toBe('');
		expect(saveSettingsMock).toHaveBeenCalledWith(expect.objectContaining({ glass: false }));
	});

	it('glass off disables see-through with its reason; Hoard disables it with "Needs a glass theme."', async () => {
		probeMock.mockResolvedValue(true);
		getSettingsMock.mockResolvedValue({ ...OK_SETTINGS, glass: false });
		primeIdentity();
		const utils = render(SettingsPage);
		const seeThrough = await waitFor(() => {
			const b = utils.getByRole('button', { name: /see-through window/i }) as HTMLButtonElement;
			expect(b.disabled).toBe(true);
			return b;
		});
		// glass:false + no theme field → themeId falls back to the DEFAULT (vault), so the reason
		// shown is the glass one.
		expect(utils.getByText('Needs glass effects on.')).toBeTruthy();
		void seeThrough;

		// Hoard is the one theme with no glass, regardless of glass being on. Unmount the first page
		// before mounting the second, or both pages' help texts sit in the document at once.
		cleanup();
		getSettingsMock.mockResolvedValue({ ...OK_SETTINGS, theme: 'hoard' });
		const utils2 = render(SettingsPage);
		await waitFor(() => {
			const b = utils2.getByRole('button', { name: /see-through window/i }) as HTMLButtonElement;
			expect(b.disabled).toBe(true);
		});
		expect(utils2.getByText('Needs a glass theme.')).toBeTruthy();
	});

	it('QURATOR-93 twin: a failed settings load disables the new controls, and the in-handler guards alone block saves', async () => {
		probeMock.mockResolvedValue(true);
		getSettingsMock.mockRejectedValue(new Error('settings.json unreadable'));
		primeIdentity();

		const { getByRole } = render(SettingsPage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());

		const glass = getByRole('button', { name: /glass effects/i }) as HTMLButtonElement;
		// The see-through row renders only after the Windows-11 probe resolves, which can land after
		// the load-failure alert. Wait for the row itself rather than assuming it is already there.
		const seeThrough = (await waitFor(() =>
			getByRole('button', { name: /see-through window/i })
		)) as HTMLButtonElement;
		const vault = getByRole('radio', { name: /^Vault/ }) as HTMLButtonElement;
		expect(glass.disabled).toBe(true);
		expect(seeThrough.disabled).toBe(true);
		expect(vault.disabled).toBe(true);

		// Bypass the DOM gate (house methodology, settings-toggle-gate.test.ts) — the handler
		// guards must hold on their own.
		glass.disabled = false;
		seeThrough.disabled = false;
		vault.disabled = false;
		await fireEvent.click(glass);
		await fireEvent.click(seeThrough);
		await fireEvent.click(vault);
		await tick();
		expect(saveSettingsMock).not.toHaveBeenCalled();
	});
});

function getAllRadios(): HTMLElement[] {
	return Array.from(document.querySelectorAll('[role="radio"]'));
}
