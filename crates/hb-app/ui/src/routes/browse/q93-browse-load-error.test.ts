// @vitest-environment jsdom
// QURATOR-93 (Browse half) — minor-2 from the 2026-08-17 design audit: two failed loads that Browse
// rendered as confident empties instead of an error + Retry.
//
//   (a) People rail: a FAILED `getContacts` used to render "No contacts yet" — indistinguishable
//       from a genuine empty peer cache. Same idiom as contacts/q93-contacts-load-error.test.ts
//       (its twin): mount with `contactsLoadError` already set (through the shared
//       `loadContactsInto` helper), assert the error + Retry render and the confident negative
//       does NOT.
//
//   (b) Private section: a FAILED `browsePrivateCollections` load was swallowed by a bare
//       `.catch(() => {})`, so the section simply never appeared for a selected peer — the fetch
//       is page-local (not keyed to any one peer), not the per-peer `browsePrivateCollections`
//       success path q92-private-collections.test.ts already pins. Mount with a peer selected
//       (the same `?peer=` deep-link idiom q92 uses) and the fetch REJECTING; assert a retryable
//       error line renders in the Private section area instead of it staying silently absent.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code; the mutation
// probe (revert each production half alone, one at a time) is documented in the task report.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts, contactsLoadError } from '$lib/stores.js';
import type { CachedPeer } from '$lib/types.js';

// The api mock — every Tauri command Browse imports is stubbed. getContacts and
// browsePrivateCollections are the spies under test; the others just need to resolve so the
// page's mount effects don't throw.
vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn(),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// $page stub: the same `/browse?peer=<npub>` deep-link idiom as q92-private-collections.test.ts.
// The (a) People-rail tests never populate a matching contact, so the deep-link simply never
// resolves (peerFromQuery returns null) — no peer gets auto-selected there. The (b) tests below
// set `contacts` to a peer matching this npub, which DOES resolve it, exactly like q92.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1pr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0be') }) };
});
vi.mock('$app/stores', () => stubPage);

import { getContacts, browsePrivateCollections } from '$lib/api.js';
const getContactsMock = getContacts as unknown as ReturnType<typeof vi.fn>;
const privateMock = browsePrivateCollections as unknown as ReturnType<typeof vi.fn>;

const PEER_NPUB = 'npub1pr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0bepr0be';

const PROF = { display_name: 'Load Error Peer', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' };

const ONE_CONTACT: CachedPeer = {
	npub: 'npub1q93browsea' + 'a'.repeat(48),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF,
};

// A peer matching the stubbed `?peer=` URL, so the (b) tests auto-select it on mount (same
// fixture shape as q92's PEER: `browse_key_hex` set so selectPeer's live-refetch path runs
// through the mocked, non-throwing `refreshContact`).
const SELECTED_PEER: CachedPeer = {
	npub: PEER_NPUB,
	browse_key_hex: 'aabbccdd',
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: { display_name: 'Sealed Peer', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-08-01T00:00:00Z' },
};

const EMPTY_STRING = /no contacts yet/i;
const CONTACTS_ERROR_STRING = /couldn.t load contacts/i;
const PRIVATE_ERROR_STRING = /couldn.t load private collections/i;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

describe('QURATOR-93 (Browse half) — People rail load failure is not a confident empty', () => {
	it('a failed contacts load renders error + Retry; "No contacts yet" does NOT', async () => {
		privateMock.mockResolvedValue([]);
		getContactsMock.mockRejectedValue(new Error('peer cache unavailable'));
		const { loadContactsInto } = await import('$lib/stores.js');
		void loadContactsInto(getContacts);

		const { getByRole, queryByText } = render(BrowsePage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		expect(queryByText(EMPTY_STRING)).toBeNull();
		expect(queryByText(CONTACTS_ERROR_STRING)).toBeTruthy();
	});

	it('Retry re-fetches through the shared helper: reject -> resolve -> the row renders, error is gone', async () => {
		privateMock.mockResolvedValue([]);
		getContactsMock.mockRejectedValueOnce(new Error('first attempt fails'));
		const { loadContactsInto } = await import('$lib/stores.js');
		void loadContactsInto(getContacts);

		const { getByRole, queryByRole, findByText } = render(BrowsePage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		getContactsMock.mockResolvedValue([ONE_CONTACT]);
		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		expect(await findByText('Load Error Peer')).toBeTruthy();
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});

	it('a successful load renders the genuine empty state, not the error', async () => {
		privateMock.mockResolvedValue([]);
		getContactsMock.mockResolvedValue([]);
		const { loadContactsInto } = await import('$lib/stores.js');
		await loadContactsInto(getContacts);

		const { queryByRole, findByText } = render(BrowsePage);
		expect(await findByText(EMPTY_STRING)).toBeTruthy();
		expect(queryByRole('alert')).toBeNull();
	});
});

describe('QURATOR-93 (Browse half) — Private section load failure is not silently absent', () => {
	it('a failed browsePrivateCollections load renders a retryable error line for the selected peer', async () => {
		getContactsMock.mockResolvedValue([]);
		privateMock.mockRejectedValue(new Error('relay hiccup'));
		contacts.set([SELECTED_PEER]);

		const { getByRole, findByText } = render(BrowsePage);
		await tick();

		expect(await findByText(PRIVATE_ERROR_STRING)).toBeTruthy();
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		// Guardrail: the public empty state is untouched by the private-load failure.
		expect(document.body.textContent).toContain('No public collections');
	});

	it('Retry re-fetches: reject -> resolve -> the private section renders, the error line is gone', async () => {
		getContactsMock.mockResolvedValue([]);
		privateMock.mockRejectedValueOnce(new Error('first attempt fails'));
		contacts.set([SELECTED_PEER]);

		const { getByRole, findByText, queryByText } = render(BrowsePage);
		await tick();
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		privateMock.mockResolvedValue([]);
		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		await waitFor(() => expect(queryByText(PRIVATE_ERROR_STRING)).toBeNull());
		// A resolved EMPTY list is the genuine-empty case — the section itself (badge/label)
		// still shouldn't render once the error clears.
		expect(document.body.textContent).not.toContain('Private collections');
	});

	it('a peer with no private entry (genuine empty, no error) renders nothing new — unchanged', async () => {
		getContactsMock.mockResolvedValue([]);
		privateMock.mockResolvedValue([]);
		contacts.set([SELECTED_PEER]);

		render(BrowsePage);
		await tick();
		await waitFor(() => {
			expect(document.body.textContent).toContain('No public collections');
		});
		expect(document.body.textContent).not.toContain('Private collections');
		expect(document.querySelectorAll('[role="alert"]').length).toBe(0);
	});
});
