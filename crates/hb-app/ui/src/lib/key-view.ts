// The Settings key view-model (spec §Identity Management / §10 Settings). Surfaces the npub and the
// share code, plus the at-rest storage status and the no-recovery / Linux-0600 warnings. The
// browse-key is NEVER rendered as raw bytes — only inside the `hbk` share code.

import type { IdentityInfo } from './types.js';

export interface KeyRow {
	label: string;
	value: string;
	/** A secret-bearing value the UI should treat carefully (the share code carries the browse-key). */
	sensitive: boolean;
	/** Optional sub-note clarifying the row's purpose (public handle vs private access pass). */
	hint?: string;
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
			// Lead with the share code: it both finds the user and unlocks browsing (it embeds the
			// npub). It carries the browse-key, so it is PRIVATE — hand it only to people you trust.
			{
				label: 'Share code',
				value: id.share_code,
				sensitive: true,
				hint: 'Private — give it to people you want browsing your collections. It also lets them find, add, and DM you. Keep it off public threads.',
			},
			// The npub stays: it is the SAFE-to-post-publicly handle (no browse-key), unlike the share code.
			{
				label: 'Your npub',
				value: id.npub,
				sensitive: false,
				hint: 'Public — post it anywhere so people can find, add, and DM you. It does not unlock browsing.',
			},
		],
		storageLabel: plainFile ? 'Protected file (0600)' : 'Encrypted by your OS',
		showStorageWarning: plainFile,
		noRecoveryNotice: NO_RECOVERY,
	};
}
