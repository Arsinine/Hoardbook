// The Settings identity-card view-model (spec §Identity Management / §10 Settings). QURATOR-379:
// the card keeps ONE key line — the truncated npub, rendered straight from IdentityInfo — plus the
// at-rest storage status. The share-code and npub rows, their hints, and the no-recovery notice
// are gone (the share code stays reachable through Chat's "Share my code").

import type { IdentityInfo } from './types.js';

export interface KeyView {
	/** True when the key is a 0600 plain file (no OS keyring) — drives the storage warning. */
	showStorageWarning: boolean;
}

export function keyView(id: KeyViewInput): KeyView {
	const plainFile = id.key_storage === 'plain-file';
	return {
		showStorageWarning: plainFile,
	};
}

/** The fields the view-model reads. Kept narrow so the card cannot grow a second key row without
 *  touching this interface. */
interface KeyViewInput {
	key_storage: IdentityInfo['key_storage'];
}
