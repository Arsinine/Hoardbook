// @vitest-environment jsdom
// The race half of QURATOR-146's non-contact deep-link. pasteKey is a NETWORK round trip (teaser +
// presence + listings, each with its own timeout on the Rust side) that can legitimately take tens
// of seconds against slow/unreachable relays. The original .then() called selectPeer(resolved)
// unconditionally — so a resolve that outlived the user's patience would land long after they'd
// clicked elsewhere and YANK the view back to the stranger's conversation (selectPeer clears
// selectedTopic/viewingRequests/selectedRequest wholesale).
//
// This file pins the landing-time re-validation as BEHAVIOUR on a real mount (CLAUDE.md §7: route
// pages mount in vitest; only `$lib/api.js` and `$app/stores`' benign `page` stub are mocked), with
// a manually-controlled deferred pasteKey so the test drives the clock, not real timers. The mock
// surface follows chat-q146-noncontact-deeplink.test.ts.
//
// The page stub must be WRITABLE (unlike the sibling file's `readable`): prong 1 of the fix
// re-reads `$page.url.searchParams` at resolve time, and only a store that re-emits can model the
// user navigating to a different URL mid-flight the way SvelteKit's real page store does.
//
// Per CLAUDE.md §9 / P-10, a green test proves nothing until seen red. The probe: delete the
// `stillWanted` guard (restore the unconditional selectPeer) — the four yank tests RED (the
// no-over-blocking test is deliberately invariant: it pins that a WANTED slow resolve still opens).
//
// jsdom computes no layout — nothing here proves the panes paint visually; only that the right
// pane's content shows the view the user actually chose, not the stale stranger's.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, toastMessage, dmRequests, inboxMessages, sentMessages, readWatermarks } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const STRANGER = 'npub1strangerstrangerstrangerstrangerstrangerstrang';
const OTHER = 'npub1otherotherotherotherotherotherotherotherotheroth';

// SvelteKit's real page store re-emits on navigation. A `readable` stub (as the sibling Q146 file
// uses) snapshots the URL once at mount, which is fine for a param the page reads once — but prong
// 1 of the fix re-reads the param at RESOLVE time, so the stub must be WRITABLE and re-emit the way
// a real goto() would. The mock factory cannot reference outer bindings (vi.mock hoists it above
// them), so the setter rides on the mocked module itself and tests pull it off the module import.
vi.mock('$app/stores', async () => {
	const { writable } = await import('svelte/store');
	type PageLike = { url: URL };
	const page = writable<PageLike>({ url: new URL('http://localhost/chat') });
	const setPageUrl = (path: string) => page.set({ url: new URL('http://localhost' + path) });
	return { page, setPageUrl };
});

// The setter is NOT part of $app/stores' real types — the mock adds it at runtime. svelte-check
// reads the ambient types, so the import must go through a local module declaration that widens
// the shape just for this file (vi.mock replaces the module wholesale at runtime regardless).
declare module '$app/stores' {
	export function setPageUrl(path: string): void;
}
import { setPageUrl } from '$app/stores';

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

import { pasteKey, getMessages, topicList, dmRequests as dmRequestsApi } from '$lib/api.js';
const pasteKeyMock = pasteKey as unknown as ReturnType<typeof vi.fn>;
const getMessagesMock = getMessages as unknown as ReturnType<typeof vi.fn>;
const topicListMock = topicList as unknown as ReturnType<typeof vi.fn>;
// (dmRequestsApi used via inline cast — one call site.)

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	// clearAllMocks clears call history but NOT implementations — re-pin the defaults this file
	// overrides per-test so nothing leaks into the next one.
	getMessagesMock.mockResolvedValue([]);
	pasteKeyMock.mockResolvedValue({ npub: STRANGER, profile: null });
	identity.set(null);
	contacts.set([]);
	toastMessage.set(null);
	dmRequests.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
	setPageUrl('/chat');
});

/** The synthetic CachedPeer pasteKey returns for a stranger: a bare-npub resolve answers identity,
 *  presence and profile; nothing about it is a saved contact. */
function strangerResolve() {
	return {
		npub: STRANGER,
		local_tags: [],
		profile: {
			display_name: 'Roster Stranger',
			tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '',
		},
		collections: [],
		online: false,
		last_fetched: '',
	};
}

/** A deferred pasteKey for STRANGER: resolves only when the test calls land(). Any OTHER npub
 *  (fetchNonContactNames also networks via pasteKey, and a re-fired deep-link resolves a new
 *  param) resolves inertly — shaped like the real API, which echoes the requested npub, so the
 *  pane never renders a peerless object. Lets the test navigate away BEFORE the resolve lands —
 *  the exact interleaving the race lives in. */
function deferredPasteKey() {
	let land: (v: unknown) => void = () => {};
	const promise = new Promise((res) => { land = res; });
	pasteKeyMock.mockImplementation((npub: string) =>
		npub === STRANGER ? promise : Promise.resolve({ npub, profile: null }));
	return { land, promise };
}

