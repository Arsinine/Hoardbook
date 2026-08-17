// @vitest-environment jsdom
// QURATOR-93 (Contacts half) — a FAILED `get_contacts` used to render as the confident empty state
// "No contacts yet. Use '+ Add contact'…". The layout's `.catch(() => {})` left the store empty, and
// the page's `$contacts.length === 0` branch couldn't tell "no data" from "no rows".
//
// BEHAVIOURAL mount tests (not a source-scan): the page is mounted with the api mocked so `getContacts`
// REJECTS, and the assertions target the AFFORDANCES (role=alert, role=button named Retry) plus the
// ABSENCE of the confident empty string — per §9, asserting the word "retry" anywhere is prose.
//
// QURATOR-80 rule both ways: a later successful fetch clears the error (second test), and the
// genuine-empty string never renders while the error holds (first test).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContactsPage from './+page.svelte';
import { contacts, contactsLoadError } from '$lib/stores.js';
import type { CachedPeer, Profile } from '$lib/types.js';

// The api mock — every Tauri command Contacts imports is stubbed. getContacts is the spy under test;
// the others just need to resolve so the page's onMount fan-out (groups/private/audience/online poll)
// doesn't throw.
vi.mock('$lib/api.js', () => ({
	follow: vi.fn().mockResolvedValue(undefined),
	refreshContact: vi.fn().mockResolvedValue(undefined),
	unfollowContact: vi.fn().mockResolvedValue(undefined),
	setContactTags: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	groupsDelete: vi.fn().mockResolvedValue(undefined),
	groupsAssign: vi.fn().mockResolvedValue(undefined),
	groupsUnassign: vi.fn().mockResolvedValue(undefined),
	groupsCreateWithMembers: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn().mockResolvedValue(undefined),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: null, relay_set: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	// The load under test — individual tests flip resolve/reject.
	getContacts: vi.fn(),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { getContacts } from '$lib/api.js';
const getContactsMock = getContacts as unknown as ReturnType<typeof vi.fn>;

const PROF: Profile = {
	display_name: 'Load Error Peer',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

const ONE_CONTACT: CachedPeer = {
	npub: 'npub1q93contact' + 'a'.repeat(50),
	collections: [],
	online: false,
	last_fetched: '2026-08-01T00:00:00Z',
	local_tags: [],
	profile: PROF,
};

const EMPTY_STRING = /no contacts yet/i;
const ERROR_STRING = /couldn.t load contacts/i;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

describe('QURATOR-93 — Contacts load failure is not a confident empty', () => {
	it('load REJECTS → error alert + Retry render; the confident "No contacts yet" does NOT', async () => {
		getContactsMock.mockRejectedValue(new Error('peer cache unavailable'));
		// Simulate the layout's failed seed: the helper sets the flag on failure (same shape as the
		// q93-home test's collections seed).
		const { loadContactsInto } = await import('$lib/stores.js');
		void loadContactsInto(getContacts);

		const { getByRole, queryByText } = render(ContactsPage);
		await waitFor(() => expect(getByRole('alert')).toBeTruthy());
		// The Retry affordance is a BUTTON (not the word appearing in prose).
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
		// THE core assertion: the confident negative must NOT render on a failed load.
		expect(queryByText(EMPTY_STRING)).toBeNull();
		expect(queryByText(ERROR_STRING)).toBeTruthy();
	});

	it('Retry re-fetches: reject → resolve → the list renders and the error is gone', async () => {
		getContactsMock.mockRejectedValueOnce(new Error('first attempt fails'));
		const { loadContactsInto } = await import('$lib/stores.js');
		void loadContactsInto(getContacts);

		const { getByRole, queryByRole, findByText } = render(ContactsPage);
		await waitFor(() => expect(getByRole('button', { name: /retry/i })).toBeTruthy());

		// The retry path now succeeds (done through the same helper, so its success clears the flag —
		// the both-directions rule).
		getContactsMock.mockResolvedValue([ONE_CONTACT]);
		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		// The list actually lands (the user's real goal)…
		expect(await findByText('Load Error Peer')).toBeTruthy();
		// …the error alert is gone.
		await waitFor(() => expect(queryByRole('alert')).toBeNull());
	});

	it('a SUCCESSFUL load renders the genuine empty state, not the error', async () => {
		getContactsMock.mockResolvedValue([]);
		const { loadContactsInto } = await import('$lib/stores.js');
		await loadContactsInto(getContacts);

		const { queryByRole, findByText } = render(ContactsPage);
		expect(await findByText(EMPTY_STRING)).toBeTruthy();
		expect(queryByRole('alert')).toBeNull();
	});
});
