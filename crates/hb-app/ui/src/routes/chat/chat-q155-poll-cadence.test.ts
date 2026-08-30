// @vitest-environment jsdom
// The CADENCE half of QURATOR-155 — the thing that actually regressed.
//
// The QURATOR-155 fix made `fetchingNames` mean "in flight" (finally-delete) rather than "ever
// attempted", which is correct for de-duplication. But `peerNameCache[npub]` is only written on a
// SUCCESS with a non-empty display_name — so an npub that settles any OTHER way (profileless peer,
// profile without a display_name, relay unreachable) lands in neither the in-flight set nor the
// name cache. The poll-driven effect re-runs fetchNonContactNames on every DM poll (a writable.set
// with a fresh-but-equal array still notifies), so that shape means ONE pasteKey relay round-trip
// per profileless request sender EVERY 3s (DM_POLL_VISIBLE_MS) for as long as the Chat tab is
// visible — exactly the "background work becomes a per-tick fan-out" class this project rules
// against, previously reported three times as a presence query.
//
// This file pins the COUNTER (not just "it was called"): mount the page, push the SAME profileless
// npub through several dmRequests store emissions (modelling consecutive polls), and assert
// pasteKey was called EXACTLY ONCE for that npub. The calls are FILTERED BY NPUB — a bare
// toHaveBeenCalledTimes over all calls is vacuous here (refreshInbox/deep-link paths may also call
// pasteKey), which is precisely how the first attempt at a related test came back green-meaningless.
//
// Per CLAUDE.md §9 / P-10: the mutation probe is to delete the `settledNames.has(npub)` guard (and
// the finally's `settledNames.add`), which restores one-fetch-per-poll — every count test below
// must RED with a call count above 1.
//
// jsdom computes no layout; nothing here proves the row paints — only the fetch cadence.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import { get } from 'svelte/store';
import ChatPage from './+page.svelte';
import { identity, contacts, dmRequests, inboxMessages, sentMessages, readWatermarks, toastMessage } from '$lib/stores.js';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const SENDER = 'npub1sendersendersendersendersendersendsendse';

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	// No deep-link param is needed here; a static /chat URL is exactly the resting state.
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});

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
	pasteKeyMock.mockResolvedValue({ profile: null });
	identity.set(null);
	contacts.set([]);
	toastMessage.set(null);
	dmRequests.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
});

/** A request bucket for SENDER, freshly allocated each call: a NEW array identity every emission is
 *  what a real poll produces (the Rust fetch returns a fresh Vec → new JS array), and it is what
 *  makes writable.set notify subscribers even though the contents are equal. */
function requestBucket() {
	return [{
		npub: SENDER,
		first_seen: 1,
		last_message_at: 2,
		message_count: 1,
		messages: [{ from: SENDER, to: ME, content: 'hello stranger', sent_at: '2026-08-10T10:00:00Z' }],
	}];
}

/** Emit the same request list the way consecutive DM polls do — fresh array identity each time. */
async function poll(times: number) {
	for (let i = 0; i < times; i++) {
		dmRequests.set(requestBucket());
		await tick();
		await tick();
	}
}

function callsFor(npub: string) {
	return pasteKeyMock.mock.calls.filter((c) => c[0] === npub);
}

describe('QURATOR-155 cadence — a settled profileless sender is fetched once, not once per poll', () => {
	it('a profileless resolve (no display_name) is fetched exactly once across repeated poll emissions', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		pasteKeyMock.mockResolvedValue({ npub: SENDER, profile: null });
		render(ChatPage);
		await poll(5);
		await waitFor(() => expect(callsFor(SENDER).length).toBeGreaterThanOrEqual(1));
		expect(callsFor(SENDER)).toHaveLength(1);
	});

	it('a REJECTED resolve (relay unreachable) is fetched exactly once across repeated poll emissions', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		pasteKeyMock.mockRejectedValue(new Error('relay unreachable'));
		render(ChatPage);
		await poll(5);
		await waitFor(() => expect(callsFor(SENDER).length).toBeGreaterThanOrEqual(1));
		expect(callsFor(SENDER)).toHaveLength(1);
	});

	it('a resolve WITH a display_name still populates the cache (skipped afterwards)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		pasteKeyMock.mockResolvedValue({
			npub: SENDER,
			profile: { display_name: 'Named Stranger', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' },
		});
		const rendered = render(ChatPage);
		await poll(5);
		// The requests pane (openRequests) is the only place the fetched name renders; open it.
		await fireEvent.click(await rendered.findByText('Message requests'));
		await waitFor(() => expect(rendered.getByText('Named Stranger')).toBeTruthy());
		await waitFor(() => expect(callsFor(SENDER)).toHaveLength(1));
		expect(get(dmRequests)).toHaveLength(1);
	});

	it('a slow in-flight resolve is not re-issued while pending (concurrent duplicate suppression)', async () => {
		identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
		let land: () => void = () => {};
		pasteKeyMock.mockImplementation(() => new Promise<void>((res) => { land = res; }));
		render(ChatPage);
		await poll(3); // several polls while the single resolve is still pending
		expect(callsFor(SENDER)).toHaveLength(1);
		land();
		await tick();
		await waitFor(() => expect(callsFor(SENDER)).toHaveLength(1));
	});
});
