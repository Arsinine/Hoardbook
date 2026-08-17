// @vitest-environment jsdom
// minor-3 — a direct `contacts.set(await getContacts())` / `collections.set(...)` refresh bypassed
// `loadContactsInto`/`loadCollectionsInto`, so a prior load-error flag stayed SET even after the
// refresh succeeded (Contacts/Home kept showing an error over valid data). Fix: every such refresh
// in this file now routes through the helpers, which set the store AND clear the flag on success.
// These tests pin two of the six sites: the onMount collections seed, and handleComposeSend.
//
// minor-4 — chat's remaining confident empties. A failed `getMessages` (DM history) or failed
// `dmRequests` fetch still rendered "No conversations yet." / "No message requests." — the same
// QURATOR-93 shape already fixed for Channels, now extended to the conversations sidebar and the
// requests pane, with a Retry affordance and success clearing the flag.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { get } from 'svelte/store';
import ChatPage from './+page.svelte';
import {
	identity, contacts, inboxMessages, sentMessages, readWatermarks, dmRequests,
	collectionsLoadError, contactsLoadError,
} from '$lib/stores.js';

const { ME, PEER } = vi.hoisted(() => ({
	ME: 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
	PEER: 'npub1peerrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr',
}));

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('$lib/api.js', () => ({
	getMessages: vi.fn().mockResolvedValue([]),
	sendMessage: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicList: vi.fn().mockResolvedValue([]),
	topicChannel: vi.fn().mockResolvedValue({ posts: [], announcements: [] }),
	topicPost: vi.fn(),
	getContacts: vi.fn().mockResolvedValue([]),
	dmRequests: vi.fn().mockResolvedValue([]),
	dmRequestAccept: vi.fn(),
	dmRequestDecline: vi.fn(),
	dmBlock: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn(),
	advanceReadWatermark: vi.fn().mockResolvedValue(undefined),
	topicAnnounceMarkSeen: vi.fn(),
	getShareCode: vi.fn().mockResolvedValue(''),
	relayStatus: vi.fn().mockResolvedValue([]),
	getCollections: vi.fn().mockResolvedValue([]),
	exportManifest: vi.fn(),
	sendFullList: vi.fn(),
	redeemManifestTicket: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
	getManifestAsks: vi.fn().mockResolvedValue([]),
}));

import { getMessages, dmRequests as fetchDmRequests, getContacts, getCollections, sendMessage } from '$lib/api.js';
const getMessagesMock = getMessages as unknown as ReturnType<typeof vi.fn>;
const fetchDmRequestsMock = fetchDmRequests as unknown as ReturnType<typeof vi.fn>;
const getContactsMock = getContacts as unknown as ReturnType<typeof vi.fn>;
const getCollectionsMock = getCollections as unknown as ReturnType<typeof vi.fn>;
const sendMessageMock = sendMessage as unknown as ReturnType<typeof vi.fn>;

const REQUEST = {
	npub: PEER,
	first_seen: 1_755_000_000,
	last_message_at: 1_755_000_000,
	message_count: 1,
	messages: [{ from: PEER, to: ME, content: 'hi, first message', sent_at: '2026-08-10T10:00:00Z' }],
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
	dmRequests.set([]);
	collectionsLoadError.set(false);
	contactsLoadError.set(false);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
}

describe('minor-3 — direct-refresh sites route through the load-error-clearing helpers', () => {
	it('onMount collections seed clears a stale collectionsLoadError on a successful getCollections', async () => {
		collectionsLoadError.set(true); // simulates a PRIOR failed load elsewhere (e.g. Home)
		getCollectionsMock.mockResolvedValue([]);
		primeIdentity();

		render(ChatPage);

		// The mount-time loadCollectionsInto(getCollections) call must clear the stale flag once it
		// resolves — a direct `collections.set(...)` here would have left it standing.
		await waitFor(() => expect(get(collectionsLoadError)).toBe(false));
	});

	it('handleComposeSend clears a stale contactsLoadError after a successful send + getContacts refresh', async () => {
		contactsLoadError.set(true); // simulates a PRIOR failed load elsewhere (e.g. Contacts)
		getContactsMock.mockResolvedValue([]);
		sendMessageMock.mockResolvedValue({ from: ME, to: PEER, content: 'hey', sent_at: '2026-08-17T00:00:00Z' });
		primeIdentity();
		contacts.set([]);

		const { getByRole, getByPlaceholderText } = render(ChatPage);

		await fireEvent.click(getByRole('button', { name: /new message/i }));
		await tick();
		await fireEvent.input(getByPlaceholderText(/npub or hbk share code…/i), { target: { value: PEER } });
		await fireEvent.input(getByPlaceholderText(/message…/i), { target: { value: 'hey' } });
		await fireEvent.click(getByRole('button', { name: /^send$/i }));

		// handleComposeSend's `await loadContactsInto(getContacts)` must clear the stale flag — a
		// direct `contacts.set(await getContacts())` here would have left it standing.
		await waitFor(() => expect(get(contactsLoadError)).toBe(false));
	});
});

describe('minor-4 — a failed DM-history load is not a confident "No conversations yet."', () => {
	it('getMessages REJECTS → error + Retry renders, the confident empty is absent; Retry → success clears it', async () => {
		getMessagesMock.mockRejectedValueOnce(new Error('relay unreachable')).mockResolvedValue([]);
		primeIdentity();
		contacts.set([]);

		const { getByText, queryByText, getByRole, findByRole } = render(ChatPage);

		await waitFor(() => expect(getByText(/couldn.t load your conversations/i)).toBeTruthy());
		expect(queryByText(/no conversations yet/i)).toBeNull();
		expect(getByRole('alert')).toBeTruthy();

		await fireEvent.click(getByRole('button', { name: /retry/i }));
		await tick();

		await waitFor(() => expect(getByText(/no conversations yet/i)).toBeTruthy());
		expect(queryByText(/couldn.t load your conversations/i)).toBeNull();
	});
});

describe('minor-4 — a failed message-requests load is not a confident "No message requests."', () => {
	it('sidebar: dmRequests REJECTS on mount → "Couldn\'t load requests." + Retry, no silent drop of the section', async () => {
		fetchDmRequestsMock.mockRejectedValue(new Error('relay unreachable'));
		primeIdentity();
		contacts.set([]);

		const { getByText, getByRole } = render(ChatPage);

		await waitFor(() => expect(getByText(/couldn.t load requests/i)).toBeTruthy());
		expect(getByRole('button', { name: /retry/i })).toBeTruthy();
	});

	it('requests pane: a load that fails AFTER the pane is already open shows error, not "No message requests."', async () => {
		fetchDmRequestsMock.mockResolvedValueOnce([REQUEST]).mockRejectedValue(new Error('relay unreachable'));
		getMessagesMock.mockResolvedValue([]);
		primeIdentity();
		contacts.set([]);

		const { getByText, getByRole, queryByText, findByRole } = render(ChatPage);

		// Requests loaded fine initially — open the pane and see the one request.
		const requestsButton = await findByRole('button', { name: /message requests/i });
		await fireEvent.click(requestsButton);
		await tick();
		await waitFor(() => expect(queryByText(/no message requests/i)).toBeNull());

		// A subsequent refresh (the same `refreshInbox` → `loadRequests()` chain the poll uses) now
		// fails while the pane is still open.
		await fireEvent.click(getByRole('button', { name: /refresh inbox/i }));

		await waitFor(() => expect(getByText(/couldn.t load message requests/i)).toBeTruthy());
		expect(queryByText(/no message requests/i)).toBeNull();
	});
});
