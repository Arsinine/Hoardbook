// @vitest-environment jsdom
// QURATOR-146 (Topics W4) — the blocker half of the roster hand-off. The whole population of a
// Topic roster is people you have NOT added, and the old deep-link effect guarded on
// `const peer = $contacts.find(...); if (peer) …` — so `?peer=<stranger-npub>` silently did
// nothing and the interest-first flow dead-ended at a read-only list. This file pins the fix as
// BEHAVIOUR on a real mount (CLAUDE.md §7: the route pages mount in vitest; only `$lib/api.js`
// and `$app/stores`' benign `page` stub are mocked), following chat-q97-compose-backdrop.test.ts's
// mock surface.
//
// The non-contact path resolves the stranger via pasteKey (the same helper fetchNonContactNames
// uses) and calls selectPeer with a synthetic CachedPeer — so the pane header renders the resolved
// display name, and selectedIsContact reads false on its own, driving the existing "may filter
// your messages" banner with zero further changes.
//
// Per CLAUDE.md §9 / P-10, a green test proves nothing until seen red. The probe for the headline
// test: revert the effect to the bare `if (peer)` contact-only guard (delete the non-contact
// pasteKey branch) — the pane never renders the stranger and the test REDS.
//
// jsdom computes no layout — nothing here proves the pane paints visually; only that the peer's
// header name and the privacy notice are in the DOM.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, toastMessage } from '$lib/stores.js';
import { get } from 'svelte/store';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const STRANGER = 'npub1strangerstrangerstrangerstrangerstrangerstrang';

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	// The page reads `$page.url.searchParams` in onMount and an $effect. Resolve the URL at
	// SUBSCRIBE time (i.e. when the component mounts) rather than fixing it at import — that way
	// each test's `history.replaceState` before render is what the component sees, and no test
	// inherits a previous one's param (the store re-reads window.location on every subscribe).
	type PageLike = { url: URL };
	return {
		page: readable<PageLike>(undefined as unknown as PageLike, (set) => {
			set({ url: new URL(window.location.href) });
			return () => {};
		}),
	};
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

import { pasteKey } from '$lib/api.js';
const pasteKeyMock = pasteKey as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	toastMessage.set(null);
	window.history.replaceState({}, '', '/chat');
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

describe('QURATOR-146 — `?peer=` opens a NON-contact (Topic roster hand-off)', () => {
	it('deep-links a stranger into their conversation pane with the resolved display name', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]); // the npub is NOT a contact — the exact roster situation
		pasteKeyMock.mockResolvedValue(strangerResolve());
		// window.history.replaceState sets the URL the page-url stub will read at mount.
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER);

		const { findByText } = render(ChatPage);

		// The pane header shows the RESOLVED name (not the short npub fallback, not nothing).
		expect(await findByText('Roster Stranger')).toBeTruthy();
		expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER);
	});

	it('shows the "may filter your messages" notice — selectedIsContact reads false on its own', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);
		pasteKeyMock.mockResolvedValue(strangerResolve());
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER);

		const { findByText } = render(ChatPage);

		// The pre-existing non-contact banner (the M17 "DMs may be restricted" notice) must appear
		// with no further changes — this is the discriminator proving the synthetic peer flows
		// through the SAME selectPeer path a contact does.
		expect(await findByText(/may not have added you back/i)).toBeTruthy();
	});

	it('a contact deep-link still resolves WITHOUT pasteKey (contact-first, no regression)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		const contact = {
			npub: STRANGER, petname: 'Saved Pal', local_tags: [],
			profile: { display_name: 'Saved Pal', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
			collections: [], online: true, last_fetched: '',
		};
		contacts.set([contact]);
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER);

		const { findByText } = render(ChatPage);

		expect(await findByText('Saved Pal')).toBeTruthy();
		// The contact path must not network: a stale resolve must never shadow a saved contact.
		await tick();
		expect(pasteKeyMock).not.toHaveBeenCalled();
	});

	it('a pasteKey failure does not open a pane and surfaces the error (pasteKey rejects profileless)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		contacts.set([]);
		pasteKeyMock.mockRejectedValue(new Error("This person hasn't published a profile yet"));
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER);

		const { queryByText } = render(ChatPage);
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalled());
		await tick();

		// No conversation pane opened for the failed resolve, and the rejection reached the toast
		// (the toast itself renders in +layout, so assert on the store it writes).
		expect(queryByText(/may not have added you back/i)).toBeNull();
		await waitFor(() => expect(get(toastMessage)?.text).toMatch(/hasn't published a profile/i));
	});
});
