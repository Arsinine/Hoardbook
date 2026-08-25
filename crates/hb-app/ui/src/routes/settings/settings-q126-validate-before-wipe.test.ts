// @vitest-environment jsdom
// QURATOR-126 (finding #15, CVSS 5.3, INV-8 regression) — doRestore() used to call wipeData()
// UNCONDITIONALLY, before anything had proven the archive restorable: peekBackup() only reads a
// 72-byte header (no KDF, no decrypt, no parse), so a TYPED PASSPHRASE — not an attacker — sent
// the app into wipe→restore with a wrong key, and the failed restoreData() left the device with
// no local identity (nsec, browse key, transport secret all gone) and no rollback.
//
// Fix: doRestore() calls validateBackup(passphrase, path) FIRST — the same arguments
// restoreData() will receive — and only proceeds to wipeData() when that resolves. On rejection
// the catch toasts the error and flips `restoring` back to false with every byte of local data
// untouched.
//
// The ORDERING is the point of the row: the assertion that matters is "validateBackup rejects ⇒
// wipeData was NEVER called" — proving an error merely *shown* would not. The mocks intercept
// every destructive edge (wipeData, restoreData, relaunch), so nothing here can touch a real
// store or a real ~/.hoardbook. Drive the real mounted page through the real DOM (pick file →
// type passphrase → click Restore), not a source scan.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile, toastMessage } from '$lib/stores.js';
import { get } from 'svelte/store';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const BACKUP_PATH = '/mnt/backups/hoardbook-2026-08-24.hbk';

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
	// The confirm dialog is the explicit "this replaces all current data" consent step; the test
	// grants it so the flow reaches the code under test rather than bailing at the prompt.
	confirm: vi.fn().mockResolvedValue(true),
}));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

import { getSettings, peekBackup, restoreData, validateBackup, wipeData } from '$lib/api.js';
import { open as openFileDialog, confirm as confirmDialog } from '@tauri-apps/plugin-dialog';
import { relaunch } from '@tauri-apps/plugin-process';

const getSettingsMock = getSettings as unknown as ReturnType<typeof vi.fn>;
const peekBackupMock = peekBackup as unknown as ReturnType<typeof vi.fn>;
const restoreDataMock = restoreData as unknown as ReturnType<typeof vi.fn>;
const validateBackupMock = validateBackup as unknown as ReturnType<typeof vi.fn>;
const wipeDataMock = wipeData as unknown as ReturnType<typeof vi.fn>;
const relaunchMock = relaunch as unknown as ReturnType<typeof vi.fn>;
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
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

/** Drive the real page through the real DOM to the point where doRestore() has run to completion:
 *  pick a passphrase-protected backup, type the passphrase, click Restore, consent.
 *  NOTE: validateBackup's behaviour is deliberately NOT set here — each test owns it, so a
 *  rejection staged before render survives (mockResolvedValue here would silently overwrite it). */
/** Exact type of `render(SettingsPage)` — `ReturnType<typeof render>` picks the generic's default
 *  instantiation, which is not this page's result type (svelte-check errors, vitest cannot see it). */
type Rendered = ReturnType<typeof renderSettings>;
const renderSettings = () => render(SettingsPage);

async function driveToRestoreForm(r: Rendered) {
	getSettingsMock.mockResolvedValue(OK_SETTINGS);
	peekBackupMock.mockResolvedValue(true);
	wipeDataMock.mockResolvedValue(undefined);
	restoreDataMock.mockResolvedValue({ npub: 'npub1restored', npub_short: 'npub1res…', share_code: 'hbk1y', key_storage: 'plain-file' });
	openMock.mockResolvedValue(BACKUP_PATH);
	confirmMock.mockResolvedValue(true);
	relaunchMock.mockResolvedValue(undefined);
	primeIdentity();

	const { getByRole } = r;
	await waitFor(() => expect((getByRole('button', { name: /restore from backup/i }) as HTMLButtonElement).disabled).toBe(false));

	await fireEvent.click(getByRole('button', { name: /restore from backup/i }));
	await tick();
	// pickRestore ran: the passphrase input is now shown (peekBackup said the archive is
	// encrypted). No aria-label, so select by the placeholder both layout variants share.
	const passInput = document.querySelector<HTMLInputElement>('input[placeholder="Backup passphrase"]');
	// `expect(...).toBeTruthy()` asserts at runtime but does not NARROW for TS, so the call below
	// stays `HTMLInputElement | null`. Throw instead: same failure message, and non-null after.
	if (!passInput) throw new Error('restore passphrase input should be visible');
	await fireEvent.input(passInput, { target: { value: 'correct-horse-battery' } });
	await tick();
	return passInput;
}

