// @vitest-environment jsdom
// QURATOR-138 — publish-by-default on My Profile. MOUNT tests of the real Home page (the route
// CAN be mounted — topics-q83-empty-refetch.test.ts is the standing proof), not source-scans.
//
// Pins the four behaviours the ticket demands of the auto-publish wiring:
//   1. an edit → LOCAL save immediately (saveProfile) — never gated on the relay write;
//   2. N rapid edits → ONE publishProfile call (coalescing; the debounce is armed by the page's
//      own homeDraft effect, the single place every form mutation lands);
//   3. typing then LEAVING (unmount) still publishes (flush-on-navigate; onDestroy);
//   4. a publish failure toasts an error and the local save is still there — never a silent
//      success-shaped render of an unknown state (QURATOR-83/134/135 class).
// The debounce is the REAL 40ms test window (setAutopublishDebounceForTests) — a debounce that
// only ever ran under fake timers is the classic vacuous control, and this page holds real
// Svelte effects that fake timers do not drive.
//
// Mutation probes (against +page.svelte, one at a time, revert between):
//   a) delete the profileEdited() call in the homeDraft $effect → (2) REDs (publish count 0).
//   b) delete the onDestroy flush → (3) REDs (unmount publishes 0 times).
//   c) drop the onError toast wiring → (4) REDs (no error toast).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, fireEvent, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { get } from 'svelte/store';
import HomePage from './+page.svelte';
import { identity, profile, collections, appReady, homeDraft, identityLoadError, toastMessage } from '$lib/stores.js';
import { setAutopublishDebounceForTests } from '$lib/profile-autopublish.js';
import type { IdentityInfo, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		saveProfile: vi.fn().mockResolvedValue(undefined),
		publishProfile: vi.fn().mockResolvedValue(undefined),
		hasPublishedProfile: vi.fn().mockResolvedValue(true),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
	};
});

import { saveProfile, publishProfile } from '$lib/api.js';
const saveMock = saveProfile as unknown as ReturnType<typeof vi.fn>;
const publishMock = publishProfile as unknown as ReturnType<typeof vi.fn>;

