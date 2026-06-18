// Backup-export view-model (spec §Backup & Durability): the passphrase strength meter, the two
// export modes (passphrase-encrypted default vs plaintext-behind-a-warning), and the share-code
// export (text + QR). The crypto lives in the Rust core; this is UI logic only.

/** The passphrase floor, mirrored from `hb_core::backup::MIN_PASSPHRASE_LEN`. Measured on the
 *  NFKC-normalized form at BOTH layers so a passphrase can't pass the UI gate and fail the core. */
export const MIN_PASSPHRASE_LEN = 12;

export interface PassphraseStrength {
	/** False blocks the export (too short on the normalized form). */
	acceptable: boolean;
	/** 0 (empty) … 4 (strong) — drives the meter bar. */
	score: number;
	label: string;
	reason?: string;
}

export function passphraseStrength(raw: string): PassphraseStrength {
	const pass = raw.normalize('NFKC');
	const len = [...pass].length; // codepoint count on the normalized form
	if (len === 0) return { acceptable: false, score: 0, label: 'Empty' };
	if (len < MIN_PASSPHRASE_LEN) {
		return {
			acceptable: false,
			score: 1,
			label: 'Too short',
			reason: `Use at least ${MIN_PASSPHRASE_LEN} characters.`,
		};
	}
	let variety = 0;
	if (/[a-z]/.test(pass)) variety++;
	if (/[A-Z]/.test(pass)) variety++;
	if (/[0-9]/.test(pass)) variety++;
	if (/[^a-zA-Z0-9]/.test(pass)) variety++;
	const score = Math.min(4, Math.max(2, variety + (len >= 20 ? 1 : 0)));
	const label = score >= 4 ? 'Strong' : score === 3 ? 'Good' : 'Fair';
	return { acceptable: true, score, label };
}

export type BackupMode = 'passphrase' | 'plaintext';

export interface BackupModeOption {
	mode: BackupMode;
	label: string;
	/** Plaintext mode is warned (the archive *is* the identity) and is never the default. */
	warned: boolean;
	description: string;
}

export function backupModeOptions(): BackupModeOption[] {
	return [
		{
			mode: 'passphrase',
			label: 'Passphrase-encrypted (recommended)',
			warned: false,
			description: 'Portable across machines and encrypted with your passphrase.',
		},
		{
			mode: 'plaintext',
			label: 'Plaintext (advanced)',
			warned: true,
			description:
				'This file IS your identity — anyone who obtains it becomes you. Store it like a master key.',
		},
	];
}

export interface ShareCodeExport {
	text: string;
	/** The rendered QR (e.g. an SVG string). Derived from the share code via the injected encoder. */
	qr: string;
	/** The share code carries the browse-key, so it is secret — the UI warns on export. */
	warned: boolean;
}

/** Bundle the share code as text + QR. The encoder is injected so tests can stub it and the Svelte
 *  component supplies the real `qrcode` renderer. */
export function shareCodeExport(code: string, qrEncode: (s: string) => string): ShareCodeExport {
	return { text: code, qr: qrEncode(code), warned: true };
}
