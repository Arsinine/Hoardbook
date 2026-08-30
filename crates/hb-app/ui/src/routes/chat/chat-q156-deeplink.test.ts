// @vitest-environment jsdom
// QURATOR-156 — two defects in the W4 non-contact `?peer=` deep-link branch, pinned as BEHAVIOUR on
// a real mount (CLAUDE.md §7: the route pages mount in vitest; only `$lib/api.js` and
// `$app/stores`' benign `page` stub are mocked), following chat-q146-noncontact-deeplink.test.ts's
// mock surface.
//
//  1. (Part 2, mandatory) `&intent=ask-access` on a NON-contact deep-link must prefill the composer
//     via the SAME applyAskAccessIntent helper the contact path uses — not open an empty composer.
//  2. (Part 1) a rejected non-contact resolve must offer an in-place Retry (the shared EmptyState
//     error surface), and pressing it must re-enter pasteKey and open the pane on success.
//  3. (QURATOR-155) `fetchingNames` must mean "in flight", not "ever attempted": a settled
//     Request-bucket fetch must not suppress a later DEEP-LINK resolve for the same npub; two
//     CONCURRENT triggers must still de-duplicate to ONE fetch.
//
// Per CLAUDE.md §9 / P-10, a green test proves nothing until seen red — each probe is named in the
// header comment of the test it belongs to, and each was run and confirmed red before reverting.
//
// jsdom computes no layout — nothing here proves the pane paints visually, only that the composer
// textarea, the Retry button and the pane header are in the DOM.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPage from './+page.svelte';
import { identity, contacts, toastMessage } from '$lib/stores.js';
import { get } from 'svelte/store';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
const STRANGER = 'npub1strangerstrangerstrangerstrangerstrangerstrang';

const stubPage = vi.hoisted(async () => {
	const { writable } = await import('svelte/store');
	// SvelteKit's real page store re-emits on navigation. The readable snapshot-once stub this file
	// used to carry is fine for a param read at mount, but the QURATOR-155 test below needs a
	// deep-link to arrive AFTER mount (a settled bucket fetch must precede it), so the stub must be
	// WRITABLE and re-emit — the same shape chat-q146-stale-resolve-race.test.ts uses.
	type PageLike = { url: URL };
	const page = writable<PageLike>(undefined as unknown as PageLike, (set) => {
		// Seed at subscribe from the live location, exactly like the old readable stub: each test's
		// history.replaceState before render is what the mounted component sees, and no test
		// inherits a previous one's param (the afterEach resets the location).
		set({ url: new URL(window.location.href) });
		return () => {};
	});
	return {
		page,
		setPageUrl: (path: string) => page.set({ url: new URL('http://localhost' + path) }),
	};
});
vi.mock('$app/stores', () => stubPage);
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

import { pasteKey, dmRequests as fetchDmRequests } from '$lib/api.js';
const pasteKeyMock = pasteKey as unknown as ReturnType<typeof vi.fn>;
const dmRequestsMock = fetchDmRequests as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	toastMessage.set(null);
	window.history.replaceState({}, '', '/chat');
});

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

/** identity + an empty contact list, so `?peer=` takes the non-contact branch. */
function mountAsStranger() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
	contacts.set([]);
}

/** A full DmRequestView for the Request-bucket store (svelte-check demands every field). */
function bucketRow(npub: string = STRANGER) {
	return { npub, first_seen: 0, last_message_at: 0, message_count: 0, messages: [] };
}

describe('QURATOR-156 — non-contact deep-link intent + in-place Retry', () => {
	it('applies `&intent=ask-access` to the composer on the NON-contact branch (same helper as the contact path)', async () => {
		mountAsStranger();
		pasteKeyMock.mockResolvedValue(strangerResolve());
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER + '&intent=ask-access');

		const { container, findByText } = render(ChatPage);

		// The pane opened for the stranger…
		expect(await findByText('Roster Stranger')).toBeTruthy();
		// …and the composer carries the pre-filled ask-access draft (applyAskAccessIntent's copy,
		// no petname param → the bare "Hi," form), NOT an empty textarea.
		await waitFor(() => {
			const textarea = container.querySelector('textarea.compose-input') as HTMLTextAreaElement;
			expect(textarea?.value).toBe('Hi, could I have your share code? I\'d like to browse your collections.');
		});
	});

	it('a rejected non-contact resolve shows an in-place Retry that re-enters pasteKey and opens the pane', async () => {
		mountAsStranger();
		pasteKeyMock
			.mockRejectedValueOnce(new Error('relay unreachable'))
			.mockResolvedValueOnce(strangerResolve());
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER);

		const { container, findByRole, findByText, queryByText } = render(ChatPage);

		// The shared EmptyState error surface (QURATOR-93's Retry affordance) appears in the empty
		// pane after the failure — no navigate-away-and-back required.
		const retry = await findByRole('button', { name: 'Retry' });
		expect(pasteKeyMock).toHaveBeenCalledTimes(1);

		// Press it: the resolve re-runs (the guard-set entry was cleared on settle) and the pane
		// opens with the resolved display name.
		await fireEvent.click(retry);
		expect(await findByText('Roster Stranger')).toBeTruthy();
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledTimes(2));
		// The failed-deeplink message is gone once the retry succeeds.
		await waitFor(() => expect(queryByText(/Couldn.t reach this person/i)).toBeNull());
		expect(container.querySelector('button')).toBeTruthy();
	});
});

