import { describe, expect, it } from 'vitest';
import { keyView } from './key-view.js';
import type { IdentityInfo } from './types.js';

const BROWSE_HEX = 'bb'.repeat(32);

function id(storage: 'os-encrypted' | 'plain-file'): IdentityInfo {
	return {
		npub: 'npub1example',
		npub_short: 'npub1ex…mple',
		// the share code carries the browse-key; the raw browse-key never appears separately
		share_code: `hbk1${BROWSE_HEX}`,
		key_storage: storage,
	};
}

describe('settings key-view (QURATOR-379 shape)', () => {
	// MUTATION (P-10): in key-view.ts, change the return to
	// `{ showStorageWarning: !plainFile }` (invert) — the first test reds on the false branch,
	// the second on the true branch.
	it('key_view_reports_only_the_storage_status', () => {
		const v = keyView(id('os-encrypted'));
		// The rows/noRecoveryNotice surface is gone with the share-code row (QURATOR-379).
		expect(v).toEqual({ showStorageWarning: false });
	});

	it('linux_storage_warning_shown_when_plain_file', () => {
		expect(keyView(id('plain-file')).showStorageWarning).toBe(true);
		expect(keyView(id('os-encrypted')).showStorageWarning).toBe(false);
	});
});
