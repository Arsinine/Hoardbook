// @vitest-environment jsdom
// QURATOR-275 — pickRestore() decided whether to ask for a passphrase by sniffing the archive's
// header via peekBackup(), and on plaintext it called doRestore() IMMEDIATELY. The header is
// attacker-controlled: swap the user's encrypted backup for a forged plaintext one and peekBackup()
// returns false, the passphrase prompt is skipped entirely, and validateBackup(null)/
// restoreData(null) is the LEGITIMATE plaintext combination — so nothing errors and an
// unauthenticated restore commits with no indication the expected protection was absent. (The
// QURATOR-228 hard error in backup.rs — plaintext header + supplied passphrase — never fires on
// this path, because the skipped prompt means null is supplied.)
//
// Fix: informed consent, NOT a credential. When the header declares plaintext, the user sees the
// in-app "Unencrypted backup" modal (QURATOR-379 — askInApp, no native OS dialog) and must accept
// it before doRestore() runs. A genuine plaintext backup — a supported shape Hoardbook itself
// produces — stays restorable after that confirmation, and the encrypted flow is untouched:
// passphrase form, no dialog until doRestore's own consent modal.
//
// MUTATION THAT MUST RED tests 1–3 (do NOT run it — the orchestrator applies and reverts):
// in ./+page.svelte, inside `async function pickRestore()`, collapse the `if (!restoreNeedsPass)`
// branch (the block starting at the `if (!restoreNeedsPass) {` line that immediately follows
// `restorePass = '';`) back to the pre-fix single statement — delete the `const ok = await
// askInApp(` … `if (!ok) { restorePath = null; return; }` lines so the branch body is just
// `doRestore();`. Then the FIRST dialog the plaintext flow reaches is doRestore()'s generic
// "Restore from backup / replaces all data…" modal, whose copy does not match /no passphrase/i —
// tests 1 and 2 red on the dialog-copy assertions; test 3 reds because a dialog now appears where
// the encrypted flow pins NONE before the passphrase form.
//
// The mocks intercept every destructive edge (wipeData, restoreData, relaunch), so nothing here
// can touch a real store. Drive the real mounted page through the real DOM (pick file → consent),
// not a source scan.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile, toastMessage, errorToasts, dismissError } from '$lib/stores.js';
import { get } from 'svelte/store';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const BACKUP_PATH = '/mnt/backups/hoardbook-2026-09-15.hbk';

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
	dmBlock: vi.fn(),
	validateShareCode: vi.fn(),
	shareCodeInfo: vi.fn(),
}));

