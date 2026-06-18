import { describe, expect, it } from 'vitest';
import { keyView } from './key-view.js';
import type { IdentityInfo } from './types.js';

const NODE_KEY = 'aa'.repeat(32);
const BROWSE_HEX = 'bb'.repeat(32);

function id(storage: 'os-encrypted' | 'plain-file'): IdentityInfo {
	return {
		npub: 'npub1example',
		npub_short: 'npub1ex…mple',
		// the share code carries the browse-key; the raw browse-key never appears separately
		share_code: `hbk1${BROWSE_HEX}`,
		key_storage: storage,
		iroh_node_key: NODE_KEY,
	};
}

describe('settings key-view', () => {
	it('key_view_renders_npub_node_key_share_code_and_storage_status', () => {
		const v = keyView(id('os-encrypted'));
		const labels = v.rows.map((r) => r.label);
		expect(labels).toContain('Your npub');
		expect(labels).toContain('iroh node key');
		expect(labels).toContain('Share code');
		// The node key row is the hex PUBLIC key.
		expect(v.rows.find((r) => r.label === 'iroh node key')?.value).toBe(NODE_KEY);
		// The browse-key is never a standalone row — only inside the (sensitive) share code.
		expect(labels).not.toContain('Browse key');
		expect(v.rows.find((r) => r.label === 'Share code')?.sensitive).toBe(true);
		expect(v.storageLabel).toBe('Encrypted by your OS');
		expect(v.noRecoveryNotice).toMatch(/cannot be recovered/i);
	});

	it('linux_storage_warning_shown_when_plain_file', () => {
		expect(keyView(id('plain-file')).showStorageWarning).toBe(true);
		expect(keyView(id('os-encrypted')).showStorageWarning).toBe(false);
	});
});