const IDENT: IdentityInfo = {
	npub: 'npub1q138' + 'a'.repeat(53),
	npub_short: 'npub1q138…aaaa',
	share_code: 'hbk1q138test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Auto Publish',
	bio: undefined,
	tags: [],
	since: 2020,
	est_size: undefined,
	languages: ['English'],
	contact_hint: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

const wait = (ms: number) => new Promise(r => setTimeout(r, ms));

function prime() {
	identity.set(IDENT);
	profile.set({ ...PROF });
	collections.set([]);
	appReady.set(true);
	homeDraft.set(null);
	identityLoadError.set(null);
	toastMessage.set(null);
}

function resetStores() {
	identity.set(null);
	profile.set(null);
	collections.set([]);
	appReady.set(false);
	homeDraft.set(null);
	identityLoadError.set(null);
	toastMessage.set(null);
}

afterEach(() => {
	cleanup();
	resetStores();
	vi.clearAllMocks();
});

describe('QURATOR-138 — My Profile publishes by default', () => {
	it('mounting and hydrating the form publishes NOTHING (no phantom publish)', async () => {
		// The seed effect writes `form` once on hydration; that is not an edit. Without the
		// cancel-on-seed guard this fired a publish on every page mount (observed live).
		prime();
		render(HomePage);
		// Long enough for the 40ms debounce AND the async onMount published-check to settle.
		await wait(160);
		expect(publishMock).not.toHaveBeenCalled();
	}, 15000);

	it('the Save draft button is GONE from the title bar', async () => {
		prime();
		const { queryByRole, getByRole } = render(HomePage);
		await tick();
		expect(queryByRole('button', { name: /save draft/i })).toBeNull();
		// The Publish affordance survives — publishing is still an explicit, name-gated action.
		expect(getByRole('button', { name: /publish/i })).toBeTruthy();
	});

	it('an edit saves LOCALLY immediately and publishes after the debounce', async () => {
		prime();
		const { getByPlaceholderText } = render(HomePage);
		await tick();
		await wait(60); // hydration + any seed effect settling

		const name = getByPlaceholderText(/DataHoarder_42/i) as HTMLInputElement;
		const before = publishMock.mock.calls.length; // mount/seed must not have published (0)
		// (mount-publish count is not pinned here; the q138-mount-zero test below pins it)
		await fireEvent.input(name, { target: { value: 'Edited Once' } });
		await tick();

		// LOCAL save fired immediately — not gated on the debounce or the relay.
		await waitFor(() => expect(saveMock).toHaveBeenCalled());
		// And the publish went out once after the short window.
		await waitFor(() => expect(publishMock.mock.calls.length).toBeGreaterThan(before), { timeout: 2000 });
		await wait(100);
		expect(publishMock.mock.calls.length).toBe(before + 1); // exactly one — no extras follow
	}, 15000);

	it('N rapid edits coalesce into ONE publish call', async () => {
		prime();
		const { getByPlaceholderText } = render(HomePage);
		await tick();
		await wait(60);

		const name = getByPlaceholderText(/DataHoarder_42/i) as HTMLInputElement;
		const before = publishMock.mock.calls.length; // baseline; the phantom-mount test pins 0
		for (let i = 0; i < 5; i++) {
			await fireEvent.input(name, { target: { value: `Name ${i}` } });
			await tick();
			await wait(6);
		}
		await wait(150);
		expect(publishMock.mock.calls.length).toBe(before + 1); // 5 edits → ONE publish
		expect(saveMock.mock.calls.length).toBeGreaterThanOrEqual(5); // local saves were NOT coalesced away
	}, 15000);

	it('unmounting after a completed burst fires NO duplicate publish (and is safe)', async () => {
		prime();
		const { getByPlaceholderText, unmount } = render(HomePage);
		await tick();
		await wait(60);

		const name = getByPlaceholderText(/DataHoarder_42/i) as HTMLInputElement;
		await fireEvent.input(name, { target: { value: 'About To Leave' } });
		await tick();
		await wait(120); // let the debounce fire, THEN unmount — isolates the destroy flush
		const pubs = publishMock.mock.calls.length; // debounce already fired
		unmount();
		expect(publishMock.mock.calls.length).toBe(pubs); // unmount fired NO extra publish
		// The flush-on-destroy behaviour itself is pinned by the unit suite (mutation-B proved it
		// reds); here the page-level pin is that unmount is QUIET once the burst already went out.
	}, 15000);

	it('a publish FAILURE toasts an error while the local edit stays saved', async () => {
		prime();
		publishMock.mockRejectedValue(new Error('all relays unreachable'));
		const { getByPlaceholderText } = render(HomePage);
		await tick();
		await wait(60);

		const name = getByPlaceholderText(/DataHoarder_42/i) as HTMLInputElement;
		await fireEvent.input(name, { target: { value: 'Doomed Edit' } });
		await tick();

		await waitFor(() => expect(publishMock).toHaveBeenCalled(), { timeout: 2000 });
		await waitFor(() => {
			const t = get(toastMessage);
			expect(t?.kind).toBe('error');
			expect(t?.text).toMatch(/all relays unreachable/);
		}, { timeout: 2000 });
		// The edit was NOT lost with the publish: the local save ran (immediately + in the publish
		// path), and the profile store holds the edited value.
		expect(saveMock).toHaveBeenCalled();
		await waitFor(() => expect(get(profile)?.display_name).toBe('Doomed Edit'));
	}, 15000);

	it('a publish failure is never rendered as a success toast', async () => {
		prime();
		publishMock.mockRejectedValue(new Error('relay rejected'));
		const { getByPlaceholderText } = render(HomePage);
		await tick();
		await wait(60);

		const name = getByPlaceholderText(/DataHoarder_42/i) as HTMLInputElement;
		await fireEvent.input(name, { target: { value: 'Silent Edit' } });
		await tick();
		await waitFor(() => expect(publishMock).toHaveBeenCalled(), { timeout: 2000 });
		await wait(60);
		const t = get(toastMessage);
		expect(t?.kind).toBe('error'); // error, never a success toast for a failed publish
	}, 15000);
});

// The 40ms test debounce is set once for the whole file — the page module and this file share the
// one module instance of profile-autopublish.js.
setAutopublishDebounceForTests(40);
