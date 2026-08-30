// @vitest-environment jsdom
// QURATOR-141 — there was NO way to ADD a block. dmBlock's only call site was chat's
// handleBlock(r: DmRequestView), which requires an existing DM Request; Settings could list and
// remove from the blocklist but never grow it. This pins the new Settings block-by-npub input:
// a valid npub reaches dm_block, an INVALID one is rejected at the input (never stored — a
// typo'd block is silent and useless), and the list refreshes after a successful block.
//
// Behavioural mount (render(Page) + fireEvent), not a source-scan — per CLAUDE.md P-4.
// The npub strings are the same fake-but-well-shaped ones the Q94 file uses; the validation
// gate under test here is the UI's CONSULTATION of validateShareCode, not bech32 itself.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import SettingsPage from './+page.svelte';
import { identity, profile } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const HOSTILE = 'npub1hostileeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

vi.mock('$lib/api.js', () => ({
	generateKeypair: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ relay_urls: [], allow_dms: true }),
	saveSettings: vi.fn().mockResolvedValue(undefined),
	importNsec: vi.fn(),
	backupData: vi.fn(),
	peekBackup: vi.fn(),
	restoreData: vi.fn(),
	wipeData: vi.fn(),
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
	// The three under test:
	dmBlock: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn(),
	shareCodeInfo: vi.fn(),
}));

import { dmBlock, dmBlockedList, validateShareCode } from '$lib/api.js';
const blockMock = dmBlock as unknown as ReturnType<typeof vi.fn>;
const blockedListMock = dmBlockedList as unknown as ReturnType<typeof vi.fn>;
const validateMock = validateShareCode as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	profile.set(null);
});

/** The npub input is the only hb-mono text input whose placeholder mentions npub-to-block. */
function npubInput(container: HTMLElement): HTMLInputElement {
	const el = container.querySelector('input[placeholder^="npub1"]') as HTMLInputElement | null;
	expect(el, 'the block-by-npub input renders in Settings').toBeTruthy();
	return el!;
}

function blockButton(container: HTMLElement): HTMLButtonElement {
	const btn = [...container.querySelectorAll('button')].find(
		(b) => b.textContent?.trim() === 'Block',
	) as HTMLButtonElement | undefined;
	expect(btn, 'the Block button renders').toBeTruthy();
	return btn!;
}

async function mount() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	profile.set({ display_name: 'Me', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' });
	return render(SettingsPage);
}

describe('QURATOR-141 — Settings block-by-npub input', () => {
	it('a valid npub calls dmBlock with that npub and refreshes the blocklist', async () => {
		validateMock.mockResolvedValue(true);
		blockedListMock.mockResolvedValueOnce([]).mockResolvedValueOnce([HOSTILE]);
		const { container } = await mount();

		const input = npubInput(container);
		await fireEvent.input(input, { target: { value: HOSTILE } });
		await fireEvent.click(blockButton(container));
		await tick();

		await waitFor(() => expect(validateMock).toHaveBeenCalledWith(HOSTILE));
		await waitFor(() => expect(blockMock).toHaveBeenCalledTimes(1));
		expect(blockMock).toHaveBeenCalledWith(HOSTILE);
		// The list is re-read after the block, so the new row appears without a restart.
		await waitFor(() => expect(blockedListMock).toHaveBeenCalledTimes(2));
	});

	it('an INVALID npub is rejected at the input — dmBlock is never called', async () => {
		validateMock.mockResolvedValue(false);
		const { container } = await mount();

		const input = npubInput(container);
		await fireEvent.input(input, { target: { value: 'npub1notarealkey' } });
		await fireEvent.click(blockButton(container));
		await tick();

		await waitFor(() => expect(validateMock).toHaveBeenCalledWith('npub1notarealkey'));
		// THE assertion: nothing is stored. A typo'd block is silent and useless.
		expect(blockMock).not.toHaveBeenCalled();
		// The input is NOT cleared either — the user gets to fix the typo in place.
		expect(npubInput(container).value).toBe('npub1notarealkey');
	});

	it('blocking your own npub is refused', async () => {
		validateMock.mockResolvedValue(true);
		const { container } = await mount();

		const input = npubInput(container);
		await fireEvent.input(input, { target: { value: ME } });
		await fireEvent.click(blockButton(container));
		await tick();

		await waitFor(() => expect(validateMock).toHaveBeenCalled());
		expect(blockMock).not.toHaveBeenCalled();
	});
});