async function driveRestore(r: Rendered) {
	await driveToRestoreForm(r);
	await fireEvent.click(r.getByRole('button', { name: /^restore$/i }));
	await tick();
}

describe('QURATOR-126 — validate the backup BEFORE wiping (INV-8)', () => {
	it('validateBackup rejects (typo\'d passphrase) → wipeData is NEVER called, nothing is restored, the error is surfaced, and `restoring` resets', async () => {
		validateBackupMock.mockRejectedValue(new Error('wrong passphrase (kdf mismatch)'));
		const r = render(SettingsPage);
		await driveRestore(r);

		// The whole point of the row: the pre-flight rejected, so the destructive half never ran.
		await waitFor(() => expect(validateBackupMock).toHaveBeenCalledTimes(1));
		expect(wipeDataMock).not.toHaveBeenCalled();
		expect(restoreDataMock).not.toHaveBeenCalled();
		expect(relaunchMock).not.toHaveBeenCalled();

		// The pre-flight saw exactly the passphrase/path the real restore would have used —
		// validating a DIFFERENT archive than the one you then wipe for would be a lookalike guard.
		expect(validateBackupMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);

		// The user is told why, via the page's existing toast(String(e), 'error') pattern.
		// Read the store with `get`, as q100-picture-error-toast.test.ts does — a manual
		// subscribe/unsub leaves TS narrowing `msg` to `null` (the callback write is invisible
		// to control-flow analysis), which vitest runs happily and svelte-check rejects.
		await waitFor(() => {
			expect(get(toastMessage)?.text).toContain('wrong passphrase');
			expect(get(toastMessage)?.kind).toBe('error');
		});

		// `restoring` went back to false: the button reads "Restore from backup", not "Restoring…".
		await waitFor(() => expect(r.getByRole('button', { name: /restore from backup/i })).toBeTruthy());
		expect(r.queryByRole('button', { name: /restoring…/i })).toBeNull();
	});

	it('validateBackup resolves → wipeData then restoreData run, with the same passphrase/path', async () => {
		validateBackupMock.mockResolvedValue(undefined);
		const r = render(SettingsPage);
		await driveRestore(r);

		await waitFor(() => expect(wipeDataMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		expect(validateBackupMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
		expect(restoreDataMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
		// The wipe happens only AFTER the pre-flight cleared the archive…
		const validateAt = validateBackupMock.mock.invocationCallOrder[0];
		const wipeAt = wipeDataMock.mock.invocationCallOrder[0];
		expect(wipeAt).toBeGreaterThan(validateAt);
		// …and the restore only AFTER the wipe — a fix that wiped but never restored would leave
		// the device empty, so the destructive half is pinned too, not just the validate gate.
		const restoreAt = restoreDataMock.mock.invocationCallOrder[0];
		expect(restoreAt).toBeGreaterThan(wipeAt);
	});

	// ── Post-fix hardening: the preflight must be about the inputs the restore actually uses. ──
	//
	// The original QURATOR-126 fix validated the CURRENT reactive values and then RE-READ them
	// after `wipeData`, while the passphrase input and Cancel stayed enabled. An adversarial
	// review flagged three identity-loss doors that stayed open behind the headline fix.

	it('editing the passphrase input WHILE validateBackup is pending does not change what restoreData receives — both calls see the SAME captured passphrase (else the wipe is for an archive that was never validated)', async () => {
		// Stage: validateBackup parks on a promise we control, so the edit lands strictly between
		// the preflight call and the wipe/restore half.
		let releaseValidate!: (v: undefined) => void;
		validateBackupMock.mockImplementation(() => new Promise<undefined>(res => { releaseValidate = res; }));
		const r = render(SettingsPage);
		const passInput = await driveToRestoreForm(r);

		await fireEvent.click(r.getByRole('button', { name: /^restore$/i }));
		await tick();
		await waitFor(() => expect(validateBackupMock).toHaveBeenCalledTimes(1));

		// Mid-flight edit: the input is still mounted, the user retypes the passphrase.
		await fireEvent.input(passInput, { target: { value: 'WRONG-edited-midflight' } });
		await tick();
		releaseValidate(undefined); // the preflight PASSES — for the ORIGINAL passphrase

		await waitFor(() => expect(wipeDataMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		// The whole row: the restore half must be provably about the SAME inputs the preflight
		// cleared. Re-reading the live field after the edit would hand restoreData the wrong key
		// on an already-wiped device.
		expect(validateBackupMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
		expect(restoreDataMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
		expect(restoreDataMock).not.toHaveBeenCalledWith('WRONG-edited-midflight', BACKUP_PATH);
	});

	it('clicking Cancel WHILE validateBackup is pending cannot orphan the restore — restoreData still receives the CAPTURED path, never null (else the wipe leaves a device nothing restores into)', async () => {
		let releaseValidate!: (v: undefined) => void;
		validateBackupMock.mockImplementation(() => new Promise<undefined>(res => { releaseValidate = res; }));
		const r = render(SettingsPage);
		await driveToRestoreForm(r);

		await fireEvent.click(r.getByRole('button', { name: /^restore$/i }));
		await tick();
		await waitFor(() => expect(validateBackupMock).toHaveBeenCalledTimes(1));

		// Cancel mid-validation. The old handler nulled `restorePath`; the restore half then ran
		// `restoreData(pass, null)` after the wipe.
		await fireEvent.click(r.getByRole('button', { name: /^cancel$/i }));
		await tick();
		releaseValidate(undefined);

		await waitFor(() => expect(wipeDataMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		expect(restoreDataMock).not.toHaveBeenCalledWith(expect.anything(), null);
		// The in-flight restore still uses the captured path.
		expect(restoreDataMock).toHaveBeenCalledWith('correct-horse-battery', BACKUP_PATH);
	});

	it('two rapid clicks produce exactly ONE wipeData — `restoring` is a re-entry guard, not a label (two overlapping confirms = two destructive flows)', async () => {
		validateBackupMock.mockResolvedValue(undefined);
		const r = render(SettingsPage);
		await driveToRestoreForm(r);

		const restoreBtn = r.getByRole('button', { name: /^restore$/i }) as HTMLButtonElement;
		// Both clicks land while `confirm` is still pending — i.e. before either invocation could
		// have set `restoring` under the old ordering. mockResolvedValue defers the resolution to
		// a microtask, so the two synchronous clicks genuinely overlap the dialog await.
		await fireEvent.click(restoreBtn);
		await fireEvent.click(restoreBtn);
		await tick();

		await waitFor(() => expect(wipeDataMock).toHaveBeenCalledTimes(1), { timeout: 4000 });
		// Exactly one destructive flow: one confirm-consented wipe, one restore.
		expect(wipeDataMock).toHaveBeenCalledTimes(1);
		expect(restoreDataMock).toHaveBeenCalledTimes(1);
		expect(validateBackupMock).toHaveBeenCalledTimes(1);
	});

	it('declining the confirm dialog leaves the UI usable — `restoring` resets, so the Restore button is not permanently locked', async () => {
		validateBackupMock.mockResolvedValue(undefined);
		const r = render(SettingsPage);
		await driveToRestoreForm(r);
		// Stage AFTER driveToRestoreForm — it sets confirmMock to `true` itself and would silently
		// overwrite a decline staged before render (same trap the header comment pins for validate).
		confirmMock.mockResolvedValue(false);
		await fireEvent.click(r.getByRole('button', { name: /^restore$/i }));
		await tick();

		await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(1));
		expect(wipeDataMock).not.toHaveBeenCalled();
		expect(validateBackupMock).not.toHaveBeenCalled();
		// `restoring` went back to false on the decline path: the Restore button re-enables.
		await waitFor(() => expect((r.getByRole('button', { name: /^restore$/i }) as HTMLButtonElement).disabled).toBe(false));
	});
});
