import { describe, expect, it, vi } from 'vitest';
import {
	resolveInitialStep,
	simulateObSaveName,
	shouldShowBackupPrompt,
	shouldShowPrivacyNotice,
	attemptDownload,
	proceedImport,
} from './onboarding.js';

describe('onboarding wizard — step resolution (ported to npub)', () => {
	it('wizard_not_shown_if_identity_exists: identity present → step 4', () => {
		expect(resolveInitialStep({ npub: 'npub1abc' }).obStep).toBe(4);
	});

	it('wizard_not_shown_if_identity_exists: identity with no profile also → step 4', () => {
		// Skipped step 2 on first launch; relaunch must not re-show the wizard.
		expect(resolveInitialStep({ npub: 'npub1xyz' }).obStep).toBe(4);
	});

	it('no identity → step 1 (wizard shown)', () => {
		expect(resolveInitialStep(null).obStep).toBe(1);
	});
});

describe('onboarding wizard — step 2 save behaviour', () => {
	it('step2_skip_writes_nothing: empty display_name skips to step 3 without saving', async () => {
		const saveFn = vi.fn().mockResolvedValue(undefined);
		const result = await simulateObSaveName('', saveFn);
		expect(result.savedCalled).toBe(false);
		expect(result.nextStep).toBe(3);
		expect(saveFn).not.toHaveBeenCalled();
	});

	it('step2_skip_writes_nothing: whitespace-only name also skips without saving', async () => {
		const saveFn = vi.fn().mockResolvedValue(undefined);
		const result = await simulateObSaveName('   ', saveFn);
		expect(result.savedCalled).toBe(false);
		expect(saveFn).not.toHaveBeenCalled();
	});

	it('non-empty display_name calls save and advances to step 3', async () => {
		const saveFn = vi.fn().mockResolvedValue(undefined);
		const result = await simulateObSaveName('DataHoarder_42', saveFn);
		expect(result.savedCalled).toBe(true);
		expect(result.nextStep).toBe(3);
		expect(saveFn).toHaveBeenCalledOnce();
	});
});

describe('onboarding — backup prompt after generate', () => {
	it('backup_prompt_shown_after_generate: shown once generated, until backed up', () => {
		expect(shouldShowBackupPrompt({ generated: true, backedUp: false })).toBe(true);
	});
	it('not shown before generate, nor after a backup is taken', () => {
		expect(shouldShowBackupPrompt({ generated: false, backedUp: false })).toBe(false);
		expect(shouldShowBackupPrompt({ generated: true, backedUp: true })).toBe(false);
	});
});

describe('onboarding — one-time pre-download privacy notice', () => {
	it('privacy_notice_shown_once_before_first_download: shown iff not yet acknowledged', () => {
		expect(shouldShowPrivacyNotice({ privacy_notice_acknowledged: false })).toBe(true);
		expect(shouldShowPrivacyNotice({ privacy_notice_acknowledged: true })).toBe(false);
	});

	it('privacy_notice_gate_blocks_download_until_acknowledged', async () => {
		const download = vi.fn().mockResolvedValue(7);
		// Not acknowledged → the download command must NOT fire.
		const blocked = await attemptDownload({ acknowledged: false, download });
		expect(blocked).toEqual({ status: 'needs-ack' });
		expect(download).not.toHaveBeenCalled();
		// Acknowledged → the download proceeds.
		const ok = await attemptDownload({ acknowledged: true, download });
		expect(ok).toEqual({ status: 'downloading', id: 7 });
		expect(download).toHaveBeenCalledOnce();
	});
});

describe('onboarding — import existing key always warns about linking', () => {
	it('import_existing_key_flow_always_surfaces_linking_warning (no Qurator/public oracle)', async () => {
		const importKey = vi.fn().mockResolvedValue({ npub: 'npub1imported' });
		// Any key — public-looking, qurator-looking, random — must hit the warning first.
		for (const _key of ['npub1public', 'npub1qurator', 'nsec1random']) {
			const r = await proceedImport({ warningAcknowledged: false, importKey });
			expect(r).toEqual({ status: 'needs-warning' });
		}
		expect(importKey).not.toHaveBeenCalled();
		// Only after acknowledging the warning does the import commit.
		const done = await proceedImport({ warningAcknowledged: true, importKey });
		expect(done.status).toBe('imported');
		expect(importKey).toHaveBeenCalledOnce();
	});
});