// NO `confirm` export — a production regression to the native dialog hits `undefined` and reds
// loudly here instead of being silently granted (same fail-closed wiring as q126).
vi.mock('@tauri-apps/plugin-dialog', () => ({
	open: vi.fn(),
	save: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

import { getSettings, peekBackup, restoreData, validateBackup, wipeData } from '$lib/api.js';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';

const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const peekBackupMock = peekBackup as unknown as ReturnType<typeof vi.fn>;
const restoreDataMock = restoreData as unknown as ReturnType<typeof vi.fn>;
const validateBackupMock = validateBackup as unknown as ReturnType<typeof vi.fn>;
const wipeDataMock = wipeData as unknown as ReturnType<typeof vi.fn>;
const openMock = openFileDialog as unknown as ReturnType<typeof vi.fn>;

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
	// Reset both toast surfaces: a leaked SUCCESS toast could satisfy a later findByText, and a
	// leaked ERROR (15s timer, QURATOR-379 stack) could satisfy a later error wait.
	toastMessage.set(null);
	for (const e of get(errorToasts)) dismissError(e.id);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

/** Exact type of `render(SettingsPage)` — `ReturnType<typeof render>` picks the generic's default
 *  instantiation, which is not this page's result type (svelte-check errors, vitest cannot see it). */
const renderSettings = () => render(SettingsPage);
type Rendered = ReturnType<typeof renderSettings>;

/** The consent dialog is the page's IN-APP Modal (role=dialog) — scope to it so the page's own
 *  buttons underneath can never match. */
async function theDialog(): Promise<HTMLElement> {
	await waitFor(() => expect(document.querySelector('[role="dialog"]')).toBeTruthy());
	return document.querySelector<HTMLElement>('[role="dialog"]')!;
}

async function modalButton(name: RegExp): Promise<HTMLButtonElement> {
	const dialog = await theDialog();
	const btn = [...dialog.querySelectorAll('button')].find((b) => name.test((b.textContent ?? '').trim()));
	expect(btn, `modal button matching ${name} should exist`).toBeTruthy();
	return btn as HTMLButtonElement;
}

/** Drive the real page to a picked archive whose header declares plaintext. Consent is NOT granted
 *  here — each test owns the modal click. */
async function pickPlaintextBackup(r: Rendered) {
	getSettingsMock.mockResolvedValue(OK_SETTINGS);
	peekBackupMock.mockResolvedValue(false);
	validateBackupMock.mockResolvedValue(undefined);
	wipeDataMock.mockResolvedValue(undefined);
	restoreDataMock.mockResolvedValue({ npub: 'npub1restored', npub_short: 'npub1res…', share_code: 'hbk1y', key_storage: 'plain-file' });
	openMock.mockResolvedValue(BACKUP_PATH);
	primeIdentity();

	await waitFor(() => expect((r.getByRole('button', { name: /restore from backup/i }) as HTMLButtonElement).disabled).toBe(false));
	await fireEvent.click(r.getByRole('button', { name: /restore from backup/i }));
	await tick();
}

describe('QURATOR-275 — plaintext header ⇒ unencrypted-file warning + explicit consent', () => {
	it('declining the unencrypted-file modal stops the restore: nothing validates, wipes or commits', async () => {
		const r = renderSettings();
		await pickPlaintextBackup(r);

		// The dialog the user sees is the UNENCRYPTED-FILE warning — title and body, not
		// doRestore()'s generic "replaces all data" modal. Under the pre-fix code no first dialog
		// exists at all, so this is the assertion the mutation reds.
		const dialog = await theDialog();
		expect(dialog.querySelector('.modal-title')?.textContent).toMatch(/unencrypted backup/i);
		expect(dialog.textContent).toMatch(/no passphrase/i);
		// Decline it.
		await fireEvent.click(await modalButton(/^cancel$/i));
		await tick();

		// The restore never started: no validation, no wipe, no commit.
		expect(validateBackupMock).not.toHaveBeenCalled();
		expect(wipeDataMock).not.toHaveBeenCalled();
		expect(restoreDataMock).not.toHaveBeenCalled();
		// The modal is gone and nothing re-armed it.
		await waitFor(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
		expect((r.getByRole('button', { name: /restore from backup/i }) as HTMLButtonElement).disabled).toBe(false);
	});

	it('a genuine plaintext backup stays restorable — after confirmation, with null (no passphrase demanded)', async () => {
		const r = renderSettings();
		await pickPlaintextBackup(r);

		// Consent is the unencrypted-file modal first (title "Unencrypted backup", body names the
		// missing passphrase); grant it via "Restore anyway".
		const dialog = await theDialog();
		expect(dialog.querySelector('.modal-title')?.textContent).toMatch(/unencrypted backup/i);
		expect(dialog.textContent).toMatch(/no passphrase/i);
		await fireEvent.click(await modalButton(/^restore anyway$/i));
		await tick();
		// …then doRestore's own replace-all consent modal (two consents, exactly like the old
		// native flow). Grant it too.
		const dialog2 = await theDialog();
		expect(dialog2.textContent).toMatch(/replaces all data on this device/i);
		await fireEvent.click(await modalButton(/^restore$/i));

		await waitFor(() => expect(wipeDataMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		// Informed consent, not a credential: the restore proceeds with null — a plaintext archive
		// must not be asked for a passphrase it cannot check (QURATOR-228 made that an error).
		expect(validateBackupMock).toHaveBeenCalledWith(null, BACKUP_PATH);
		expect(restoreDataMock).toHaveBeenCalledWith(null, BACKUP_PATH);
		// …and only after the wipe, as QURATOR-126 ordered.
		const validateAt = validateBackupMock.mock.invocationCallOrder[0];
		const wipeAt = wipeDataMock.mock.invocationCallOrder[0];
		const restoreAt = restoreDataMock.mock.invocationCallOrder[0];
		expect(wipeAt).toBeGreaterThan(validateAt);
		expect(restoreAt).toBeGreaterThan(wipeAt);
	});

	it('an encrypted archive is untouched: the passphrase form appears directly, with no dialog before the consent modal', async () => {
		getSettingsMock.mockResolvedValue(OK_SETTINGS);
		peekBackupMock.mockResolvedValue(true);
		validateBackupMock.mockResolvedValue(undefined);
		wipeDataMock.mockResolvedValue(undefined);
		restoreDataMock.mockResolvedValue({ npub: 'npub1restored', npub_short: 'npub1res…', share_code: 'hbk1y', key_storage: 'plain-file' });
		openMock.mockResolvedValue(BACKUP_PATH);
		primeIdentity();
		const r = renderSettings();

		await waitFor(() => expect((r.getByRole('button', { name: /restore from backup/i }) as HTMLButtonElement).disabled).toBe(false));
		await fireEvent.click(r.getByRole('button', { name: /restore from backup/i }));
		await tick();

		// The existing encrypted flow: passphrase form shown (peekBackup said encrypted), and
		// pickRestore itself asked NOTHING — no dialog yet.
		const passInput = document.querySelector<HTMLInputElement>('input[placeholder="Backup passphrase"]');
		if (!passInput) throw new Error('restore passphrase input should be visible');
		expect(document.querySelector('[role="dialog"]')).toBeNull();

		await fireEvent.input(passInput, { target: { value: 'correct-horse-battery' } });
		await tick();
		await fireEvent.click(r.getByRole('button', { name: /^restore$/i }));
		await tick();

		// doRestore's consent modal — its copy is the "replaces all data" body, not the plaintext
		// warning (that modal only belongs to the plaintext path).
		const dialog = await theDialog();
		expect(dialog.textContent).toMatch(/replaces all data on this device/i);
		await fireEvent.click(await modalButton(/^restore$/i));

		await waitFor(() => expect(validateBackupMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		expect(validateBackupMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
	});
});
