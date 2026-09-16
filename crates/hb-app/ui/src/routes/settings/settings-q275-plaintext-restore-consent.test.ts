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
// Fix: informed consent, NOT a credential. When the header declares plaintext, the user sees an
// unencrypted-file warning (native confirm, the same channel handleBackup() uses for plaintext
// export) and must accept it before doRestore() runs. A genuine plaintext backup — a supported
// shape Hoardbook itself produces — stays restorable after that confirmation, and the encrypted
// flow is untouched: passphrase form, no new dialog.
//
// MUTATION THAT MUST RED tests 1 and 2 (do NOT run it — the orchestrator applies and reverts):
// in ./+page.svelte, inside `async function pickRestore()`, collapse the `if (!restoreNeedsPass)`
// branch (the block starting at the `if (!restoreNeedsPass) {` line that immediately follows
// `restorePass = '';`, ~line 88) back to the pre-fix single statement — delete the `const ok =
// await confirm(` … `if (!ok) { restorePath = null; return; }` lines so the branch body is just
// `doRestore();`. Then the FIRST confirm the flow reaches is doRestore()'s generic
// "Restoring REPLACES all current data…" copy, which does not match /not encrypted/i, and both
// first-confirm-message assertions below go red while everything else stays plausible — proving
// the tests pin the warning's presence, not merely that some dialog appeared.
//
// The mocks intercept every destructive edge (wipeData, restoreData, relaunch), so nothing here
// can touch a real store. Drive the real mounted page through the real DOM (pick file → consent),
// not a source scan.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile, toastMessage } from '$lib/stores.js';

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

vi.mock('@tauri-apps/plugin-dialog', () => ({
	open: vi.fn(),
	save: vi.fn(),
	confirm: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

import { getSettings, peekBackup, restoreData, validateBackup, wipeData } from '$lib/api.js';
import { open as openFileDialog, confirm as confirmDialog } from '@tauri-apps/plugin-dialog';

const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const peekBackupMock = peekBackup as unknown as ReturnType<typeof vi.fn>;
const restoreDataMock = restoreData as unknown as ReturnType<typeof vi.fn>;
const validateBackupMock = validateBackup as unknown as ReturnType<typeof vi.fn>;
const wipeDataMock = wipeData as unknown as ReturnType<typeof vi.fn>;
const openMock = openFileDialog as unknown as ReturnType<typeof vi.fn>;
const confirmMock = confirmDialog as unknown as ReturnType<typeof vi.fn>;

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
	// (blockedByStickyError), making an unrelated test fail on store leakage (per q176).
	toastMessage.set(null);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

/** Exact type of `render(SettingsPage)` — `ReturnType<typeof render>` picks the generic's default
 *  instantiation, which is not this page's result type (svelte-check errors, vitest cannot see it). */
const renderSettings = () => render(SettingsPage);
type Rendered = ReturnType<typeof renderSettings>;

/** Drive the real page to a picked archive whose header declares plaintext. `confirm`'s verdict is
 *  deliberately NOT set here — each test owns it, so a staging done before render survives. */
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

/** The message of the FIRST confirm the flow reached — the page's only channel to the user for
 *  this warning is the native dialog, so "the warning was shown" is pinned on its copy. */
function firstConfirmMessage(): string {
	return String(confirmMock.mock.calls[0]?.[0] ?? '');
}

describe('QURATOR-275 — plaintext header ⇒ unencrypted-file warning + explicit consent', () => {
	it('declining the unencrypted-file warning stops the restore: nothing validates, wipes or commits', async () => {
		confirmMock.mockResolvedValue(false);
		const r = renderSettings();
		await pickPlaintextBackup(r);

		await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(1));
		// The dialog the user just declined is the UNENCRYPTED-FILE warning — not doRestore()'s
		// generic "replaces all current data" confirm. Under the pre-fix code no first dialog
		// exists at all, so this is the assertion the mutation reds.
		expect(firstConfirmMessage()).toMatch(/not encrypted/i);
		// The restore never started: no validation, no wipe, no commit, no second dialog.
		expect(validateBackupMock).not.toHaveBeenCalled();
		expect(wipeDataMock).not.toHaveBeenCalled();
		expect(restoreDataMock).not.toHaveBeenCalled();
		expect(confirmMock).toHaveBeenCalledTimes(1);
	});

	it('a genuine plaintext backup stays restorable — after confirmation, with null (no passphrase demanded)', async () => {
		confirmMock.mockResolvedValue(true);
		const r = renderSettings();
		await pickPlaintextBackup(r);

		await waitFor(() => expect(wipeDataMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		// Consent was the unencrypted-file warning first; the generic replace-all confirm second.
		expect(firstConfirmMessage()).toMatch(/not encrypted/i);
		expect(confirmMock).toHaveBeenCalledTimes(2);
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

	it('an encrypted archive is untouched: the passphrase form appears directly, with no new dialog interposed', async () => {
		getSettingsMock.mockResolvedValue(OK_SETTINGS);
		peekBackupMock.mockResolvedValue(true);
		validateBackupMock.mockResolvedValue(undefined);
		wipeDataMock.mockResolvedValue(undefined);
		restoreDataMock.mockResolvedValue({ npub: 'npub1restored', npub_short: 'npub1res…', share_code: 'hbk1y', key_storage: 'plain-file' });
		openMock.mockResolvedValue(BACKUP_PATH);
		confirmMock.mockResolvedValue(true);
		primeIdentity();
		const r = renderSettings();

		await waitFor(() => expect((r.getByRole('button', { name: /restore from backup/i }) as HTMLButtonElement).disabled).toBe(false));
		await fireEvent.click(r.getByRole('button', { name: /restore from backup/i }));
		await tick();

		// The existing encrypted flow: passphrase form shown (peekBackup said encrypted), and
		// pickRestore itself asked NOTHING — the only confirm so far is doRestore()'s own.
		const passInput = document.querySelector<HTMLInputElement>('input[placeholder="Backup passphrase"]');
		if (!passInput) throw new Error('restore passphrase input should be visible');
		expect(confirmMock).not.toHaveBeenCalled();

		await fireEvent.input(passInput, { target: { value: 'correct-horse-battery' } });
		await tick();
		await fireEvent.click(r.getByRole('button', { name: /^restore$/i }));
		await tick();

		await waitFor(() => expect(validateBackupMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		expect(validateBackupMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
		expect(firstConfirmMessage()).toMatch(/replaces all current data/i);
		expect(confirmMock).toHaveBeenCalledTimes(1);
	});
});