describe('QURATOR-155 — fetchingNames means "in flight", not "ever attempted"', () => {
	// DO NOT re-pin "a settled bucket fetch re-fetches on the next bucket poll" — that assertion was
	// WRONG. The bucket loop is driven by `$effect(() => fetchNonContactNames($dmRequests.map(...)))`
	// on a DM_POLL_VISIBLE_MS (3s) poll, so "re-fetch after settle" there means one relay round-trip
	// per profileless request sender EVERY 3 SECONDS, forever — the exact fan-out `settledNames` was
	// added to stop, and the once-per-session cadence is already pinned by chat-q155-poll-cadence.test.ts.
	// What QURATOR-155's ticket actually names is the DEEP-LINK half: the `?peer=` branch must share
	// `fetchingNames` as an IN-FLIGHT guard only, so that a settle (which fills `settledNames`) never
	// suppresses a later deep-link resolve for that same npub.
	it('a settled Request-bucket fetch does not suppress a later deep-link resolve for the same npub', async () => {
		mountAsStranger();
		// PROFILELESS bucket resolve: nothing lands in peerNameCache, and the settle puts STRANGER
		// into settledNames — the exact state a leaked-set defect needs to be visible.
		pasteKeyMock.mockResolvedValue({ npub: STRANGER, local_tags: [], profile: null, collections: [], online: false, last_fetched: '' });
		dmRequestsMock.mockResolvedValue([bucketRow()]);

		// Phase 1 — no deep-link param: the Request-bucket effect (fetchNonContactNames) resolves
		// STRANGER once and settles.
		window.history.replaceState({}, '', '/chat');
		const rendered = render(ChatPage);
		await waitFor(() => expect(pasteKeyMock).toHaveBeenCalledWith(STRANGER));
		// Let the settle land (settledNames.add) before the deep-link arrives.
		await new Promise((r) => setTimeout(r, 50));
		expect(pasteKeyMock.mock.calls.filter((c) => c[0] === STRANGER).length).toBe(1);

		// Phase 2 — same mount, now the deep-link arrives (a roster click navigating to /chat?peer=).
		// peerDeepLinked is still '' (no link was handled yet), so the effect re-runs on the URL
		// change; the deep-link path consults fetchingNames ONLY, never settledNames, so the resolve
		// MUST fire again and open the pane even though STRANGER's bucket fetch has settled. If
		// settledNames ever leaks into that guard (the QURATOR-155 defect this pins), the second
		// resolve never happens and the pane stays on the empty state.
		setPageUrl('/chat?peer=' + STRANGER);
		await waitFor(
			() => expect(pasteKeyMock.mock.calls.filter((c) => c[0] === STRANGER).length).toBe(2),
			{ timeout: 3000 },
		);
		// The conversation pane actually OPENED for the deep-linked stranger — not just that
		// pasteKey ran. The non-contact banner renders only inside the open peer pane.
		await waitFor(() => {
			expect(rendered.getByText(/may not have added you back/i)).toBeTruthy();
		});
	});

	it('two concurrent triggers for the same npub still de-duplicate to ONE fetch', async () => {
		mountAsStranger();
		// A pasteKey that stays pending until we release it — both callers are genuinely in flight
		// at once. NOTE: no profile → nothing cached, so the second trigger is not absorbed by the
		// peerNameCache check, only by the in-flight guard.
		let release!: (v: unknown) => void;
		const gate = new Promise((r) => { release = r; });
		pasteKeyMock.mockImplementation(() => gate.then(() => ({ npub: STRANGER, local_tags: [], profile: null, collections: [], online: false, last_fetched: '' })));

		// Two same-npub triggers race: the deep-link branch AND the Request-bucket pass.
		window.history.replaceState({}, '', '/chat?peer=' + STRANGER);
		dmRequestsMock.mockResolvedValue([bucketRow()]);
		render(ChatPage);

		// Let both callers start (the effect + the mount-driven bucket pass), then settle the gate.
		await waitFor(() => expect(pasteKeyMock.mock.calls.length).toBeGreaterThanOrEqual(1));
		await new Promise((r) => setTimeout(r, 50));
		release(undefined);
		await new Promise((r) => setTimeout(r, 50));

		// The de-dup the set exists for still holds: exactly ONE in-flight request.
		const callsForStranger = pasteKeyMock.mock.calls.filter((c) => c[0] === STRANGER);
		expect(callsForStranger.length).toBe(1);
	});
});
