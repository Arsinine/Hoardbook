// The Settings key view-model (spec §Identity Management / §10 Settings). Surfaces the npub and the
// share code, plus the at-rest storage status and the no-recovery / Linux-0600 warnings. The
// browse-key is NEVER rendered as raw bytes — only inside the `hbk` share code.

import type { IdentityInfo } from './types.js';

export interface KeyRow {
	label: string;
	value: string;
	/** A secret-bearing value the UI should treat carefully (the share code carries the browse-key). */
	sensitive: boolean;
}

export interface KeyView {
	rows: KeyRow[];
	/** "Encrypted by your OS" (Windows DPAPI) or "Protected file (0600)" (Linux/macOS). */
	storageLabel: string;
	/** True when the key is a 0600 plain file (no OS keyring) — drives the storage warning. */
	showStorageWarning: boolean;
	/** Always shown: a lost npub cannot be recovered (backup is the only protection). */
	noRecoveryNotice: string;
}

const NO_RECOVERY =
	'Your npub is your identity and cannot be recovered if lost — there is no reset. ' +
	'Back up your profile and keep it somewhere safe.';

export function keyView(id: IdentityInfo): KeyView {
	const plainFile = id.key_storage === 'plain-file';
	return {
		rows: [
			{ label: 'Your npub', value: id.npub, sensitive: false },
			// The share code embeds the browse-key — secret. The raw browse-key is never a row.
			{ label: 'Share code', value: id.share_code, sensitive: true },
		],
		storageLabel: plainFile ? 'Protected file (0600)' : 'Encrypted by your OS',
		showStorageWarning: plainFile,
		noRecoveryNotice: NO_RECOVERY,
	};
}
