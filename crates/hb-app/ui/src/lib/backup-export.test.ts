import { describe, expect, it } from 'vitest';
import {
	MIN_PASSPHRASE_LEN,
	passphraseStrength,
	backupModeOptions,
	shareCodeExport,
} from './backup-export.js';

describe('backup export — passphrase strength meter', () => {
	it('passphrase_strength_meter_blocks_weak_or_short_passphrase', () => {
		expect(passphraseStrength('').acceptable).toBe(false);
		expect(passphraseStrength('short').acceptable).toBe(false); // < 12
		expect(passphraseStrength('a'.repeat(MIN_PASSPHRASE_LEN - 1)).acceptable).toBe(false);
		const ok = passphraseStrength('correct horse battery staple');
		expect(ok.acceptable).toBe(true);
		expect(ok.score).toBeGreaterThanOrEqual(2);
	});

	it('floor is measured on the NFKC-normalized form (matches the Rust core)', () => {
		// 12 composed 'é' (U+00E9) → 12 chars after NFKC → acceptable.
		expect(passphraseStrength('é'.repeat(12)).acceptable).toBe(true);
		// A string whose NFKC form is only 11 chars is blocked even if raw length looks longer
		// (a fullwidth digit normalizes to one ASCII char, so 11 of them = 11 chars).
		expect(passphraseStrength('１'.repeat(11)).acceptable).toBe(false);
	});
});

describe('backup export — modes', () => {
	it('backup_export_offers_passphrase_and_plaintext_modes_plaintext_warned', () => {
		const opts = backupModeOptions();
		const pass = opts.find((o) => o.mode === 'passphrase')!;
		const plain = opts.find((o) => o.mode === 'plaintext')!;
		expect(pass).toBeTruthy();
		expect(plain).toBeTruthy();
		expect(pass.warned).toBe(false);
		expect(plain.warned).toBe(true); // plaintext is always behind a warning
		// Passphrase is presented as the recommended default (first option).
		expect(opts[0].mode).toBe('passphrase');
	});
});

describe('backup export — share code (text + QR)', () => {
	it('share_code_export_renders_text_and_qr', () => {
		const code = 'hbk1examplecode';
		const out = shareCodeExport(code, (s) => `<svg>${s}</svg>`);
		expect(out.text).toBe(code);
		expect(out.qr).toBe(`<svg>${code}</svg>`); // QR derived from the code
		expect(out.warned).toBe(true); // carries the browse-key → secret
	});
});
