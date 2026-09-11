// @vitest-environment jsdom
// QURATOR-188 — Browse never applied pending browse-key grants: `applyKeyGrants()` had exactly
// ONE caller in the whole UI (Contacts' `loadKeyGrants`, contacts/+page.svelte), so a
// granted-but-keyless contact kept rendering "🔒 Listings locked" until the user happened to
// visit Contacts. Fix: Browse's onMount now fires its own `loadKeyGrants()` twin and, when the
// call reports at least one applied npub, re-reads contacts via `loadContactsInto(getContacts)`.
//
// This is a BEHAVIOURAL mount test (the q134 pattern): Browse is mounted with only `$lib/api.js`
// mocked, everything else real — the Svelte 5 runes, the deep-link $effect, the shared stores.
//
// Test 2's interleaving is the fresh-session shape the ticket names ("go straight to Browse in a
// fresh session"): the store starts EMPTY, the `/browse?peer=` deep-link effect waits on
// $contacts (peerFromQuery returns null until the match exists), the mount-time grant lands
// FIRST, and the re-read is what puts the KEYED row in the store — so selection happens keyed
// and the collections grid renders instead of the lock. (A peer already selected keyless keeps
// its `selectedPeer` snapshot until re-selected — `selectedPeer` is local $state, not
// store-derived; that staleness is out of scope here and noted in the task report.)
//
// Per CLAUDE.md §9, a green test proves nothing until seen red. The pre-fix RED run and the two
// post-fix mutation probes (delete the onMount call; make the re-read unconditional) are
// documented in the task report.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import BrowsePage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { CachedPeer, Collection } from '$lib/types.js';

// Every VALUE export Browse's `$lib/api.js` import names (line 7) — a missing one is `undefined`
// at the call site and the mount throws. q134's mock is shorter because this page's line-7 list
// grew after it was written; this factory matches the current import exactly, plus the two
// QURATOR-188 additions (applyKeyGrants, getContacts — getContacts was already imported for
// retryContactsLoad but never called at mount until now).
vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn(),
	importManifest: vi.fn(),
	requestManifest: vi.fn(),
	requestManifestFrom: vi.fn(),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn(),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn(),
	groupsCreateWithMembers: vi.fn(),
	groupsAssign: vi.fn(),
	groupsDelete: vi.fn(),
	groupsUnassign: vi.fn(),
	contactUpdateGroups: vi.fn(),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	applyKeyGrants: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

// $page stub: the `/browse?peer=<npub>` deep-link selects the peer through selectPeer exactly as
// production does (the q92/q102/q134 pattern). The npub is inlined because vi.mock is hoisted
// above every const.
const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/browse?peer=npub1q188q188q188q188q188q188q188q188q188q188q188q188q188') }) };
});
vi.mock('$app/stores', () => stubPage);

// bech32-safe fixture id (same shape as q134's — peerFromQuery matches on the full string); must
// equal the npub in the stubbed $page URL.
const PEER_NPUB = 'npub1q188q188q188q188q188q188q188q188q188q188q188q188q188';

/** A granted collection — the row only the post-grant re-read returns. */
function grantedCollection(): Collection {
	return {
		slug: 'granted-pack',
		path_alias: 'granted-pack',
		item_count: 2,
		total_bytes: 2048,
		content_types: [],
		tags: [],
		languages: [],
		last_updated: '2026-09-01T00:00:00Z',
		listing: [{ name: 'granted-clip.mkv', item_type: 'File', size: '2KB', tags: [], children: [] }],
	} as Collection;
}

/** The PRE-grant row — keyless with sealed listings, the exact state that renders 🔒 (q134 state 2). */
function keylessSealedPeer(): CachedPeer {
	return {
		npub: PEER_NPUB,
		// deliberately NO browse_key_hex — keyless, the owner's scenario
		collections: [],
		online: false,
		last_fetched: '2026-09-01T00:00:00Z',
		local_tags: [],
		listings_state: 'Sealed',
		profile: { display_name: 'Granted Peer', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '2026-09-01T00:00:00Z' },
	} as CachedPeer & { listings_state: 'Sealed' };
}

