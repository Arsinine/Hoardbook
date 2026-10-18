// @vitest-environment jsdom
// Step 1 offers an existing Nostr key as an equal choice to a new one (owner, 2026-10-18: seeding
// starts on Nostr, so most first users already have a key). The import stays behind the linking
// warning: the Import button is disabled until it is acknowledged.
//
// QURATOR-164 baseline disclosure — now a wizard step (step 3 of 4), not a startup dialog: an
// unprompted modal at launch read as an error. Drives the real Home wizard from step 1 and pins
// that the sharing step shows, that passing it records `serving_notice_acknowledged: true` with
// every other settings field intact, and that the wizard moves on to the collection step.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import HomePage from './+page.svelte';
import { identity, profile, collections, appReady, homeDraft, identityLoadError } from '$lib/stores.js';
import type { IdentityInfo } from '$lib/types.js';

const IDENT: IdentityInfo = {
	npub: 'npub1share' + 'a'.repeat(52),
	npub_short: 'npub1share…aaaa',
	share_code: 'hbk1sharetest',
	key_storage: 'os-encrypted',
};

const SETTINGS = { relay_urls: ['wss://relay.example.com'], swarm_caching: false, serving_notice_acknowledged: false };

const { getSettingsMock, saveSettingsMock, importNsecMock } = vi.hoisted(() => ({
	importNsecMock: vi.fn(),
	getSettingsMock: vi.fn(),
	saveSettingsMock: vi.fn(async (_s: Record<string, unknown>) => undefined),
}));

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		generateKeypair: vi.fn(async () => IDENT),
		importNsec: importNsecMock,
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
		getSettings: getSettingsMock,
		saveSettings: saveSettingsMock,
	};
});

afterEach(() => {
	cleanup();
	identity.set(null);
	profile.set(null);
	collections.set([]);
	appReady.set(false);
	homeDraft.set(null);
	identityLoadError.set(null);
	vi.clearAllMocks();
});

describe('onboarding wizard', () => {
	it('step 3 explains background sharing; Continue records the acknowledgement and moves to step 4', async () => {
		getSettingsMock.mockResolvedValue({ ...SETTINGS });
		identity.set(null);
		appReady.set(true);
		const r = render(HomePage);
		await tick();

		await fireEvent.click(await r.findByText('Create a new key'));
		await fireEvent.click(await r.findByText("I'll do it later"));
		await fireEvent.click(await r.findByText('Skip'));

		await r.findByText('Passing it on');
		expect(r.getByText('Step 3 of 4')).toBeTruthy();
		expect(r.getByText(/sends them your copy in the background/)).toBeTruthy();
		expect(saveSettingsMock).not.toHaveBeenCalled();

		// MUTATION (P-10): in routes/+page.svelte, in `obAckSharing`, delete the line
		// `acknowledgeServingNotice().catch(() => { });` — this test reds on the save assert below.
		await fireEvent.click(r.getByText('Continue →'));
		await r.findByText('Add your first collection');
		await waitFor(() => expect(saveSettingsMock).toHaveBeenCalledTimes(1));
		const saved = saveSettingsMock.mock.calls[0]![0];
		expect(saved.serving_notice_acknowledged).toBe(true);
		expect(saved.relay_urls).toEqual(['wss://relay.example.com']);
		expect(saved.swarm_caching).toBe(false);
	});

	it('a failed settings write does not strand the user on the sharing step', async () => {
		getSettingsMock.mockRejectedValue(new Error('settings unreadable'));
		identity.set(null);
		appReady.set(true);
		const r = render(HomePage);
		await tick();

		await fireEvent.click(await r.findByText('Create a new key'));
		await fireEvent.click(await r.findByText("I'll do it later"));
		await fireEvent.click(r.getByText('Skip'));
		await fireEvent.click(await r.findByText('Continue →'));
		await r.findByText('Add your first collection');
	});

	it('step 1: "Use my Nostr key" imports behind the linking warning and lands on the npub + backup screen', async () => {
		importNsecMock.mockResolvedValue(IDENT);
		identity.set(null);
		appReady.set(true);
		const r = render(HomePage);
		await tick();

		await fireEvent.click(await r.findByText('Use my Nostr key', { selector: 'button' }));
		const nsecInput = r.getByLabelText('Nostr key') as HTMLInputElement;
		await fireEvent.input(nsecInput, { target: { value: '  nsec1abc  ' } });
		const importBtn = r.getByText('Import key').closest('button')!;
		// MUTATION (P-10): in routes/+page.svelte, on the modal's Import button, delete
		// `!obImportWarnAck || ` from its `disabled` expression — this test reds here.
		expect(importBtn.disabled).toBe(true);

		await fireEvent.click(r.getByLabelText(/I understand/));
		expect(importBtn.disabled).toBe(false);
		await fireEvent.click(importBtn);

		expect(r.queryByLabelText('Key password')).toBeNull();
		await waitFor(() => expect(importNsecMock).toHaveBeenCalledWith('nsec1abc', ''));
		await r.findByText('Your identity is ready');
		// QURATOR-379: the ready screen shows the NPUB (one line, truncated in CSS but complete in
		// the DOM); the share code never appears — it stays reachable through Chat only.
		expect(r.getByText(IDENT.npub)).toBeTruthy();
		expect(r.queryByText(IDENT.share_code)).toBeNull();
	});

	it('step 1: an ncryptsec key asks for its password and passes it to the import', async () => {
		importNsecMock.mockResolvedValue(IDENT);
		identity.set(null);
		appReady.set(true);
		const r = render(HomePage);
		await tick();

		await fireEvent.click(await r.findByText('Use my Nostr key', { selector: 'button' }));
		await fireEvent.click(r.getByLabelText(/I understand/));
		await fireEvent.input(r.getByLabelText('Nostr key'), { target: { value: 'ncryptsec1qgg9xyz' } });
		const importBtn = r.getByText('Import key').closest('button')!;
		// MUTATION (P-10): in routes/+page.svelte, on the modal's Import button, delete
		// `(isEncryptedKey(obImportNsec) && !obImportPassword) || ` from `disabled` — this reds here.
		expect(importBtn.disabled).toBe(true);

		await fireEvent.input(r.getByLabelText('Key password'), { target: { value: 'hunter2' } });
		expect(importBtn.disabled).toBe(false);
		await fireEvent.click(importBtn);
		await waitFor(() => expect(importNsecMock).toHaveBeenCalledWith('ncryptsec1qgg9xyz', 'hunter2'));
	});

	it('every step after the first has Back, and step 2 has exactly one Skip', async () => {
		getSettingsMock.mockResolvedValue({ ...SETTINGS });
		identity.set(null);
		appReady.set(true);
		const r = render(HomePage);
		await tick();

		await fireEvent.click(await r.findByText('Create a new key'));
		await fireEvent.click(await r.findByText("I'll do it later"));
		await r.findByText('Name yourself');
		expect(r.getAllByText('Skip')).toHaveLength(1);

		await fireEvent.click(r.getByText('Skip'));
		await fireEvent.click(await r.findByText('Continue →'));
		await r.findByText('Add your first collection');

		await fireEvent.click(r.getByText('← Back'));
		await r.findByText('Passing it on');
		await fireEvent.click(r.getByText('← Back'));
		await r.findByText('Name yourself');
		await fireEvent.click(r.getByText('← Back'));
		// Step 1 revisited after the key exists: the npub and backup, not a second "create".
		await r.findByText('Your identity is ready');
		expect(r.queryByText('Create a new key')).toBeNull();
	});
});

