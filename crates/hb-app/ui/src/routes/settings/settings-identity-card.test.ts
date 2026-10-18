// @vitest-environment jsdom
// QURATOR-379 — the Settings Identity card regression.
//
//   1. The card is npub-shaped: the TRUNCATED npub renders, the FULL npub never does, and the
//      share code (label or value) is gone — it stays reachable through Chat only.
//   2. The "Copy npub" icon button copies the FULL npub.
//   3. The storage warning is the one shortened sentence, only for plain-file storage.
//   4. The Nostr-key import is DELETED (owner, Option A): no "Use my Nostr key" / "Import key" /
//      "Use a different Nostr key" control, with and without an identity loaded. Switching keys
//      is Danger zone → Wipe data → the wizard's import step, which has its own tests
//      (onboarding-wizard.test.ts — untouched here).
//
// MUTATIONS THAT MUST RED (both seen red, restored by re-applying the inverse edit):
//   M-1  in ./+page.svelte, change `.identity-npub-text`'s body `{idNpubShort}` back to the
//        share-code value (`{$identity?.share_code}`) → test 1 reds (short npub absent, share
//        code present).
//   M-2  re-insert a "Use my Nostr key" button into the page markup (e.g. just above the
//        `<!-- QURATOR-379 (owner, Option A): … import section is gone` comment) → test 2 reds
//        (both branches: with identity and without).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import SettingsPage from './+page.svelte';
import { identity, profile } from '$lib/stores.js';

const NPUB = 'npub1q0xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx9';
const NPUB_SHORT = 'npub1q0x…xxx9';
const SHARE_CODE = 'hbk1abc123';

vi.mock('$lib/api.js', () => ({
	generateKeypair: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({
		relay_urls: ['wss://relay.example.com'],
		allow_dms: true,
		privacy_notice_acknowledged: true,
		last_seen_version: '',
		snapshot_auto_update: true,
		snapshot_reconcile_poll: false,
		show_online_count: true,
		discoverable: false,
		big_relay_url: '',
	}),
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
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

import { get } from 'svelte/store';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	profile.set(null);
});

function primeIdentity(keyStorage: 'os-encrypted' | 'plain-file') {
	identity.set({ npub: NPUB, npub_short: NPUB_SHORT, share_code: SHARE_CODE, key_storage: keyStorage });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
}

describe('Settings Identity card (QURATOR-379)', () => {
	it('shows the truncated npub — never the full npub, never the share code', async () => {
		primeIdentity('os-encrypted');
		const { getByRole } = render(SettingsPage);

		await waitFor(() => expect(document.body.textContent).toContain(NPUB_SHORT));
		const text = document.body.textContent ?? '';
		expect(text).not.toContain(NPUB);
		expect(text).not.toContain('Share code');
		expect(text).not.toContain(SHARE_CODE);
		// The one copy affordance is the npub one; a share-code copy affordance would be a
		// second [aria-label] on the card.
		expect(getByRole('button', { name: 'Copy npub' })).toBeTruthy();
		expect(document.body.querySelector('[aria-label="Copy share code"]')).toBeNull();
	});

	it('"Copy npub" copies the FULL npub', async () => {
		primeIdentity('os-encrypted');
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.assign(navigator, { clipboard: { writeText } });
		const { getByRole } = render(SettingsPage);

		const btn = await waitFor(() => getByRole('button', { name: 'Copy npub' }));
		await fireEvent.click(btn);
		expect(writeText).toHaveBeenCalledWith(NPUB);
	});

	it('the storage warning is the ONE shortened sentence, only for plain-file storage', async () => {
		primeIdentity('plain-file');
		render(SettingsPage);
		await waitFor(() =>
			expect(document.body.textContent).toContain(
				'Your key is stored as a protected file, not in an OS keyring. Anyone with access to your user account can read it.',
			),
		);
		// No residue of the old red box / hints.
		const text = document.body.textContent ?? '';
		expect(text).not.toMatch(/no recovery|No recovery|cannot be restored/i);

		cleanup();
		primeIdentity('os-encrypted');
		render(SettingsPage);
		await waitFor(() => expect(getByRoleMaybe('Copy npub')).toBeTruthy());
		expect(document.body.textContent).not.toContain('not in an OS keyring');
	});
});

function getByRoleMaybe(name: string): HTMLElement | null {
	return [...document.body.querySelectorAll('button')].find((b) => b.getAttribute('aria-label') === name) ?? null;
}

describe('the Nostr-key import is deleted from Settings (QURATOR-379 Option A)', () => {
	it('renders no import control, with an identity loaded', async () => {
		primeIdentity('os-encrypted');
		render(SettingsPage);
		await waitFor(() => expect(document.body.textContent).toContain(NPUB_SHORT));
		const text = document.body.textContent ?? '';
		expect(text).not.toMatch(/Use my Nostr key/i);
		expect(text).not.toMatch(/Import key/i);
		expect(text).not.toMatch(/Use a different Nostr key/i);
	});

	it('renders no import control in the no-identity view either', async () => {
		identity.set(null);
		profile.set(null);
		const r = render(SettingsPage);
		await waitFor(() => expect(r.getByRole('button', { name: /generate identity/i })).toBeTruthy());
		const text = document.body.textContent ?? '';
		expect(text).not.toMatch(/Use my Nostr key/i);
		expect(text).not.toMatch(/Import key/i);
		expect(text).not.toMatch(/Use a different Nostr key/i);
	});
});