/** The POST-grant row — same peer, same Sealed listings on the relay, but the key arrived. The
 *  lock derivation needs BOTH keyless AND Sealed, so the key alone clears it. */
function grantedPeer(): CachedPeer {
	return {
		...keylessSealedPeer(),
		browse_key_hex: 'a1b2c3grantkeyarrivedf0f0',
		collections: [grantedCollection()],
	} as CachedPeer & { listings_state: 'Sealed' };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

import { applyKeyGrants, getContacts, refreshContact } from '$lib/api.js';
const grantsMock = applyKeyGrants as unknown as ReturnType<typeof vi.fn>;
const getContactsMock = getContacts as unknown as ReturnType<typeof vi.fn>;
const refreshMock = refreshContact as unknown as ReturnType<typeof vi.fn>;

describe('QURATOR-188 — Browse applies pending key grants on mount', () => {
	it('mounting Browse calls applyKeyGrants exactly once (Contacts is no longer the only caller)', async () => {
		grantsMock.mockResolvedValue([]);
		contacts.set([keylessSealedPeer()]);
		render(BrowsePage);
		await tick();

		await waitFor(() => expect(grantsMock).toHaveBeenCalledTimes(1));
		// Exactly once — not a poll hook (mirrors the Contacts receive-side ruling).
		await new Promise((r) => setTimeout(r, 10));
		expect(grantsMock).toHaveBeenCalledTimes(1);
	});

	it('a reported npub re-reads contacts, and the granted peer renders their collection — not 🔒', async () => {
		// Fresh session: the store starts EMPTY. The deep-link effect waits on $contacts; the
		// mount-time grant + re-read is what puts the KEYED row there before selection happens.
		grantsMock.mockResolvedValue([PEER_NPUB]);
		getContactsMock.mockResolvedValue([grantedPeer()]);
		refreshMock.mockResolvedValue(grantedPeer());
		render(BrowsePage);
		await tick();

		// The grant applied → the contacts store was re-read through getContacts.
		await waitFor(() => expect(getContactsMock).toHaveBeenCalledTimes(1));
		// …and the keyed row surfaced by that re-read renders the granted collection: the grid
		// shows the collection card, never the lock (which needs keyless AND Sealed).
		await waitFor(() => expect(document.body.textContent).toContain('granted-pack'));
		expect(document.body.textContent).not.toContain('Listings locked');
		expect(grantsMock).toHaveBeenCalledTimes(1);
	});

	it('NO applied npub ⇒ no contacts re-read (an unconditional per-mount fetch is the defect this repo rejects)', async () => {
		grantsMock.mockResolvedValue([]);
		contacts.set([keylessSealedPeer()]);
		render(BrowsePage);
		await tick();

		// Let the applyKeyGrants promise resolve and the empty-array check run its course.
		await waitFor(() => expect(grantsMock).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 10));
		expect(getContactsMock).not.toHaveBeenCalled();
	});

	it('a rejected applyKeyGrants is non-fatal — the page still renders, no unhandled rejection', async () => {
		grantsMock.mockRejectedValue(new Error('relays unreachable'));
		// Keyless + Fetched (nothing published) — q134 state 1: renders "No public collections".
		contacts.set([{ ...keylessSealedPeer(), listings_state: 'Fetched' } as CachedPeer]);
		render(BrowsePage);
		await tick();

		await waitFor(() => expect(grantsMock).toHaveBeenCalledTimes(1));
		// The mount survived the rejection and rendered a real branch (an unhandled rejection
		// would have failed the run before this line).
		await waitFor(() => expect(document.body.textContent).toContain('No public collections'));
		expect(getContactsMock).not.toHaveBeenCalled();
	});
});