describe('QURATOR-146 — a slow non-contact resolve must not override in-flight navigation', () => {
	it('a resolve landing AFTER a sidebar conversation click does not yank the view back', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		const contact = {
			npub: OTHER, petname: 'Saved Pal', local_tags: [],
			profile: { display_name: 'Saved Pal', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: true, last_fetched: '',
		};
		contacts.set([contact]);
		// The sidebar only lists contacts with DM history (M15 W6) — seed one from OTHER so the row
		// exists to click.
		getMessagesMock.mockResolvedValue([
			{ from: OTHER, to: ME, content: 'hey from other', sent_at: '2026-08-10T10:00:00Z' },
		]);
		const deferred = deferredPasteKey();
		setPageUrl('/chat?peer=' + STRANGER);
		const rendered = render(ChatPage);
		// Wait until the deep-link effect has actually fired the resolve BEFORE clicking away —
		// otherwise the click could beat the effect and prove nothing.
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER));
		// The text appears twice once selected (sidebar preview + thread bubble) — click the row's.
		await fireEvent.click((await rendered.findAllByText('hey from other'))[0]);
		await tick();

		deferred.land(strangerResolve());
		await tick();
		await tick();

		// The user is still on Saved Pal's thread (the bubble renders only for the SELECTED peer);
		// the stale stranger never took the pane.
		expect(rendered.getByText('Saved Pal', { selector: '.pane-peer-name' })).toBeTruthy();
		expect(rendered.queryByText('Roster Stranger')).toBeNull();
		expect(rendered.queryByText(/may not have added you back/i)).toBeNull();
	});

	it('a resolve landing AFTER the user opens the Requests pane does not yank the view back', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);
		// One pending request so the "Message requests" sidebar row exists to click. Set via the
		// API mock, not the store: the page's own loadRequests() on mount would overwrite a store
		// pre-set with the (empty) fetch result.
		(dmRequestsApi as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce([{
			npub: OTHER, first_seen: 1, last_message_at: 2, message_count: 1,
			messages: [{ from: OTHER, to: ME, content: 'hi', sent_at: '2026-01-01T00:00:00Z' }],
		}]);
		const deferred = deferredPasteKey();
		setPageUrl('/chat?peer=' + STRANGER);
		const rendered = render(ChatPage);

		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER));
		const rows = await rendered.findAllByText('Message requests');
		// [0] = the sidebar row (the pane header, if present, sorts after it).
		await fireEvent.click(rows[0]);
		await tick();

		deferred.land(strangerResolve());
		await tick();
		await tick();

		// Still on Requests (its explainer renders only in that pane), not the stranger's thread.
		expect(await rendered.findByText('Quarantined until you accept, decline, or block')).toBeTruthy();
		expect(rendered.queryByText('Roster Stranger')).toBeNull();
	});

	it('a resolve landing AFTER the user opens a Topic channel does not yank the view back', async () => {
		topicListMock.mockResolvedValue([{
			topic_id: 't-top', name: 'vinyl', description: '', tags: [], private: false, joined_at: 1,
		}]);
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);
		const deferred = deferredPasteKey();
		setPageUrl('/chat?peer=' + STRANGER);
		const rendered = render(ChatPage);

		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER));
		// The sidebar's Channels section renders once loadTopics settles.
		await fireEvent.click(await rendered.findByText('vinyl'));
		await tick();

		deferred.land(strangerResolve());
		await tick();
		await tick();

		// Still on the channel (its empty-thread copy renders only in that pane), not the stranger's.
		expect(await rendered.findByText(/No posts in the last 24h/i)).toBeTruthy();
		expect(rendered.queryByText('Roster Stranger')).toBeNull();
	});

	it('when nothing else was selected, the slow resolve still opens the pane (no over-blocking)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);
		const deferred = deferredPasteKey();
		setPageUrl('/chat?peer=' + STRANGER);
		const rendered = render(ChatPage);

		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER));
		deferred.land(strangerResolve());
		await tick();
		await tick();

		expect(await rendered.findByText('Roster Stranger')).toBeTruthy();
		expect(await rendered.findByText(/may not have added you back/i)).toBeTruthy();
	});

	it('a resolve landing after the URL param changed (deep-link to a DIFFERENT stranger) does not yank', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);
		const deferred = deferredPasteKey();
		// The SECOND deep-link's resolve must stay pending — if it landed it would select OTHER in
		// both fixed and unfixed code and mask which guard did the work.
		pasteKeyMock.mockImplementation((npub: string) =>
			npub === STRANGER ? deferred.promise : new Promise(() => {}));
		setPageUrl('/chat?peer=' + STRANGER);
		const rendered = render(ChatPage);

		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER));
		// The user deep-links elsewhere (e.g. re-clicks a different roster row in Topics) — the URL
		// param changes while the first resolve is still in flight, the way a real goto() would.
		// (A full route change unmounts the page and isn't mountable here, so the param change is
		// the observable half of prong 1.)
		setPageUrl('/chat?peer=' + OTHER);
		await tick();
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(OTHER));

		deferred.land(strangerResolve());
		await tick();
		await tick();

		// The STALE resolve must not open anything — the pane stays on "select a contact" because
		// the only live deep-link (OTHER) is still resolving.
		expect(rendered.queryByText('Roster Stranger')).toBeNull();
		expect(rendered.getByText('Select a contact to view the conversation.')).toBeTruthy();
	});
});
