// @vitest-environment jsdom
// QURATOR-100 — profile-picture apply/remove had NO error path on Home: `handlePictureFile` and
// `handleRemovePicture` were `try { … } finally { pictureBusy = false; }` with no catch, so a
// rejected apply/remove surfaced as an unhandled rejection with zero user feedback.
//
// applyProfilePicture/removeProfilePicture (lib/profile-picture.ts) catch their own errors and
// return false/undefined, but the PAGE's handlers can still reject — `applyProfilePicture` calls
// `compressToDataUri`, which can throw synchronously in jsdom (no createImageBitmap) and, more
// importantly, the handler itself runs outside the helper's try in real shapes (e.g. a throwing
// read of `$profile`, or any future reordering). This test drives the REAL page handlers via the
// real file input / remove button and asserts the error toast renders and no unhandled rejection
// escapes — pinning the catch that the fix adds.
//
// The toast store is shared module state (single global toast slot), so the toast is asserted via
// the rendered DOM of the page under test + the store value.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import HomePage from './+page.svelte';
import { identity, profile, collections, appReady, homeDraft, identityLoadError, toastMessage } from '$lib/stores.js';
import { get } from 'svelte/store';
import type { IdentityInfo, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/api.js')>();
	return {
		...actual,
		hasPublishedProfile: vi.fn().mockResolvedValue(false),
		collectionSourceAccessible: vi.fn().mockResolvedValue(true),
	};
});

// The picture pipeline is mocked so we can force the REJECTION from inside applyProfilePicture —
// the page handler must toast whatever the pipeline throws.
vi.mock('$lib/profile-picture.js', () => ({
	applyProfilePicture: vi.fn(),
	removeProfilePicture: vi.fn(),
}));

import { applyProfilePicture, removeProfilePicture } from '$lib/profile-picture.js';
const applyMock = applyProfilePicture as unknown as ReturnType<typeof vi.fn>;
const removeMock = removeProfilePicture as unknown as ReturnType<typeof vi.fn>;

const IDENT: IdentityInfo = {
	npub: 'npub1q100' + 'a'.repeat(53),
	npub_short: 'npub1q100…aaaa',
	share_code: 'hbk1q100test',
	key_storage: 'os-encrypted',
};

const PROF: Profile = {
	display_name: 'Picture Tester',
	bio: undefined,
	tags: [],
	since: 2021,
	est_size: undefined,
	languages: ['English'],
	contact_hint: undefined,
	email: undefined,
	location: undefined,
	social_links: [],
	willing_to: [],
	content_types: [],
	picture: 'data:image/webp;base64,AAAA',
	updated: '2026-08-01T00:00:00Z',
};

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

/** Render Home onto the profile view (obStep 4) with a profile that HAS a picture. */
async function mountHome() {
	homeDraft.set({ ...PROF });
	profile.set({ ...PROF });
	identity.set(IDENT);
	appReady.set(true);
	const r = render(HomePage);
	await tick();
	await waitFor(() => expect(r.getByText('My Profile')).toBeTruthy());
	return r;
}

describe('QURATOR-100 — picture apply/remove failures reach the user', () => {
	it('applyProfilePicture REJECTS → an error toast renders; no unhandled rejection', async () => {
		applyMock.mockRejectedValue(new Error('picture pipeline exploded'));
		const { container } = await mountHome();

		const onUnhandled = vi.fn();
		process.on('unhandledRejection', onUnhandled);

		// Drive the page's REAL handler through the real hidden file input.
		const input = container.querySelector('input[type="file"]') as HTMLInputElement;
		expect(input).toBeTruthy();
		// jsdom's input.files is read-only; Object.defineProperty is the standard fireEvent route.
		const file = new File(['x'], 'me.png', { type: 'image/png' });
		Object.defineProperty(input, 'files', { value: [file], configurable: true });
		await fireEvent.change(input);

		// THE ASSERTION: the user sees the error, not a silent unhandled rejection. The toast DOM is
		// rendered by +layout.svelte (which a page-only mount doesn't include), so assert on the
		// toastMessage store — the single source of truth the layout renders from.
		await waitFor(() => {
			expect(get(toastMessage)?.kind).toBe('error');
			expect(get(toastMessage)?.text).toContain('picture pipeline exploded');
		});
		// pictureBusy must still have been released by the finally.
		const picker = container.querySelector('.avatar-picker') as HTMLButtonElement;
		expect(picker.disabled).toBe(false);

		await new Promise((r) => setTimeout(r, 20));
		expect(onUnhandled).not.toHaveBeenCalled();
		process.off('unhandledRejection', onUnhandled);
	});

	it('removeProfilePicture REJECTS → an error toast renders; no unhandled rejection', async () => {
		removeMock.mockRejectedValue(new Error('remove failed hard'));
		const { container } = await mountHome();

		const onUnhandled = vi.fn();
		process.on('unhandledRejection', onUnhandled);

		const rm = await waitFor(() => {
			const el = [...container.querySelectorAll('button')].find((b) =>
				(b.textContent ?? '').trim().toLowerCase() === 'remove picture');
			expect(el).toBeTruthy();
			return el as HTMLButtonElement;
		});
		await fireEvent.click(rm);

		await waitFor(() => {
			expect(get(toastMessage)?.kind).toBe('error');
			expect(get(toastMessage)?.text).toContain('remove failed hard');
		});
		const picker = container.querySelector('.avatar-picker') as HTMLButtonElement;
		expect(picker.disabled).toBe(false);

		await new Promise((r) => setTimeout(r, 20));
		expect(onUnhandled).not.toHaveBeenCalled();
		process.off('unhandledRejection', onUnhandled);
	});

	// Success still works and does NOT error-toast (guards against a catch that swallows everything).
	it('applyProfilePicture RESOLVES true → no error toast', async () => {
		applyMock.mockResolvedValue(true);
		const { container } = await mountHome();

		const input = container.querySelector('input[type="file"]') as HTMLInputElement;
		const file = new File(['x'], 'me.png', { type: 'image/png' });
		Object.defineProperty(input, 'files', { value: [file], configurable: true });
		await fireEvent.change(input);

		await waitFor(() => expect(applyMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));
		expect(get(toastMessage)?.kind ?? null).not.toBe('error');
	});
});
